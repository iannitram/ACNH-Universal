// SPDX-License-Identifier: MIT
// Copyright (c) 2026 iannitram
// Adapted from iannitram/totk-universal at 80148e16e60b5ca5d34ca87d3ef3360567490352:
// crates/core/src/hub.rs (discovery/transport: 27-285, pairing: 287-544,
// authenticated resumable transfer: 546-1136). See NOTICE for the MIT notice.
//! The build hub on the home network (SPEC.md M11; docs/adr/0004-build-hubs.md): find a hub,
//! pair with it once, and transfer player bundles or prepared Android data. The `hub` feature
//! enables both protocols. TCP checks that addresses are on the home network.

use crate::bundle::{self, Bundle, BundleFile};
use crate::error::{Error, Result};
use crate::platform::{AppDirs, Os, Vars};
use chacha20poly1305::{
    aead::{AeadInOut, Generate},
    ChaCha20Poly1305, Key, KeyInit, Nonce,
};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use std::collections::{BTreeSet, VecDeque};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The mDNS service a hub announces.
pub const SERVICE: &str = "_acnh-universal._tcp.local.";

/// A hub found on the home network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// `recipe::Hub::key()`.
    pub hub: String,
    pub addr: IpAddr,
    pub port: u16,
    pub targets: Vec<Os>,
}

/// A device paired with this one, as kept in `<config>/acnh-universal/peers.json`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Peer {
    pub device: String,
    /// Hex of the key both sides derived at pairing.
    pub key_hex: String,
}

impl fmt::Debug for Peer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Peer")
            .field("device", &self.device)
            .field("key_hex", &"[redacted]")
            .finish()
    }
}

/// A byte stream to a peer: TCP on the home network, or an in-memory pair in tests.
pub trait Transport: std::io::Read + std::io::Write {}

/// Longest a single read or write may stall before the peer counts as gone. Five minutes,
/// not less: the receiving side hashes a whole build after its last chunk, and the sending
/// side waits through that, so a shorter limit would fail large builds on slow disks.
pub const IO_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// TCP byte stream whose peer is checked before connecting or accepting it for use.
/// Every read and write has `IO_TIMEOUT`, so a silent peer cannot hang the other end.
#[derive(Debug)]
pub struct TcpTransport(TcpStream);

impl TcpTransport {
    pub fn connect(addr: SocketAddr) -> Result<Self> {
        connect_home(addr, |addr| {
            let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(10))?;
            Self::configure(&stream)?;
            Ok(Self(stream))
        })
    }

    /// Wrap an accepted connection only if its peer is on the home network.
    pub fn from_stream(stream: TcpStream) -> Result<Self> {
        check_home(stream.peer_addr()?.ip())?;
        Self::configure(&stream)?;
        Ok(Self(stream))
    }

    fn configure(stream: &TcpStream) -> io::Result<()> {
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))
    }
}

fn check_home(addr: IpAddr) -> Result<()> {
    if is_home_network(addr) {
        Ok(())
    } else {
        Err(Error::Hub(format!(
            "peer address {addr} is outside the home network"
        )))
    }
}

fn connect_home<T>(
    addr: SocketAddr,
    connect: impl FnOnce(SocketAddr) -> io::Result<T>,
) -> Result<T> {
    check_home(addr.ip())?;
    Ok(connect(addr)?)
}

impl Read for TcpTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for TcpTransport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Transport for TcpTransport {}

/// One end of a blocking, full-duplex in-memory byte stream for hub protocol tests.
#[derive(Debug)]
pub struct MemoryTransport {
    sender: Sender<Vec<u8>>,
    receiver: Receiver<Vec<u8>>,
    pending: VecDeque<u8>,
}

impl MemoryTransport {
    pub fn pair() -> (Self, Self) {
        let (a_tx, a_rx) = mpsc::channel();
        let (b_tx, b_rx) = mpsc::channel();
        (
            Self {
                sender: a_tx,
                receiver: b_rx,
                pending: VecDeque::new(),
            },
            Self {
                sender: b_tx,
                receiver: a_rx,
                pending: VecDeque::new(),
            },
        )
    }
}

impl Read for MemoryTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.pending.is_empty() {
            match self.receiver.recv() {
                Ok(bytes) => self.pending.extend(bytes),
                Err(_) => return Ok(0),
            }
        }
        self.pending.read(buf)
    }
}

impl Write for MemoryTransport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !buf.is_empty() {
            self.sender
                .send(buf.to_vec())
                .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Transport for MemoryTransport {}

/// True for loopback, 10/8, 172.16/12, 192.168/16, 169.254/16, IPv6 loopback, fc00::/7 and
/// fe80::/10; false for anything else. Every connection checks it first.
pub fn is_home_network(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(addr) => {
            let octets = addr.octets();
            addr.is_loopback()
                || octets[0] == 10
                || (octets[0] == 172 && (16..=31).contains(&octets[1]))
                || (octets[0] == 192 && octets[1] == 168)
                || (octets[0] == 169 && octets[1] == 254)
        }
        IpAddr::V6(addr) => {
            let octets = addr.octets();
            addr.is_loopback()
                || octets[0] & 0xfe == 0xfc
                || (octets[0] == 0xfe && octets[1] & 0xc0 == 0x80)
        }
    }
}

