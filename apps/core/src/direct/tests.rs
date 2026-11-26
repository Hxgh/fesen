use super::*;
use tokio::time::Instant;

struct DualPath;

impl Platform for DualPath {
    fn network(&self) -> io::Result<platform::Network> {
        Ok(platform::Network::default())
    }
    fn protect(&self, _: &socket2::Socket, _: Option<Ipv4Addr>) -> io::Result<()> {
        Ok(())
    }
    fn alternate_udp(&self) -> Option<(UdpSocket, Vec<Ipv4Addr>)> {
        Some((
            platform::datagram(|_| Ok(())).ok()?,
            vec![Ipv4Addr::new(192, 168, 3, 9)],
        ))
    }
    fn ipv6_udp(&self) -> Option<(UdpSocket, Vec<std::net::Ipv6Addr>)> {
        Some((
            platform::datagram6(|_| Ok(())).ok()?,
            vec!["2001:db8::1".parse().unwrap()],
        ))
    }
    fn apply(&self, _: &platform::Tun) -> io::Result<()> {
        Ok(())
    }
    fn write_packet(&self, _: &[u8]) {}
    fn set_input(&self, _: tokio::sync::mpsc::Sender<Vec<u8>>) {}
}

#[tokio::test]
async fn alternate_candidates_advertise_and_accept_on_their_own_socket() {
    let origin = Identity::new().unwrap();
    let target = Identity::new().unwrap();
    let sender = Direct::new(origin.clone(), &DualPath, CancellationToken::new(), false).unwrap();
    let receiver = Direct::new(target.clone(), &DualPath, CancellationToken::new(), false).unwrap();
    receiver.renew(vec![Peer {
        id: "sender".into(),
        certificate: origin.cert,
    }]);
    sender.renew(vec![Peer {
        id: "receiver".into(),
        certificate: target.cert,
    }]);
    let candidates = receiver.candidates(vec![Ipv4Addr::new(10, 1, 0, 1)]);
    assert_eq!(candidates.len(), 2);
    assert_ne!(candidates[0].port(), candidates[1].port());
    assert_eq!(*candidates[1].ip(), Ipv4Addr::new(192, 168, 3, 9));
    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, candidates[1].port());
    let accepting = async {
        let connection = receiver.accept().await.unwrap().await.unwrap();
        assert!(receiver.identify(&connection).is_some());
        connection
    };
    let dialing = sender.connect("receiver", || async { Ok(vec![address.into()]) });
    let (outgoing, _) = timeout(Duration::from_secs(5), async {
        tokio::join!(dialing, accepting)
    })
    .await
    .unwrap();
    assert_eq!(
        outgoing.unwrap().0.remote_address(),
        SocketAddr::V4(address)
    );
    sender.close();
    receiver.close();
}

#[tokio::test]
async fn ipv6_transport_authenticates_and_carries_an_ipv4_target() {
    let caller = Identity::new().unwrap();
    let publisher = Identity::new().unwrap();
    let sender = Direct::new(caller.clone(), &DualPath, CancellationToken::new(), false).unwrap();
    let receiver = Direct::new(
        publisher.clone(),
        &DualPath,
        CancellationToken::new(),
        false,
    )
    .unwrap();
    sender.renew(vec![Peer {
        id: "publisher".into(),
        certificate: publisher.cert,
    }]);
    receiver.renew(vec![Peer {
        id: "caller".into(),
        certificate: caller.cert,
    }]);
    let advertised = receiver.candidates6();
    assert_eq!(advertised.len(), 1);
    assert_eq!(advertised[0].ip().to_string(), "2001:db8::1");
    let loopback = SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, advertised[0].port()));
    let target = "198.51.100.10:8080".parse().unwrap();
    let publisher = async {
        let conn = receiver.accept().await.unwrap().await.unwrap();
        assert!(receiver.identify(&conn).is_some());
        let (mut tx, mut rx) = conn.accept_bi().await.unwrap();
        assert_eq!(frame_in::<Open>(&mut rx).await.unwrap().target, target);
        frame_out(&mut tx, &Option::<Code>::None).await.unwrap();
        tx.finish().unwrap();
        tx.stopped().await.unwrap();
    };
    let caller = async {
        let (conn, _) = sender
            .connect("publisher", || async { Ok(vec![loopback]) })
            .await
            .unwrap();
        assert_eq!(conn.remote_address(), loopback);
        let (_, mut rx) = open(
            &conn,
            Open {
                service: "test".into(),
                target,
            },
        )
        .await
        .unwrap();
        assert_eq!(rx.read(&mut [0]).await.unwrap(), None);
    };
    timeout(Duration::from_secs(5), async {
        tokio::join!(publisher, caller)
    })
    .await
    .unwrap();
    sender.close();
    receiver.close();
}

