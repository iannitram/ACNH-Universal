#![cfg(feature = "hub")]
// Locked tests for hub::is_home_network (M11): the private-range check, range edges included.
use acnh_core::hub::is_home_network;
use std::net::IpAddr;

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

#[test]
fn ipv4_private_ranges_are_home() {
    for s in [
        "127.0.0.1",
        "127.255.255.255",
        "10.0.0.0",
        "10.255.255.255",
        "172.16.0.0",
        "172.31.255.255",
        "192.168.0.0",
        "192.168.255.255",
        "169.254.0.0",
        "169.254.255.255",
    ] {
        assert!(is_home_network(ip(s)), "{s}");
    }
}

#[test]
fn ipv4_just_outside_the_ranges_is_not_home() {
    for s in [
        "0.0.0.0",
        "8.8.8.8",
        "9.255.255.255",
        "11.0.0.0",
        "126.255.255.255",
        "128.0.0.0",
        "172.15.255.255",
        "172.32.0.0",
        "192.167.255.255",
        "192.169.0.0",
        "169.253.255.255",
        "169.255.0.0",
        "255.255.255.255",
    ] {
        assert!(!is_home_network(ip(s)), "{s}");
    }
}

#[test]
fn ipv6_loopback_unique_local_and_link_local_are_home() {
    for s in [
        "::1",
        "fc00::",
        "fd12:3456::1",
        "fdff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
        "fe80::",
        "fe80::1",
        "febf:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
    ] {
        assert!(is_home_network(ip(s)), "{s}");
    }
}

#[test]
fn ipv6_just_outside_the_ranges_is_not_home() {
    for s in [
        "::",
        "::2",
        "1::1",
        "2001:4860:4860::8888",
        "fbff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
        "fe00::",
        "fe7f:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
        "fec0::",
        "ff02::1",
    ] {
        assert!(!is_home_network(ip(s)), "{s}");
    }
}
