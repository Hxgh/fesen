use super::*;
use crate::platform::{Network, Platform, Tun};
use socket2::Socket;
use std::{
    collections::{HashMap, HashSet},
    io,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, duplex},
    net::TcpListener,
    sync::mpsc,
};

struct LoopbackPlatform;
impl Platform for LoopbackPlatform {
    fn network(&self) -> io::Result<Network> {
        Ok(Network::default())
    }
    fn protect(&self, _: &Socket, _: Option<Ipv4Addr>) -> io::Result<()> {
        Ok(())
    }
    fn apply(&self, _: &Tun) -> io::Result<()> {
        Ok(())
    }
    fn write_packet(&self, _: &[u8]) {}
    fn set_input(&self, _: mpsc::Sender<Vec<u8>>) {}
}

async fn respond(bytes: &[u8]) -> Result<u16, Code> {
    let (client, mut target) = duplex(4096);
    let mock = async {
        let mut request = vec![];
        while !request.ends_with(b"\r\n\r\n") {
            request.push(target.read_u8().await.unwrap());
        }
        assert!(request.starts_with(b"GET / HTTP/1.1\r\n"));
        let host = b"host: 198.51.100.10:8080\r\n";
        assert!(request.windows(host.len()).any(|s| s == host));
        target.write_all(bytes).await.unwrap();
        target.shutdown().await.unwrap();
    };
    tokio::join!(http(client, "198.51.100.10:8080".parse().unwrap()), mock).0
}