#[test]
fn expired_roster_cannot_authorize_new_traffic() {
    let membership = Membership::default();
    let cert = vec![1, 2, 3];
    membership.renew(vec![Peer {
        id: "member".into(),
        certificate: cert.clone(),
    }]);
    let allowed = membership.certificate(&cert).unwrap();
    membership.expire_now();
    assert!(membership.certificate(&cert).is_none());
    assert!(membership.peer("member").is_none());
    membership.invalidate();
    assert!(allowed.cancel.is_cancelled());
}

#[tokio::test]
async fn quic_bulk_stream_throughput_and_o1_membership_lookup() {
    let sender_id = Identity::new().unwrap();
    let receiver_id = Identity::new().unwrap();
    let sender = Direct::new(sender_id.clone(), &DualPath, CancellationToken::new(), false).unwrap();
    let receiver =
        Direct::new(receiver_id.clone(), &DualPath, CancellationToken::new(), false).unwrap();

    let mut peers = Vec::with_capacity(128);
    peers.push(Peer {
        id: "sender".into(),
        certificate: sender_id.cert.clone(),
    });
    for i in 1..128u16 {
        let mut fake = vec![0u8; 128];
        fake[0..2].copy_from_slice(&i.to_be_bytes());
        peers.push(Peer {
            id: format!("peer-{i}"),
            certificate: fake,
        });
    }
    receiver.renew(peers);
    sender.renew(vec![Peer {
        id: "receiver".into(),
        certificate: receiver_id.cert,
    }]);
    assert!(receiver.membership.certificate(&sender_id.cert).is_some());

    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, receiver.local_port());
    const TOTAL: usize = 8 * 1024 * 1024;
    const CHUNK: usize = 64 * 1024;

    let server_task = tokio::spawn({
        let receiver = receiver.clone();
        async move {
            let conn = receiver.accept().await.unwrap().await.unwrap();
            let (mut tx, mut rx) = conn.accept_bi().await.unwrap();
            let _open: Open = frame_in(&mut rx).await.unwrap();
            frame_out(&mut tx, &Option::<Code>::None).await.unwrap();
            let payload = vec![0x5au8; CHUNK];
            let mut sent = 0;
            while sent < TOTAL {
                tx.write_all(&payload).await.unwrap();
                sent += CHUNK;
            }
            tx.finish().unwrap();
            let _ = tx.stopped().await;
        }
    });

    let (conn, _) = sender
        .connect("receiver", || async { Ok(vec![address.into()]) })
        .await
        .unwrap();
    let started = Instant::now();
    let (_, mut rx) = open(
        &conn,
        Open {
            service: "bench".into(),
            target: "198.51.100.10:80".parse().unwrap(),
        },
    )
    .await
    .unwrap();
    let mut buf = vec![0u8; CHUNK];
    let mut received = 0;
    while let Some(n) = rx.read(&mut buf).await.unwrap() {
        received += n;
    }
    let elapsed = started.elapsed();
    server_task.await.unwrap();
    assert_eq!(received, TOTAL);
    let mbps = (TOTAL as f64 / (1024.0 * 1024.0)) / elapsed.as_secs_f64();
    eprintln!("QUIC loopback 8 MiB transfer: {elapsed:?} ({mbps:.1} MiB/s)");
    sender.close();
    receiver.close();
}
