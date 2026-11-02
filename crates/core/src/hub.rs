//! The build hub on the home network (SPEC.md M11; docs/adr/0004-build-hubs.md): discovery
//! (mDNS), one-time pairing (SPAKE2 and a code on both screens), sealed transfer of a bundle
//! and of the game data a prepare made, to my own paired devices only. Behind the `hub`
//! feature; copied from iannitram/totk-universal's `hub` (MIT) by its issues, which add the
//! pure-Rust crates it needs. Planned: only the address check is in the skeleton.

use std::net::IpAddr;

/// True for loopback, 10/8, 172.16/12, 192.168/16, 169.254/16, IPv6 loopback, fc00::/7 and
/// fe80::/10; false for anything else. Every connection checks it first.
pub fn is_home_network(addr: IpAddr) -> bool {
    not_built!("hub::is_home_network")
}