/// An active mDNS announcement. Keep this value alive while the hub is listening;
/// dropping it stops the announcement and its daemon.
pub struct Announcement {
    daemon: DiscoveryDaemon,
    fullname: String,
}

impl Drop for Announcement {
    fn drop(&mut self) {
        // Enqueue the goodbye before shutting down the daemon.
        if let Ok(done) = self.daemon.0.unregister(&self.fullname) {
            let _ = done.recv_timeout(Duration::from_secs(2));
        }
    }
}

/// Announce a listening hub on the supplied local interfaces. All advertised addresses must
/// be home-network addresses; pass actual interface addresses, not wildcard bind addresses.
/// `instance` and `hostname` identify this service on the local network. The hostname must
/// end in `.local.` and should be unique to this hub.
pub fn announce(
    instance: &str,
    hostname: &str,
    hub: crate::recipe::Hub,
    addresses: &[IpAddr],
    port: u16,
) -> Result<Announcement> {
    if addresses.is_empty() || port == 0 {
        return Err(Error::Hub(
            "announcement needs an address and listening port".into(),
        ));
    }
    for address in addresses {
        check_home(*address)?;
    }
    if instance.is_empty() || !hostname.ends_with(".local.") {
        return Err(Error::Hub(
            "announcement needs an instance and .local. hostname".into(),
        ));
    }
    let targets = crate::recipe::serves(hub)
        .iter()
        .map(|os| os.key())
        .collect::<Vec<_>>()
        .join(",");
    let properties = [("hub", hub.key()), ("targets", targets.as_str())];
    let info = mdns_sd::ServiceInfo::new(
        SERVICE,
        instance,
        hostname,
        addresses,
        port,
        properties.as_slice(),
    )
    .map_err(mdns_error)?;
    let fullname = info.get_fullname().to_owned();
    let daemon = DiscoveryDaemon(mdns_sd::ServiceDaemon::new().map_err(mdns_error)?);
    daemon
        .0
        .disable_interface(mdns_sd::IfKind::All)
        .map_err(mdns_error)?;
    for address in addresses {
        daemon.0.enable_interface(*address).map_err(mdns_error)?;
    }
    daemon.0.register(info).map_err(mdns_error)?;
    Ok(Announcement { daemon, fullname })
}

/// Hubs announcing `SERVICE`, waited for up to `wait_ms`.
pub fn discover(wait_ms: u64) -> Result<Vec<Found>> {
    if wait_ms == 0 {
        return Ok(Vec::new());
    }
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(wait_ms))
        .ok_or_else(|| Error::Hub("discovery wait is too long".into()))?;
    let daemon = DiscoveryDaemon(mdns_sd::ServiceDaemon::new().map_err(mdns_error)?);
    let events = daemon.0.browse(SERVICE).map_err(mdns_error)?;
    let mut found = Vec::new();
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match events.recv_timeout(remaining) {
            Ok(mdns_sd::ServiceEvent::ServiceResolved(service)) => {
                for hub in resolved_hubs(&service) {
                    if !found.contains(&hub) {
                        found.push(hub);
                    }
                }
            }
            Ok(_) => {}
            Err(_) if events.is_disconnected() => {
                return Err(Error::Hub("discovery daemon disconnected".into()));
            }
            Err(_) => break,
        }
    }
    found.sort_by(|a, b| (&a.hub, a.addr, a.port).cmp(&(&b.hub, b.addr, b.port)));
    Ok(found)
}

fn mdns_error(error: mdns_sd::Error) -> Error {
    Error::Hub(format!("mDNS: {error}"))
}

// Ensure every exit path stops the daemon, including a failed browse.
struct DiscoveryDaemon(mdns_sd::ServiceDaemon);

impl Drop for DiscoveryDaemon {
    fn drop(&mut self) {
        let _ = self.0.shutdown();
    }
}

// TXT schema: hub=<recipe::Hub::key()>, targets=<comma-separated Os::key() values>.
fn resolved_hubs(service: &mdns_sd::ResolvedService) -> Vec<Found> {
    let port = service.get_port();
    if port == 0 {
        return Vec::new();
    }
    let Some(hub) = service
        .get_property_val_str("hub")
        .filter(|key| !key.is_empty())
    else {
        return Vec::new();
    };
    let Some(targets) = service.get_property_val_str("targets") else {
        return Vec::new();
    };
    let targets: Option<Vec<_>> = targets
        .split(',')
        .map(|target| {
            [Os::Linux, Os::Windows, Os::Mac, Os::Ios, Os::Android]
                .into_iter()
                .find(|os| os.key() == target)
        })
        .collect();
    let Some(targets) = targets else {
        return Vec::new();
    };
    service
        .get_addresses()
        .iter()
        .map(|addr| addr.to_ip_addr())
        .filter(|addr| is_home_network(*addr))
        .map(|addr| Found {
            hub: hub.into(),
            addr,
            port,
            targets: targets.clone(),
        })
        .collect()
}

