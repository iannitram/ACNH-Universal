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
