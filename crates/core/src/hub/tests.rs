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