/// Wrong codes that end a pairing: the code is then spent, and pairing starts again with a new one.
pub const MAX_FAILED_ATTEMPTS: u32 = 3;
/// How long a pairing code stays valid after it is made.
pub const CODE_LIFETIME: Duration = Duration::from_secs(5 * 60);

const MISMATCH: &str = "the pairing code did not match";
const SPENT: &str = "this pairing code is used up: start pairing again with a new code";
const EXPIRED: &str = "this pairing code has expired: start pairing again with a new code";

/// The time source a pairing session reads. Tests inject their own; production uses `SystemClock`.
pub trait Clock: Send {
    fn now(&self) -> Instant;
}

/// The real clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// One six-digit code and the attempts made with it. The code is single-session: a correct
/// pairing or `MAX_FAILED_ATTEMPTS` wrong codes spend it, and it expires `CODE_LIFETIME` after
/// it is made. Nothing here is saved, because a restart always makes a new code.
pub struct PairingSession<C: Clock = SystemClock> {
    code: Vec<u8>,
    made: Instant,
    clock: C,
    failures: u32,
    spent: bool,
}

impl PairingSession<SystemClock> {
    /// A session for `code`, timed by the system clock.
    pub fn new(code: &str) -> Result<Self> {
        Self::with_clock(code, SystemClock)
    }
}

impl<C: Clock> PairingSession<C> {
    /// A session for `code`, timed by `clock`. The code's lifetime starts now.
    pub fn with_clock(code: &str, clock: C) -> Result<Self> {
        if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
            return Err(pair_mismatch());
        }
        let made = clock.now();
        Ok(Self {
            code: code.as_bytes().to_vec(),
            made,
            clock,
            failures: 0,
            spent: false,
        })
    }

    fn expired(&self) -> bool {
        self.clock.now().saturating_duration_since(self.made) >= CODE_LIFETIME
    }

    /// Refuses an attempt before any bytes move when the code is spent or expired.
    fn check_open(&self) -> Result<()> {
        if self.spent {
            return Err(Error::Hub(SPENT.into()));
        }
        if self.expired() {
            return Err(Error::Hub(EXPIRED.into()));
        }
        Ok(())
    }

    /// Counts a finished attempt: success spends the code, and every failure adds one. A dropped
    /// connection, a timeout, a malformed message and a bad confirmation all count, because a
    /// peer that guessed wrong could otherwise learn it from our confirmation and hang up unseen.
    fn record<T>(&mut self, result: &Result<T>) {
        if result.is_ok() {
            self.spent = true;
            return;
        }
        self.failures += 1;
        if self.failures >= MAX_FAILED_ATTEMPTS {
            self.spent = true;
        }
    }
}

/// Pairs over `t` with the session's six-digit code shown on both screens; both sides derive the
/// same key, or it is `Error::Hub("the pairing code did not match")`.
pub fn pair<C: Clock>(
    t: &mut dyn Transport,
    session: &mut PairingSession<C>,
    device: &str,
) -> Result<Peer> {
    let vars = Vars::from_lookup(Os::current(), |name| std::env::var(name).ok())?;
    pair_in(t, session, device, &AppDirs::new(&vars))
}

/// Pair and remember a peer in the supplied application folders (also for mobile callers).
/// `device` is the local label for the remote device. No peer file is touched until confirmation.
pub fn pair_in<C: Clock>(
    t: &mut dyn Transport,
    session: &mut PairingSession<C>,
    device: &str,
    dirs: &AppDirs,
) -> Result<Peer> {
    pair_with_context(t, session, device, dirs, b"acnh-universal/hub/pair/v1")
}

fn pair_with_context<C: Clock>(
    t: &mut dyn Transport,
    session: &mut PairingSession<C>,
    device: &str,
    dirs: &AppDirs,
    context: &[u8],
) -> Result<Peer> {
    session.check_open()?;
    let result = exchange(t, session, device, dirs, context);
    session.record(&result);
    result
}

