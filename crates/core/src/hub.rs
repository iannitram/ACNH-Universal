//! The build hub on the home network (SPEC.md M11; docs/adr/0004-build-hubs.md): discovery
//! (mDNS), one-time pairing (SPAKE2 and a code on both screens), sealed transfer of a bundle
//! and of the game data a prepare made, to my own paired devices only. Behind the `hub`
//! feature; copied from iannitram/totk-universal's `hub` (MIT) by its issues, which add the
//! pure-Rust crates it needs. Planned: only the address check is in the skeleton.

use std::net::IpAddr;

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

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::Error;

/// The mDNS service type a hub announces and a device browses for.
pub const SERVICE_TYPE: &str = "_acnh-universal._tcp.local.";

/// A hub found (or announced) on the home network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubInfo {
    /// The hub's instance name (a device name the player chose).
    pub name: String,
    /// Where it listens.
    pub addr: SocketAddr,
}

/// How hubs are announced and found. mDNS in the app, loopback in tests.
pub trait Transport {
    /// Announce a hub. Refuses an address outside the home network.
    fn announce(&self, hub: &HubInfo) -> Result<(), Error>;
    /// Hubs seen within `timeout`.
    fn browse(&self, timeout: Duration) -> Result<Vec<HubInfo>, Error>;
}

fn refuse_public(hub: &HubInfo) -> Result<(), Error> {
    if is_home_network(hub.addr.ip()) {
        Ok(())
    } else {
        Err(Error::Hub(format!(
            "{} is not on the home network; refused",
            hub.addr.ip()
        )))
    }
}

/// Find hubs through `transport`, keeping only private addresses.
pub fn discover(transport: &dyn Transport, timeout: Duration) -> Result<Vec<HubInfo>, Error> {
    let mut hubs = transport.browse(timeout)?;
    hubs.retain(|hub| is_home_network(hub.addr.ip()));
    Ok(hubs)
}

/// An in-process transport: announced hubs are what browse returns. For tests.
#[derive(Debug, Clone, Default)]
pub struct LoopbackTransport {
    hubs: Arc<Mutex<Vec<HubInfo>>>,
}

impl LoopbackTransport {
    pub fn new() -> Self {
        Self::default()
    }

    /// Plant a hub without the address check, as a hostile peer could.
    pub fn inject(&self, hub: HubInfo) {
        self.lock().push(hub);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<HubInfo>> {
        self.hubs.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Transport for LoopbackTransport {
    fn announce(&self, hub: &HubInfo) -> Result<(), Error> {
        refuse_public(hub)?;
        self.lock().push(hub.clone());
        Ok(())
    }

    fn browse(&self, _timeout: Duration) -> Result<Vec<HubInfo>, Error> {
        Ok(self.lock().clone())
    }
}

/// The real transport: mDNS announce and browse with `mdns-sd`.
pub struct MdnsTransport {
    daemon: mdns_sd::ServiceDaemon,
}

fn mdns_err(e: mdns_sd::Error) -> Error {
    Error::Hub(format!("mDNS: {e}"))
}

impl MdnsTransport {
    pub fn new() -> Result<Self, Error> {
        let daemon = mdns_sd::ServiceDaemon::new().map_err(mdns_err)?;
        Ok(Self { daemon })
    }
}

impl Drop for MdnsTransport {
    fn drop(&mut self) {
        // Dropping the daemon handle alone leaves its thread and services alive.
        let _ = self.daemon.shutdown();
    }
}

impl Transport for MdnsTransport {
    fn announce(&self, hub: &HubInfo) -> Result<(), Error> {
        refuse_public(hub)?;
        let host = format!("{}.local.", hub.name);
        let info = mdns_sd::ServiceInfo::new(
            SERVICE_TYPE,
            &hub.name,
            &host,
            hub.addr.ip(),
            hub.addr.port(),
            None::<std::collections::HashMap<String, String>>,
        )
        .map_err(mdns_err)?;
        self.daemon.register(info).map_err(mdns_err)
    }

    fn browse(&self, timeout: Duration) -> Result<Vec<HubInfo>, Error> {
        let receiver = self.daemon.browse(SERVICE_TYPE).map_err(mdns_err)?;
        let deadline = Instant::now() + timeout;
        let mut found = Vec::new();
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match receiver.recv_timeout(left) {
                Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) => {
                    let name = info
                        .get_fullname()
                        .strip_suffix(SERVICE_TYPE)
                        .unwrap_or(info.get_fullname())
                        .trim_end_matches('.')
                        .to_string();
                    for ip in info.get_addresses() {
                        found.push(HubInfo {
                            name: name.clone(),
                            addr: SocketAddr::new(*ip, info.get_port()),
                        });
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        // Best effort: a failed stop only leaves an idle browse behind.
        let _ = self.daemon.stop_browse(SERVICE_TYPE);
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hub(ip: &str) -> HubInfo {
        HubInfo {
            name: "kitchen-mac".into(),
            addr: SocketAddr::new(ip.parse().unwrap(), 7447),
        }
    }

    #[test]
    fn loopback_finds_a_fake_hub() {
        let t = LoopbackTransport::new();
        t.announce(&hub("192.168.1.20")).unwrap();
        let found = discover(&t, Duration::from_millis(10)).unwrap();
        assert_eq!(found, vec![hub("192.168.1.20")]);
    }

    #[test]
    fn public_address_is_refused() {
        let t = LoopbackTransport::new();
        assert!(matches!(t.announce(&hub("8.8.8.8")), Err(Error::Hub(_))));
        t.inject(hub("8.8.8.8"));
        assert!(discover(&t, Duration::from_millis(10)).unwrap().is_empty());
    }

    #[test]
    fn dropping_mdns_transport_stops_its_daemon() {
        let t = MdnsTransport::new().unwrap();
        let events = t.daemon.monitor().unwrap();
        drop(t);
        // Shutdown closes the monitor channel; a leaked daemon leaves it open.
        while events.recv_timeout(Duration::from_secs(1)).is_ok() {}
        assert!(events.is_disconnected());
    }
}