#[tokio::test]
async fn gets_a_page_and_reports_http_status_without_following_redirects() {
    assert_eq!(
        respond(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK").await,
        Ok(200)
    );
    assert_eq!(
        respond(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n").await,
        Ok(503)
    );
    assert_eq!(
        respond(
            b"HTTP/1.1 302 Found\r\nLocation: http://other.invalid/\r\nContent-Length: 0\r\n\r\n"
        )
        .await,
        Ok(302)
    );
}

#[tokio::test]
async fn rejects_both_an_open_non_http_socket_and_an_incomplete_page() {
    assert_eq!(respond(b"SSH-2.0-test\r\n").await, Err(Code::HttpInvalid));
    assert_eq!(
        respond(b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\nshort").await,
        Err(Code::HttpInvalid)
    );
}

#[tokio::test]
async fn ssh_probe_reads_banner_passively_and_identifies_os() {
    let (client, mut server) = duplex(256);
    let send = async {
        server
            .write_all(b"SSH-2.0-OpenSSH_for_Windows_9.5\r\n")
            .await
            .unwrap();
        server.shutdown().await.unwrap();
    };
    let info = tokio::join!(ssh(client, Duration::from_secs(1)), send)
        .0
        .unwrap();
    assert_eq!(info.os, Some(HostOs::Windows));
    assert_eq!(info.banner, "SSH-2.0-OpenSSH_for_Windows_9.5");

    let (client2, mut server2) = duplex(256);
    let send2 = async {
        server2
            .write_all(b"SSH-2.0-OpenSSH_9.6p1 Ubuntu-3ubuntu13\r\n")
            .await
            .unwrap();
    };
    let info2 = tokio::join!(ssh(client2, Duration::from_secs(1)), send2)
        .0
        .unwrap();
    assert_eq!(info2.os, Some(HostOs::Linux));

    let (client3, mut server3) = duplex(256);
    let send3 = async {
        server3.write_all(b"HTTP/1.1 200 OK\r\n").await.unwrap();
    };
    assert_eq!(
        tokio::join!(ssh(client3, Duration::from_secs(1)), send3).0,
        Err(Code::ProtocolMismatch)
    );
}

#[tokio::test]
async fn rdp_and_tls_probes_validate_protocol_responses() {
    let (client, mut server) = duplex(256);
    let rdp_mock = async {
        let mut req = [0u8; 19];
        server.read_exact(&mut req).await.unwrap();
        assert_eq!(req, RDP_X224_REQ);
        server
            .write_all(&[
                0x03, 0x00, 0x00, 0x13, 0x0e, 0xd0, 0x00, 0x00, 0x12, 0x34, 0x00, 0x02, 0x2f, 0x08,
                0x00, 0x02, 0x00, 0x00, 0x00,
            ])
            .await
            .unwrap();
    };
    let info = tokio::join!(rdp(client, Duration::from_secs(1)), rdp_mock)
        .0
        .unwrap();
    assert!(info.nla);

    let (tls_client, mut tls_server) = duplex(256);
    let tls_mock = async {
        let mut hdr = [0u8; 5];
        tls_server.read_exact(&mut hdr).await.unwrap();
        assert_eq!(hdr[0], 0x16);
        tls_server
            .write_all(&[0x16, 0x03, 0x03, 0x00, 0x04, 0x02, 0x00, 0x00, 0x00])
            .await
            .unwrap();
    };
    assert_eq!(
        tokio::join!(tls(tls_client, Duration::from_secs(1)), tls_mock).0,
        Ok(())
    );
}

#[tokio::test]
async fn host_probe_rejects_reserved_ips_and_excess_ports() {
    let p = LoopbackPlatform;
    assert_eq!(host(&p, Ipv4Addr::LOCALHOST, &[]).await, Err(Code::Invalid));
    assert_eq!(
        host(&p, Ipv4Addr::new(198, 18, 0, 5), &[]).await,
        Err(Code::Invalid)
    );
    assert_eq!(
        host(&p, Ipv4Addr::new(198, 51, 100, 10), &[0]).await,
        Err(Code::Invalid)
    );
    assert_eq!(
        host(
            &p,
            Ipv4Addr::new(198, 51, 100, 10),
            &[8000, 8001, 8002, 8003, 8004, 8005]
        )
        .await,
        Err(Code::Invalid)
    );
}

#[tokio::test]
async fn custom_port_probe_detects_ssh_http_and_https_on_nonstandard_ports() {
    let ssh_ln = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let ssh_port = ssh_ln.local_addr().unwrap().port();
    tokio::spawn(async move {
        if let Ok((mut s, _)) = ssh_ln.accept().await {
            let _ = s.write_all(b"SSH-2.0-OpenSSH_9.6p1 Debian-1\r\n").await;
        }
    });

    let http_ln = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let http_port = http_ln.local_addr().unwrap().port();
    tokio::spawn(async move {
        if let Ok((mut s, _)) = http_ln.accept().await {
            let mut buf = [0u8; 256];
            let _ = s.read(&mut buf).await;
            let _ = s
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await;
        }
    });

    let tls_ln = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let tls_port = tls_ln.local_addr().unwrap().port();
    tokio::spawn(async move {
        // First conn gets HEAD / -> close immediately; second gets TLS ClientHello -> ServerHello.
        for _ in 0..2 {
            if let Ok((mut s, _)) = tls_ln.accept().await {
                let mut buf = [0u8; 256];
                let n = s.read(&mut buf).await.unwrap_or(0);
                if n > 0 && buf[0] == 0x16 {
                    let _ = s
                        .write_all(&[0x16, 0x03, 0x03, 0x00, 0x04, 0x02, 0x00, 0x00, 0x00])
                        .await;
                }
            }
        }
    });

    let p = LoopbackPlatform;
    let (ssh_res, ssh_os) =
        host::probe_host_port_bounded(&p, Ipv4Addr::LOCALHOST, ssh_port, true, 1000).await;
    assert!(ssh_res.open && ssh_res.kind == ServiceKind::Ssh);
    assert_eq!(ssh_os, Some(HostOs::Linux));

    let (http_res, _) =
        host::probe_host_port_bounded(&p, Ipv4Addr::LOCALHOST, http_port, true, 1000).await;
    assert!(http_res.open && http_res.kind == ServiceKind::Http);
    assert_eq!(http_res.http_status, Some(200));
    // Pure TCP connect latency must not include the 240ms passive SSH banner wait.
    assert!(http_res.rtt_ms.unwrap() < 150, "rtt={:?}", http_res.rtt_ms);

    let (tls_res, _) =
        host::probe_host_port_bounded(&p, Ipv4Addr::LOCALHOST, tls_port, true, 1000).await;
    assert!(tls_res.open && tls_res.kind == ServiceKind::Https);

    // Slow/silent peer on an extra port must stay bounded and fall back to open TCP without aborting.
    let silent_ln = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let silent_port = silent_ln.local_addr().unwrap().port();
    let _keep = tokio::spawn(async move {
        if let Ok((_s, _)) = silent_ln.accept().await {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
    let (silent_res, _) =
        host::probe_host_port_bounded(&p, Ipv4Addr::LOCALHOST, silent_port, true, 1000).await;
    assert!(silent_res.open && silent_res.kind == ServiceKind::Tcp);
}

#[tokio::test]
async fn local_hints_enrich_tcp_detail_and_skip_non_http_recommendations() {
    let tcp_ln = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let tcp_port = tcp_ln.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = tcp_ln.accept().await;
        tokio::time::sleep(Duration::from_millis(500)).await;
    });

    let p = LoopbackPlatform;
    let user_req = HashSet::new();
    let mut local_hints = HashMap::new();
    local_hints.insert(tcp_port, Some("node".to_owned()));
    local_hints.insert(3306, Some("mysqld".to_owned()));

    let res = host::probe_targets(
        &p,
        Ipv4Addr::LOCALHOST,
        vec![(tcp_port, true), (3306, true)],
        &user_req,
        &local_hints,
    )
    .await
    .unwrap();

    let node_entry = res.ports.iter().find(|e| e.port == tcp_port).unwrap();
    assert!(node_entry.open);
    assert!(node_entry.recommended);
    assert_eq!(node_entry.detail.as_deref(), Some("node"));

    // Non-HTTP DB port stays unchecked by default unless explicitly requested.
    let db_entry = res.ports.iter().find(|e| e.port == 3306).unwrap();
    assert!(!db_entry.recommended);
}