fn exchange<C: Clock>(
    t: &mut dyn Transport,
    session: &PairingSession<C>,
    device: &str,
    dirs: &AppDirs,
    context: &[u8],
) -> Result<Peer> {
    let (state, outbound) = Spake2::<Ed25519Group>::start_symmetric(
        &Password::new(&session.code[..]),
        &Identity::new(context),
    );
    t.write_all(&outbound)?;
    t.flush()?;
    let mut inbound = [0; 33];
    t.read_exact(&mut inbound)?;
    // Ordering the ephemeral public messages assigns distinct directions without a client role.
    let (send_label, receive_label) = match outbound.as_slice().cmp(&inbound) {
        std::cmp::Ordering::Less => (b"pair/v1/low-to-high", b"pair/v1/high-to-low"),
        std::cmp::Ordering::Greater => (b"pair/v1/high-to-low", b"pair/v1/low-to-high"),
        std::cmp::Ordering::Equal => return Err(pair_mismatch()),
    };
    let key = state.finish(&inbound).map_err(|_| pair_mismatch())?;
    if key.len() != 32 {
        return Err(pair_mismatch());
    }
    let confirmation = |label: &[u8]| {
        let mut mac =
            <Hmac<Sha256> as Mac>::new_from_slice(&key).expect("HMAC accepts any key length");
        mac.update(label);
        mac
    };
    t.write_all(&confirmation(send_label).finalize().into_bytes())?;
    t.flush()?;
    let mut received = [0; 32];
    t.read_exact(&mut received)?;
    let confirmed = confirmation(receive_label).verify_slice(&received).is_ok();
    // Acknowledge verification too: a damaged MAC in one direction must not leave
    // only one side remembering the peer. Failed confirmation sends no valid acknowledgement.
    let acknowledgement = |label: &[u8]| {
        let mut mac = confirmation(label);
        mac.update(b"/accepted");
        mac
    };
    let accepted: [u8; 32] = if confirmed {
        acknowledgement(send_label).finalize().into_bytes().into()
    } else {
        [0; 32]
    };
    t.write_all(&accepted)?;
    t.flush()?;
    t.read_exact(&mut received)?;
    let acknowledged = acknowledgement(receive_label)
        .verify_slice(&received)
        .is_ok();
    if !confirmed || !acknowledged {
        return Err(pair_mismatch());
    }
    // A code that ran out while the exchange was in flight remembers nobody.
    if session.expired() {
        return Err(Error::Hub(EXPIRED.into()));
    }
    let peer = Peer {
        device: device.into(),
        key_hex: key.iter().map(|b| format!("{b:02x}")).collect(),
    };
    let _guard = PEERS_LOCK
        .lock()
        .map_err(|_| Error::Hub("peers file is busy".into()))?;
    let mut peers = read_peers(dirs)?;
    peers.retain(|old| old.device != device);
    peers.push(peer.clone());
    write_peers(dirs, &peers)?;
    Ok(peer)
}

fn pair_mismatch() -> Error {
    Error::Hub(MISMATCH.into())
}

static PEERS_LOCK: Mutex<()> = Mutex::new(());

/// Read remembered peers. A missing file means no peers; errors never include its contents.
pub fn read_peers(dirs: &AppDirs) -> Result<Vec<Peer>> {
    let bytes = match fs::read(dirs.config.join("peers.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let peers: Vec<Peer> =
        serde_json::from_slice(&bytes).map_err(|_| Error::Hub("invalid peers file".into()))?;
    if peers.iter().any(|peer| {
        peer.key_hex.len() != 64
            || !peer
                .key_hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }) {
        return Err(Error::Hub("invalid peers file".into()));
    }
    Ok(peers)
}

/// Forget a device, returning whether it was remembered.
pub fn remove_peer(dirs: &AppDirs, device: &str) -> Result<bool> {
    let _guard = PEERS_LOCK
        .lock()
        .map_err(|_| Error::Hub("peers file is busy".into()))?;
    let mut peers = read_peers(dirs)?;
    let before = peers.len();
    peers.retain(|peer| peer.device != device);
    if before == peers.len() {
        return Ok(false);
    }
    write_peers(dirs, &peers)?;
    Ok(true)
}

fn write_peers(dirs: &AppDirs, peers: &[Peer]) -> Result<()> {
    let bytes =
        serde_json::to_vec(peers).map_err(|_| Error::Hub("could not encode peers file".into()))?;
    fs::create_dir_all(&dirs.config)?;
    crate::platform::atomic_write(&dirs.config.join("peers.json"), &bytes)
}

/// Plaintext bytes per chunk (the final chunk may be shorter).
pub const CHUNK_SIZE: usize = 64 * 1024;
const MAX_MANIFEST: usize = 1024 * 1024;
const CACHE: &str = ".hub-transfer";
const WIRE: &[u8; 8] = b"ACNHHUB1";

fn hub_error(message: &str) -> Error {
    Error::Hub(message.into())
}

// Transport and filesystem errors must stay plain, without paths or protocol/key material.
fn transfer_error(error: Error) -> Error {
    match error {
        Error::Hub(_) => error,
        _ => hub_error("transfer interrupted or a build file is unavailable"),
    }
}

fn pairing_key(peer: &Peer) -> Result<[u8; 32]> {
    let mut key = [0; 32];
    if peer.key_hex.len() != 64 {
        return Err(hub_error("this device is not paired"));
    }
    for (out, hex) in key
        .iter_mut()
        .zip(peer.key_hex.as_bytes().as_chunks::<2>().0)
    {
        let digit = |b| match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            _ => Err(hub_error("this device is not paired")),
        };
        *out = digit(hex[0])? * 16 + digit(hex[1])?;
    }
    Ok(key)
}

fn keyed(key: &[u8], label: &[u8], data: &[u8]) -> Hmac<Sha256> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(b"acnh-universal/hub/transfer/v1/");
    mac.update(label);
    mac.update(data);
    mac
}

