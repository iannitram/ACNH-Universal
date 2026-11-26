//! A running hub: announces itself, answers pairing, bundle, prepared-data and report requests,
//! and shows the owner a generated six-digit pairing code. Both `hub serve` and the launcher's
//! hub page run it.
use super::*;
use std::net::{Ipv4Addr, TcpListener, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

/// What a hub serves and how it presents itself.
pub struct HostConfig {
    /// The name other devices pair with and pull from.
    pub name: String,
    pub role: crate::recipe::Hub,
    /// Published builds: `<name>/bundle.txt` beside each build's files (`publish_build`).
    pub builds: PathBuf,
    /// Prepared Android data, for the Strix Halo hub only.
    pub prepared: Option<PathBuf>,
    /// Arm pairing with a generated code. A code that is used up or has expired is replaced.
    pub pair: bool,
    /// Announce on the home network (mDNS). Tests that only use the port turn it off.
    pub announce: bool,
}

/// What a hub shows its owner.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostStatus {
    /// The six-digit code a new device types, while pairing is armed.
    pub code: Option<String>,
    /// Requests answered successfully so far.
    pub served: u32,
    /// The code of the last failed request, if any.
    pub last_error: Option<String>,
}

/// Most connections served at once; the rest are dropped.
const MAX_CONNECTIONS: usize = 32;

struct Shared {
    stop: AtomicBool,
    status: Mutex<HostStatus>,
}

/// A hub listening on a home-network address. Dropping it stops the hub and its announcement.
pub struct HubHost {
    shared: Arc<Shared>,
    address: SocketAddr,
    thread: Option<JoinHandle<()>>,
}

/// This device's address on the home network, if it has one: the local address of the route to
/// a private address (nothing is sent). `None` when it is not on a home network.
pub fn local_home_address() -> Option<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(10, 255, 255, 255), 9)).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (is_home_network(ip) && !ip.is_loopback()).then_some(ip)
}

fn new_session(shared: &Shared) -> Option<PairingSession> {
    let code = generate_code().ok()?;
    let session = PairingSession::new(&code).ok()?;
    if let Ok(mut status) = shared.status.lock() {
        status.code = Some(code);
    }
    Some(session)
}

impl HubHost {
    /// Starts serving on `listener`, which must be bound to a home-network address.
    pub fn start(dirs: AppDirs, listener: TcpListener, config: HostConfig) -> Result<HubHost> {
        let address = listener.local_addr()?;
        check_home(address.ip())?;
        if config.prepared.is_some() && config.role == crate::recipe::Hub::Mac {
            return Err(hub_error(
                "prepared data requires the Strix Halo or desktop hub",
            ));
        }
        listener.set_nonblocking(true)?;
        let announcement = if config.announce {
            let hostname = format!("{}.local.", config.name);
            Some(announce(
                &config.name,
                &hostname,
                config.role,
                &[address.ip()],
                address.port(),
            )?)
        } else {
            None
        };
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            status: Mutex::new(HostStatus::default()),
        });
        let pairing = if config.pair {
            Some(Mutex::new(
                new_session(&shared).ok_or_else(|| hub_error("could not make a pairing code"))?,
            ))
        } else {
            None
        };
        let worker = Arc::clone(&shared);
        let thread = std::thread::spawn(move || {
            let _announcement = announcement;
            // One short-lived thread per connection, so a slow peer holds only its own socket.
            // The cap keeps a flood of silent connections from exhausting threads.
            let active = AtomicUsize::new(0);
            std::thread::scope(|scope| {
                while !worker.stop.load(Ordering::Relaxed) {
                    if let Some(pairing) = &pairing {
                        let mut session = pairing.lock().unwrap_or_else(|e| e.into_inner());
                        if !session.is_open() {
                            if let Some(fresh) = new_session(&worker) {
                                *session = fresh;
                            }
                        }
                    }
                    let stream = match listener.accept() {
                        Ok((stream, _)) => stream,
                        Err(_) => {
                            std::thread::sleep(Duration::from_millis(50));
                            continue;
                        }
                    };
                    // A stream accepted from a non-blocking listener may start non-blocking.
                    if stream.set_nonblocking(false).is_err() {
                        continue;
                    }
                    if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                        active.fetch_sub(1, Ordering::SeqCst);
                        continue;
                    }
                    let (dirs, config, worker, active, pairing) =
                        (&dirs, &config, &worker, &active, pairing.as_ref());
                    scope.spawn(move || {
                        if let Ok(mut transport) = TcpTransport::from_stream(stream) {
                            let result = serve_request(
                                &mut transport,
                                dirs,
                                &config.builds,
                                config.prepared.as_deref(),
                                crate::recipe::serves(config.role),
                                pairing,
                            );
                            if let Ok(mut status) = worker.status.lock() {
                                match result {
                                    Ok(()) => status.served += 1,
                                    Err(error) => status.last_error = Some(error.code().to_owned()),
                                }
                            }
                        }
                        active.fetch_sub(1, Ordering::SeqCst);
                    });
                }
            });
        });
        Ok(HubHost {
            shared,
            address,
            thread: Some(thread),
        })
    }

    /// The address the hub listens on.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// The code and counts to show the owner.
    pub fn status(&self) -> HostStatus {
        self.shared
            .status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_default()
    }

    /// Stops the hub and waits for it to finish the request it is on.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for HubHost {
    fn drop(&mut self) {
        self.shutdown();
    }
}
