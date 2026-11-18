// SPDX-License-Identifier: MIT
// Copyright (c) 2026 iannitram
// Adapted from iannitram/totk-universal at 80148e16e60b5ca5d34ca87d3ef3360567490352,
// crates/core/tests/hub_discover.rs. See NOTICE for the MIT notice.
#![cfg(feature = "hub")]

use acnh_core::hub::{discover, MemoryTransport, TcpTransport, Transport};
use std::io::{Read, Write};
use std::net::TcpListener;

fn exchange(t: &mut dyn Transport) {
    t.write_all(b"hub request").unwrap();
    t.flush().unwrap();
    let mut reply = [0; 8];
    t.read_exact(&mut reply).unwrap();
    assert_eq!(&reply, b"response");
}

fn respond(t: &mut dyn Transport) {
    let mut request = [0; 11];
    t.read_exact(&mut request).unwrap();
    assert_eq!(&request, b"hub request");
    t.write_all(b"response").unwrap();
}

#[test]
fn memory_pair_supports_hub_protocol_and_eof() {
    let (mut client, mut server) = MemoryTransport::pair();
    assert_eq!(client.read(&mut []).unwrap(), 0);
    client.write_all(b"").unwrap();
    let worker = std::thread::spawn(move || respond(&mut server));
    exchange(&mut client);
    worker.join().unwrap();
    assert_eq!(client.read(&mut [0]).unwrap(), 0);
    assert_eq!(
        client.write(b"closed").unwrap_err().kind(),
        std::io::ErrorKind::BrokenPipe
    );
}

#[test]
fn tcp_transport_checks_and_exchanges_on_loopback() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut server = TcpTransport::from_stream(stream).unwrap();
        respond(&mut server);
    });
    let mut client = TcpTransport::connect(addr).unwrap();
    exchange(&mut client);
    worker.join().unwrap();
}

#[test]
fn tcp_refuses_public_peer() {
    for addr in ["8.8.8.8:1234", "[2001:4860:4860::8888]:1234"] {
        let error = TcpTransport::connect(addr.parse().unwrap()).unwrap_err();
        assert_eq!(error.code(), "hub");
        assert!(error.to_string().contains("outside the home network"));
    }
}

#[test]
fn memory_pair_preserves_bytes_across_write_boundaries() {
    let (mut writer, mut reader) = MemoryTransport::pair();
    writer.write_all(b"abc").unwrap();
    writer.write_all(b"defgh").unwrap();
    drop(writer);
    let mut first = [0; 2];
    reader.read_exact(&mut first).unwrap();
    assert_eq!(&first, b"ab");
    let mut rest = Vec::new();
    reader.read_to_end(&mut rest).unwrap();
    assert_eq!(&rest, b"cdefgh");
}

#[test]
fn zero_wait_does_not_start_mdns() {
    assert!(discover(0).unwrap().is_empty());
}

// Only Linux CI may send multicast: macOS must never prompt for Local Network access.
#[cfg(target_os = "linux")]
#[test]
fn discovers_fake_hub_on_loopback() {
    use acnh_core::hub::{is_home_network, SERVICE};
    use acnh_core::platform::Os;
    use mdns_sd::{IfKind, ServiceDaemon, ServiceInfo};

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let daemon = ServiceDaemon::new().unwrap();
    daemon.disable_interface(IfKind::All).unwrap();
    daemon.enable_interface(IfKind::LoopbackV4).unwrap();
    let instance = format!("hub-discover-{port}");
    let hostname = format!("{instance}.local.");
    let info = ServiceInfo::new(
        SERVICE,
        &instance,
        &hostname,
        "127.0.0.1",
        port,
        [("hub", "strix-halo"), ("targets", "linux,android")].as_slice(),
    )
    .unwrap();
    daemon.register(info).unwrap();
    let result = discover(5000);
    daemon
        .shutdown()
        .unwrap()
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    let found = result.unwrap();
    assert!(found.iter().all(|hub| is_home_network(hub.addr)));
    assert!(found.iter().any(|hub| hub.hub == "strix-halo"
        && hub.addr.is_loopback()
        && hub.port == port
        && hub.targets == [Os::Linux, Os::Android]));
}