fn derive(key: &[u8], label: &[u8], data: &[u8]) -> [u8; 32] {
    keyed(key, label, data).finalize().into_bytes().into()
}

fn random_salt() -> Result<[u8; 32]> {
    Key::try_generate()
        .map(Into::into)
        .map_err(|_| hub_error("could not start a fresh transfer"))
}

// No Debug implementation: the cipher, counters and keys are private protocol state.
struct Channel {
    send: ChaCha20Poly1305,
    receive: ChaCha20Poly1305,
    sent: u64,
    received: u64,
}

impl Channel {
    fn new(key: &[u8], transcript: &[u8], server: bool) -> Self {
        let outbound = derive(key, b"server-key", transcript);
        let inbound = derive(key, b"client-key", transcript);
        let (outbound, inbound) = if server {
            (outbound, inbound)
        } else {
            (inbound, outbound)
        };
        Self {
            send: ChaCha20Poly1305::new(&outbound.into()),
            receive: ChaCha20Poly1305::new(&inbound.into()),
            sent: 0,
            received: 0,
        }
    }

    // Each directional key is fresh for this connection. A monotonically increasing
    // 64-bit sequence, prefixed by four zero bytes, is the 96-bit nonce. Never wrap.
    fn nonce(sequence: &mut u64) -> Result<Nonce> {
        let mut bytes = [0; 12];
        bytes[4..].copy_from_slice(&sequence.to_be_bytes());
        *sequence = sequence
            .checked_add(1)
            .ok_or_else(|| hub_error("transfer too long"))?;
        Ok(bytes.into())
    }

    fn send(&mut self, t: &mut dyn Transport, aad: &[u8], mut bytes: Vec<u8>) -> Result<()> {
        self.send
            .encrypt_in_place(&Self::nonce(&mut self.sent)?, aad, &mut bytes)
            .map_err(|_| hub_error("could not seal a transfer chunk"))?;
        t.write_all(&(bytes.len() as u32).to_be_bytes())?;
        t.write_all(&bytes)?;
        t.flush()?;
        Ok(())
    }

    fn receive(&mut self, t: &mut dyn Transport, aad: &[u8], max: usize) -> Result<Vec<u8>> {
        let mut length = [0; 4];
        t.read_exact(&mut length)?;
        let length = u32::from_be_bytes(length) as usize;
        if length < 16 || length > max + 16 {
            return Err(hub_error("invalid transfer frame"));
        }
        let mut bytes = vec![0; length];
        t.read_exact(&mut bytes)?;
        self.receive
            .decrypt_in_place(&Self::nonce(&mut self.received)?, aad, &mut bytes)
            .map_err(|_| hub_error("a transfer chunk failed authentication"))?;
        Ok(bytes)
    }
}

// Both fresh 256-bit salts and the target are authenticated with distinct role labels.
// An unpaired connection sees only a version and random challenge, never a bundle.
fn handshake_for(
    t: &mut dyn Transport,
    key: &[u8],
    target: Os,
    server: bool,
    wire: &[u8; 8],
) -> Result<Channel> {
    let mut server_salt = [0; 32];
    let mut client_salt = [0; 32];
    if server {
        server_salt = random_salt()?;
        t.write_all(wire)?;
        t.write_all(&server_salt)?;
        t.flush()?;
        t.read_exact(&mut client_salt)?;
    } else {
        let mut version = [0; 8];
        t.read_exact(&mut version)?;
        if &version != wire {
            return Err(hub_error("unsupported hub transfer protocol"));
        }
        t.read_exact(&mut server_salt)?;
        client_salt = random_salt()?;
        t.write_all(&client_salt)?;
    }
    let mut transcript = Vec::from(wire.as_slice());
    transcript.extend_from_slice(&server_salt);
    transcript.extend_from_slice(&client_salt);
    transcript.extend_from_slice(target.key().as_bytes());
    let send_label: &[u8] = if server {
        b"server-proof"
    } else {
        b"client-proof"
    };
    let receive_label: &[u8] = if server {
        b"client-proof"
    } else {
        b"server-proof"
    };
    let proof = derive(key, send_label, &transcript);
    let mut received = [0; 32];
    if server {
        t.read_exact(&mut received)?;
    } else {
        t.write_all(&proof)?;
        t.flush()?;
        t.read_exact(&mut received)?;
    }
    keyed(key, receive_label, &transcript)
        .verify_slice(&received)
        .map_err(|_| hub_error("this device is not paired for this target"))?;
    if server {
        t.write_all(&proof)?;
        t.flush()?;
    }
    Ok(Channel::new(key, &transcript, server))
}

fn handshake(t: &mut dyn Transport, key: &[u8], target: Os, server: bool) -> Result<Channel> {
    handshake_for(t, key, target, server, WIRE)
}

mod prepared;
mod service;
pub use prepared::{pull_prepared, pull_prepared_app, pull_prepared_app_with, serve_prepared};
pub use service::{
    pair_hub, pull_hub, pull_hub_prepared, pull_hub_prepared_app, pull_hub_prepared_app_with,
    read_fleet, report_hub, report_hub_build, serve_request,
};

// Reject traversal, Windows aliases, duplicate names, file/directory collisions and our
// private staging namespace before opening any listed file, on both ends.
fn validate_bundle(text: &str, target: Os) -> Result<Bundle> {
    let b = bundle::from_text(text).map_err(|_| hub_error("invalid or forbidden bundle"))?;
    if b.target != target || b.files.len() > u32::MAX as usize {
        return Err(hub_error("bundle target does not match"));
    }
    let mut names = BTreeSet::new();
    for file in &b.files {
        let lower = file.path.to_ascii_lowercase();
        if bundle::is_forbidden(&lower)
            || lower.split('/').next() == Some(CACHE)
            || file.path.split('/').any(|part| {
                let stem = part.split('.').next().unwrap_or("").to_ascii_lowercase();
                part.is_empty()
                    || part == "."
                    || part == ".."
                    || part.ends_with(['.', ' '])
                    || part
                        .chars()
                        .any(|c| c.is_control() || "\\:<>\"|?*".contains(c))
                    || ["con", "prn", "aux", "nul"].contains(&stem.as_str())
                    || (stem.len() == 4
                        && (stem.starts_with("com") || stem.starts_with("lpt"))
                        && matches!(stem.as_bytes()[3], b'1'..=b'9'))
            })
            || !names.insert(lower)
        {
            return Err(hub_error("unsafe or forbidden bundle path"));
        }
    }
    for name in &names {
        for (at, _) in name.match_indices('/') {
            if names.contains(&name[..at]) {
                return Err(hub_error("overlapping bundle paths"));
            }
        }
    }
    Ok(b)
}

fn require_dir(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(hub_error("a build folder is not a plain directory"));
    }
    Ok(())
}

fn make_dir(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => require_dir(path),
        Err(e) => Err(e.into()),
    }
}

fn file_path(root: &Path, relative: &str, create: bool) -> Result<PathBuf> {
    require_dir(root)?;
    let mut path = root.to_path_buf();
    let parts: Vec<_> = relative.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        path.push(part);
        if i + 1 < parts.len() {
            if create {
                make_dir(&path)?;
            } else {
                require_dir(&path)?;
            }
        } else {
            match fs::symlink_metadata(&path) {
                Ok(m) if !m.is_file() || m.file_type().is_symlink() => {
                    return Err(hub_error("a build file is not a plain file"));
                }
                Ok(_) => {}
                Err(e) if create && e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(path)
}

fn read_manifest(path: &Path) -> Result<String> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(hub_error("bundle text is not a plain file"));
    }
    let mut text = String::new();
    File::open(path)?
        .take((MAX_MANIFEST + 1) as u64)
        .read_to_string(&mut text)?;
    if text.len() > MAX_MANIFEST {
        return Err(hub_error("bundle text is too large"));
    }
    Ok(text)
}

// Publishing convention: builds/<build name>/bundle.txt beside its listed files.
// Newest is the bundle.txt modification time; a folder name breaks a timestamp tie.
fn newest(builds: &Path, target: Os) -> Result<(Bundle, PathBuf, String)> {
    require_dir(builds)?;
    let mut candidates = Vec::new();
    for entry in fs::read_dir(builds)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let manifest = entry.path().join("bundle.txt");
        match fs::symlink_metadata(&manifest) {
            Ok(m) if m.is_file() => {
                candidates.push((m.modified()?, entry.file_name(), entry.path()));
            }
            Ok(_) => return Err(hub_error("bundle text is not a plain file")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    candidates.sort();
    for (_, _, root) in candidates.into_iter().rev() {
        let text = read_manifest(&root.join("bundle.txt"))?;
        let b = bundle::from_text(&text).map_err(|_| hub_error("invalid or forbidden bundle"))?;
        if b.target == target {
            return Ok((validate_bundle(&text, target)?, root, text));
        }
    }
    Err(hub_error("no build is ready for this target"))
}

fn chunk_len(file: &BundleFile, index: u64) -> Result<usize> {
    let offset = index
        .checked_mul(CHUNK_SIZE as u64)
        .filter(|offset| *offset < file.size)
        .ok_or_else(|| hub_error("invalid chunk request"))?;
    Ok((file.size - offset).min(CHUNK_SIZE as u64) as usize)
}

fn chunk_aad(digest: &[u8], file: &BundleFile, file_index: u32, index: u64) -> Vec<u8> {
    let mut aad = Vec::from(b"chunk/v1/".as_slice());
    aad.extend_from_slice(digest);
    aad.extend_from_slice(&file_index.to_be_bytes());
    aad.extend_from_slice(&index.to_be_bytes());
    aad.extend_from_slice(&file.size.to_be_bytes());
    aad.extend_from_slice(file.path.as_bytes());
    aad
}

/// Sends the newest bundle for `target` and its files from `builds` to a paired peer.
/// Publish each build as `builds/<build name>/bundle.txt` (`bundle::to_text`), with its
/// files beside it. The newest matching bundle.txt modification time wins, then folder name.
/// `peer` is the locally remembered peer, never a key supplied by the remote connection.
pub fn serve(t: &mut dyn Transport, peer: &Peer, builds: &Path, target: Os) -> Result<Bundle> {
    serve_inner(t, peer, builds, target).map_err(transfer_error)
}

fn serve_inner(t: &mut dyn Transport, peer: &Peer, builds: &Path, target: Os) -> Result<Bundle> {
    let key = pairing_key(peer)?;
    let mut channel = handshake(t, &key, target, true)?;
    let (b, root, text) = newest(builds, target)?;
    // Validate the entire list before touching any listed file or sending the manifest.
    for file in &b.files {
        let path = file_path(&root, &file.path, false)?;
        if fs::metadata(path)?.len() != file.size {
            return Err(hub_error("a build file does not match the bundle"));
        }
    }
    let digest = Sha256::digest(text.as_bytes());
    channel.send(t, b"manifest/v1", text.into_bytes())?;
    let mut last = None;
    loop {
        let request = channel.receive(t, &digest, 13)?;
        if request == [1] {
            channel.send(t, &digest, vec![1])?;
            return Ok(b);
        }
        if request.len() != 13 || request[0] != 0 {
            return Err(hub_error("invalid chunk request"));
        }
        let file_index = u32::from_be_bytes(request[1..5].try_into().unwrap());
        let index = u64::from_be_bytes(request[5..13].try_into().unwrap());
        if last.is_some_and(|previous| previous >= (file_index, index)) {
            return Err(hub_error("repeated or out-of-order chunk request"));
        }
        last = Some((file_index, index));
        let file = b
            .files
            .get(file_index as usize)
            .ok_or_else(|| hub_error("invalid chunk request"))?;
        let mut bytes = vec![0; chunk_len(file, index)?];
        let mut source = File::open(file_path(&root, &file.path, false)?)?;
        source.seek(SeekFrom::Start(index * CHUNK_SIZE as u64))?;
        source.read_exact(&mut bytes)?;
        channel.send(t, &chunk_aad(&digest, file, file_index, index), bytes)?;
    }
}

// create_new + sync + rename (platform::atomic_write_with) means an interrupted write can never look like a finished
// chunk. Temp files, including file assembly, stay under the private staging namespace
// so a process crash cannot leave them in the tree later handed to bundle::install.
fn atomic_write(
    path: &Path,
    temp_dir: &Path,
    write: impl FnOnce(&mut File) -> Result<()>,
) -> Result<()> {
    crate::platform::atomic_write_with(path, temp_dir, write)
}

fn cached_chunk(path: &Path, key: &[u8], aad: &[u8], length: usize) -> Result<Option<Vec<u8>>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(hub_error("a staged chunk is not a plain file"));
    }
    if metadata.len() != (length + 32) as u64 {
        return Ok(None);
    }
    let mut bytes = vec![0; length];
    let mut tag = [0; 32];
    let mut file = File::open(path)?;
    file.read_exact(&mut bytes)?;
    file.read_exact(&mut tag)?;
    let mut receipt = keyed(key, b"finished-chunk", aad);
    receipt.update(&bytes);
    if receipt.verify_slice(&tag).is_err() {
        return Ok(None);
    }
    Ok(Some(bytes))
}

// Same size + SHA-256 checks as bundle::verify, using one bounded buffer. bundle::verify
// currently reads a whole file, so calling it here would break the hub's memory bound.
fn verify_streaming(b: &Bundle, staged: &Path) -> Result<()> {
    for file in &b.files {
        let (size, sha256) =
            crate::fsutil::size_and_sha256(&file_path(staged, &file.path, false)?)?;
        if size != file.size || sha256 != file.sha256 {
            return Err(hub_error("a staged file does not match the bundle"));
        }
    }
    Ok(())
}

/// Pulls a paired hub's newest bundle into `staged`. Authenticated, atomically written
/// chunks under `staged/.hub-transfer` survive errors; only missing or damaged chunks are
/// requested after reconnecting. All paths are checked before creating anything in staged.
/// Checks size and SHA-256 with bounded memory before handing back the Bundle for install.
pub fn pull(t: &mut dyn Transport, peer: &Peer, target: Os, staged: &Path) -> Result<Bundle> {
    pull_inner(t, peer, target, staged).map_err(transfer_error)
}

fn pull_inner(t: &mut dyn Transport, peer: &Peer, target: Os, staged: &Path) -> Result<Bundle> {
    let key = pairing_key(peer)?;
    let mut channel = handshake(t, &key, target, false)?;
    let manifest = channel.receive(t, b"manifest/v1", MAX_MANIFEST)?;
    let text = std::str::from_utf8(&manifest).map_err(|_| hub_error("invalid bundle text"))?;
    let b = validate_bundle(text, target)?;
    let digest = Sha256::digest(&manifest);
    let cache_key = derive(&key, b"staging-key", &digest);
    make_dir(staged)?;
    let cache_root = staged.join(CACHE);
    make_dir(&cache_root)?;
    let cache = cache_root.join(format!("{:x}", digest));
    make_dir(&cache)?;
    for (file_index, file) in b.files.iter().enumerate() {
        let file_index = file_index as u32;
        let destination = file_path(staged, &file.path, true)?;
        atomic_write(&destination, &cache_root, |output| {
            let mut hash = Sha256::new();
            let count = file.size.div_ceil(CHUNK_SIZE as u64);
            for index in 0..count {
                let length = chunk_len(file, index)?;
                let aad = chunk_aad(&digest, file, file_index, index);
                let chunk_path = cache.join(format!("{file_index}-{index}"));
                let bytes = match cached_chunk(&chunk_path, &cache_key, &aad, length)? {
                    Some(bytes) => bytes,
                    None => {
                        let mut request = vec![0];
                        request.extend_from_slice(&file_index.to_be_bytes());
                        request.extend_from_slice(&index.to_be_bytes());
                        channel.send(t, &digest, request)?;
                        let bytes = channel.receive(t, &aad, length)?;
                        if bytes.len() != length {
                            return Err(hub_error("a transfer chunk has the wrong size"));
                        }
                        let mut receipt = keyed(&cache_key, b"finished-chunk", &aad);
                        receipt.update(&bytes);
                        let tag = receipt.finalize().into_bytes();
                        atomic_write(&chunk_path, &cache, |out| {
                            out.write_all(&bytes)?;
                            out.write_all(&tag)?;
                            Ok(())
                        })?;
                        bytes
                    }
                };
                output.write_all(&bytes)?;
                hash.update(&bytes);
            }
            if format!("{:x}", hash.finalize()) != file.sha256 {
                // Authenticated but wrong source contents must not poison future retries.
                fs::remove_dir_all(&cache)?;
                return Err(hub_error("a staged file does not match the bundle"));
            }
            Ok(())
        })?;
    }
    verify_streaming(&b, staged)?;
    channel.send(t, &digest, vec![1])?;
    if channel.receive(t, &digest, 1)? != [1] {
        return Err(hub_error("hub did not finish the transfer"));
    }
    fs::remove_dir_all(cache_root)?;
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_peer_is_refused_before_connecting_or_sending() {
        for ip in ["8.8.8.8", "2001:4860:4860::8888", "::ffff:8.8.8.8"] {
            let (mut sender, mut receiver) = MemoryTransport::pair();
            let mut connected = false;
            let result = connect_home(SocketAddr::new(ip.parse().unwrap(), 5353), |_| {
                connected = true;
                sender.write_all(b"must never be sent")?;
                Ok(())
            });
            assert!(matches!(result, Err(Error::Hub(_))));
            assert!(!connected);
            drop(sender);
            let mut bytes = Vec::new();
            receiver.read_to_end(&mut bytes).unwrap();
            assert!(bytes.is_empty());
        }
    }

    #[test]
    fn tcp_transport_sets_io_timeouts_on_both_ends() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpTransport::connect(listener.local_addr().unwrap()).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        let server = TcpTransport::from_stream(accepted).unwrap();
        for stream in [&client.0, &server.0] {
            assert_eq!(stream.read_timeout().unwrap(), Some(IO_TIMEOUT));
            assert_eq!(stream.write_timeout().unwrap(), Some(IO_TIMEOUT));
        }
    }

    #[test]
    fn resolved_records_filter_public_addresses_and_malformed_txt() {
        let service = |port, properties: [(&str, &str); 2]| {
            mdns_sd::ServiceInfo::new(
                SERVICE,
                "test",
                "test.local.",
                "127.0.0.1,8.8.8.8,::1",
                port,
                properties.as_slice(),
            )
            .unwrap()
            .as_resolved_service()
        };
        let properties = [("hub", "mac"), ("targets", "mac,ios")];
        let found = resolved_hubs(&service(1234, properties));
        assert!(resolved_hubs(&service(0, properties)).is_empty());
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|hub| is_home_network(hub.addr)
            && hub.hub == "mac"
            && hub.port == 1234
            && hub.targets == [Os::Mac, Os::Ios]));
        for properties in [
            [("hub", "mac"), ("targets", "mac,unknown")],
            [("hub", "mac"), ("targets", "")],
            [("hub", ""), ("targets", "mac")],
            [("hub", "mac"), ("other", "mac")],
        ] {
            assert!(resolved_hubs(&service(1234, properties)).is_empty());
        }
    }
}
