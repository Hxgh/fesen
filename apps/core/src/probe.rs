use crate::platform::{self, Platform};
use fesen_acl::Rules;
use fesen_proto::{Code, HostOs, ServiceKind};
use futures_util::future::join_all;
use http_body_util::{BodyExt, Empty};
use hyper::{Request, body::Bytes, client::conn::http1};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    net::{Ipv4Addr, SocketAddrV4},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    time::{Instant, timeout},
};

pub const DEFAULT_HOST_PORTS: [u16; 5] = [22, 3389, 445, 80, 443];
pub const MAX_HOST_PROBE_PORTS: usize = 10;

// Well-known non-HTTP binary/database/mail ports: never send synthetic HTTP HEAD noise to them.
const NON_HTTP_PORTS: [u16; 18] = [
    21, 25, 53, 110, 143, 465, 587, 993, 995, 1433, 1521, 3306, 5432, 5672, 6379, 9092, 11211,
    27017,
];

// Standard X.224 Connection Request (TPKT + COTP CR + RDP Negotiation Request, zero credentials).
const RDP_X224_REQ: [u8; 19] = [
    0x03, 0x00, 0x00, 0x13, 0x0e, 0xe0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x08, 0x00, 0x03,
    0x00, 0x00, 0x00,
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeOutcome {
    pub kind: ServiceKind,
    pub rtt_ms: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_os: Option<HostOs>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostPortProbe {
    pub port: u16,
    pub open: bool,
    pub extra: bool,
    pub kind: ServiceKind,
    pub recommended: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rtt_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_hint: Option<HostOs>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostProbe {
    pub ip: Ipv4Addr,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_os: Option<HostOs>,
    pub ports: Vec<HostPortProbe>,
    pub duration_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshInfo {
    pub banner: String,
    pub os: Option<HostOs>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RdpInfo {
    pub nla: bool,
}

pub async fn http(
    io: impl AsyncRead + AsyncWrite + Unpin,
    address: SocketAddrV4,
) -> Result<u16, Code> {
    timeout(Duration::from_secs(6), check_http(io, address))
        .await
        .map_err(|_| Code::TargetTimeout)?
}

async fn check_http(
    io: impl AsyncRead + AsyncWrite + Unpin,
    address: SocketAddrV4,
) -> Result<u16, Code> {
    let (mut sender, connection) = http1::Builder::new()
        .max_headers(64)
        .max_buf_size(32 * 1024)
        .handshake(TokioIo::new(io))
        .await
        .map_err(|_| Code::HttpInvalid)?;
    let request = Request::get("/")
        .header("host", address.to_string())
        .header("connection", "close")
        .header("accept-encoding", "identity")
        .body(Empty::<Bytes>::new())
        .map_err(|_| Code::Internal)?;
    let result = async {
        let response = sender
            .send_request(request)
            .await
            .map_err(|_| Code::HttpInvalid)?;
        let status = response.status();
        if status.is_success() {
            let mut body = response.into_body();
            let mut bytes = 0;
            while let Some(frame) = body.frame().await {
                if let Ok(data) = frame.map_err(|_| Code::HttpInvalid)?.into_data() {
                    bytes += data.len();
                    if bytes >= 8 * 1024 {
                        break;
                    }
                }
            }
        }
        Ok(status.as_u16())
    };
    tokio::pin!(result);
    tokio::select! {
        biased;
        code = &mut result => code,
        closed = connection => {
            closed.map_err(|_| Code::HttpInvalid)?;
            result.await
        }
    }
}

pub async fn ssh(mut io: impl AsyncRead + Unpin, wait: Duration) -> Result<SshInfo, Code> {
    let line = read_line_limited(&mut io, 256, wait).await?;
    parse_ssh_banner(&line).ok_or(Code::ProtocolMismatch)
}

pub async fn rdp(
    mut io: impl AsyncRead + AsyncWrite + Unpin,
    wait: Duration,
) -> Result<RdpInfo, Code> {
    timeout(wait, async {
        io.write_all(&RDP_X224_REQ)
            .await
            .map_err(|_| Code::TargetUnavailable)?;
        let _ = io.flush().await;
        let mut buf = [0u8; 64];
        let mut len = 0;
        while len < 7 {
            let n = io
                .read(&mut buf[len..])
                .await
                .map_err(|_| Code::ProtocolMismatch)?;
            if n == 0 {
                break;
            }
            len += n;
        }
        if len < 7 || buf[0] != 0x03 || buf[1] != 0x00 || buf[5] != 0xd0 {
            return Err(Code::ProtocolMismatch);
        }
        let nla = len >= 19 && buf[11] == 0x02 && (buf[15] & 0x02) != 0;
        Ok(RdpInfo { nla })
    })
    .await
    .map_err(|_| Code::TargetTimeout)?
}

pub async fn tls(
    mut io: impl AsyncRead + AsyncWrite + Unpin,
    wait: Duration,
) -> Result<(), Code> {
    let hello = tls_client_hello();
    timeout(wait, async {
        io.write_all(&hello)
            .await
            .map_err(|_| Code::TargetUnavailable)?;
        let _ = io.flush().await;
        let mut buf = [0u8; 6];
        io.read_exact(&mut buf)
            .await
            .map_err(|_| Code::ProtocolMismatch)?;
        if buf[0] == 0x16 && buf[1] == 0x03 && (1..=3).contains(&buf[2]) && buf[5] == 0x02 {
            Ok(())
        } else {
            Err(Code::ProtocolMismatch)
        }
    })
    .await
    .map_err(|_| Code::TargetTimeout)?
}

enum HeadOutcome {
    Http(u16),
    ClosedOrAlert,
    Other,
}

async fn probe_http_head(
    io: &mut (impl AsyncRead + AsyncWrite + Unpin),
    address: SocketAddrV4,
    wait: Duration,
) -> HeadOutcome {
    let req = format!("HEAD / HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
    let res = timeout(wait, async {
        io.write_all(req.as_bytes()).await.ok()?;
        let _ = io.flush().await;
        let mut buf = [0u8; 256];
        let n = io.read(&mut buf).await.ok()?;
        Some(buf[..n].to_vec())
    })
    .await;
    match res {
        Ok(Some(bytes)) if bytes.is_empty() => HeadOutcome::ClosedOrAlert,
        Ok(Some(bytes)) => {
            if let Some(code) = parse_http_status_line(&bytes) {
                HeadOutcome::Http(code)
            } else if bytes.len() >= 2 && bytes[0] == 0x15 && bytes[1] == 0x03 {
                HeadOutcome::ClosedOrAlert
            } else {
                HeadOutcome::Other
            }
        }
        Ok(None) => HeadOutcome::ClosedOrAlert,
        Err(_) => HeadOutcome::Other,
    }
}

pub async fn host(
    platform: &dyn Platform,
    ip: Ipv4Addr,
    extra_ports: &[u16],
) -> Result<HostProbe, Code> {
    // Enforce the exact same IPv4 non-reserved boundary as publishing rules.
    Rules::parse(&ip.to_string(), "80").map_err(|_| Code::Invalid)?;
    let mut targets: Vec<(u16, bool)> = DEFAULT_HOST_PORTS.iter().map(|&p| (p, false)).collect();
    let mut seen: HashSet<u16> = DEFAULT_HOST_PORTS.into_iter().collect();
    let mut extras = Vec::new();
    for &port in extra_ports {
        if port == 0 {
            return Err(Code::Invalid);
        }
        if seen.insert(port) {
            extras.push(port);
        }
    }
    extras.sort_unstable();
    targets.extend(extras.into_iter().map(|p| (p, true)));
    if targets.len() > MAX_HOST_PROBE_PORTS {
        return Err(Code::Invalid);
    }
    probe_targets(platform, ip, targets).await
}

pub async fn ports(
    platform: &dyn Platform,
    ip: Ipv4Addr,
    requested: &[u16],
) -> Result<HostProbe, Code> {
    Rules::parse(&ip.to_string(), "80").map_err(|_| Code::Invalid)?;
    let mut seen = HashSet::new();
    let mut targets = Vec::new();
    for &port in requested {
        if port == 0 {
            return Err(Code::Invalid);
        }
        if seen.insert(port) {
            targets.push((port, !DEFAULT_HOST_PORTS.contains(&port)));
        }
    }
    if targets.is_empty() || targets.len() > MAX_HOST_PROBE_PORTS {
        return Err(Code::Invalid);
    }
    probe_targets(platform, ip, targets).await
}

async fn probe_targets(
    platform: &dyn Platform,
    ip: Ipv4Addr,
    targets: Vec<(u16, bool)>,
) -> Result<HostProbe, Code> {
    let started = Instant::now();
    // Each port enforces its own bounded budget (<=2200ms) so one slow custom port never discards
    // healthy results from sibling ports; outer timeout is a pure safety guard.
    let raw = timeout(
        Duration::from_millis(3200),
        join_all(
            targets
                .into_iter()
                .map(|(port, extra)| probe_host_port_bounded(platform, ip, port, extra)),
        ),
    )
    .await
    .map_err(|_| Code::TargetTimeout)?;

    let mut host_os = None;
    let mut saw_linux = false;
    let mut has_rdp = false;
    let mut has_remote_entry = false;

    for (entry, os_hint) in &raw {
        if !entry.open {
            continue;
        }
        if matches!(entry.kind, ServiceKind::Ssh | ServiceKind::Rdp) {
            has_remote_entry = true;
        }
        if entry.kind == ServiceKind::Rdp {
            has_rdp = true;
        }
        match os_hint {
            Some(HostOs::Windows) => host_os = Some(HostOs::Windows),
            Some(HostOs::Linux) => saw_linux = true,
            None => {}
        }
    }
    if host_os.is_none() {
        if has_rdp {
            host_os = Some(HostOs::Windows);
        } else if saw_linux {
            host_os = Some(HostOs::Linux);
        }
    }

    let ports = raw
        .into_iter()
        .map(|(mut entry, _)| {
            entry.recommended = entry.open
                && match entry.kind {
                    ServiceKind::Ssh | ServiceKind::Rdp => true,
                    ServiceKind::Smb => entry.extra,
                    ServiceKind::Http | ServiceKind::Https => entry.extra || !has_remote_entry,
                    ServiceKind::Tcp => entry.extra,
                };
            entry
        })
        .collect();

    Ok(HostProbe {
        ip,
        host_os,
        ports,
        duration_ms: started.elapsed().as_millis().min(u32::MAX as u128) as u32,
    })
}

async fn probe_host_port_bounded(
    platform: &dyn Platform,
    ip: Ipv4Addr,
    port: u16,
    extra: bool,
) -> (HostPortProbe, Option<HostOs>) {
    let default_kind = match port {
        22 => ServiceKind::Ssh,
        3389 => ServiceKind::Rdp,
        445 => ServiceKind::Smb,
        80 => ServiceKind::Http,
        443 => ServiceKind::Https,
        _ => ServiceKind::Tcp,
    };
    let target = SocketAddrV4::new(ip, port);
    let started = Instant::now();
    let Ok(mut stream) = platform::tcp_timeout(platform, target, Duration::from_millis(1000)).await
    else {
        return (
            HostPortProbe {
                port,
                open: false,
                extra,
                kind: default_kind,
                recommended: false,
                rtt_ms: None,
                http_status: None,
                detail: None,
                os_hint: None,
            },
            None,
        );
    };
    // Measure pure TCP connect latency before protocol fingerprinting waits.
    let rtt_ms = started.elapsed().as_millis().clamp(1, u32::MAX as u128) as u32;

    let fingerprint = async {
        match port {
            22 => match ssh(&mut stream, Duration::from_millis(1150)).await {
                Ok(info) => (ServiceKind::Ssh, None, Some(info.banner), info.os),
                Err(_) => (ServiceKind::Tcp, None, None, None),
            },
            3389 => match rdp(&mut stream, Duration::from_millis(800)).await {
                Ok(info) => (
                    ServiceKind::Rdp,
                    None,
                    Some(if info.nla { "NLA" } else { "RDP" }.into()),
                    Some(HostOs::Windows),
                ),
                Err(_) => (ServiceKind::Tcp, None, None, None),
            },
            445 => (ServiceKind::Smb, None, None, None),
            80 => match probe_http_head(&mut stream, target, Duration::from_millis(800)).await {
                HeadOutcome::Http(code) => (ServiceKind::Http, Some(code), None, None),
                _ => (ServiceKind::Tcp, None, None, None),
            },
            443 => match tls(&mut stream, Duration::from_millis(800)).await {
                Ok(()) => (ServiceKind::Https, None, None, None),
                Err(_) => (ServiceKind::Tcp, None, None, None),
            },
            _ => {
                // Custom user-requested port: check passive banner first (catches custom SSH like 2222).
                let mut banner_buf = [0u8; 160];
                match timeout(Duration::from_millis(240), stream.read(&mut banner_buf)).await {
                    Ok(Ok(n @ 1..)) => {
                        if let Some(info) = parse_ssh_banner(&banner_buf[..n]) {
                            (ServiceKind::Ssh, None, Some(info.banner), info.os)
                        } else {
                            (ServiceKind::Tcp, None, None, None)
                        }
                    }
                    Ok(Ok(0)) | Ok(Err(_)) => (ServiceKind::Tcp, None, None, None),
                    Err(_) => {
                        if NON_HTTP_PORTS.contains(&port) {
                            return (ServiceKind::Tcp, None, None, None);
                        }
                        match probe_http_head(&mut stream, target, Duration::from_millis(650)).await {
                            HeadOutcome::Http(code) => (ServiceKind::Http, Some(code), None, None),
                            HeadOutcome::ClosedOrAlert => {
                                drop(stream);
                                if let Ok(mut tls_stream) = platform::tcp_timeout(
                                    platform,
                                    target,
                                    Duration::from_millis(500),
                                )
                                .await
                                    && tls(&mut tls_stream, Duration::from_millis(650))
                                        .await
                                        .is_ok()
                                {
                                    (ServiceKind::Https, None, None, None)
                                } else {
                                    (ServiceKind::Tcp, None, None, None)
                                }
                            }
                            HeadOutcome::Other => (ServiceKind::Tcp, None, None, None),
                        }
                    }
                }
            }
        }
    };

    // If protocol identification exceeds remaining budget, still preserve the open TCP port.
    let (kind, http_status, detail, os_hint) = timeout(Duration::from_millis(1200), fingerprint)
        .await
        .unwrap_or((ServiceKind::Tcp, None, None, None));

    (
        HostPortProbe {
            port,
            open: true,
            extra,
            kind,
            recommended: false,
            rtt_ms: Some(rtt_ms),
            http_status,
            detail,
            os_hint,
        },
        os_hint,
    )
}

async fn read_line_limited(
    io: &mut (impl AsyncRead + Unpin),
    limit: usize,
    wait: Duration,
) -> Result<Vec<u8>, Code> {
    timeout(wait, async {
        let mut out = Vec::with_capacity(64);
        let mut chunk = [0u8; 64];
        while out.len() < limit {
            let max = (limit - out.len()).min(chunk.len());
            let n = io
                .read(&mut chunk[..max])
                .await
                .map_err(|_| Code::ProtocolMismatch)?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&chunk[..n]);
            if out.contains(&b'\n') {
                break;
            }
        }
        Ok(out)
    })
    .await
    .map_err(|_| Code::TargetTimeout)?
}

fn parse_ssh_banner(bytes: &[u8]) -> Option<SshInfo> {
    let line = bytes.split(|&b| b == b'\n' || b == b'\r').next()?;
    if !(line.starts_with(b"SSH-2.0-") || line.starts_with(b"SSH-1.99-")) {
        return None;
    }
    let clean: String = line
        .iter()
        .copied()
        .filter(|b| (0x20..=0x7e).contains(b))
        .take(64)
        .map(char::from)
        .collect();
    let banner = clean.trim().to_owned();
    if banner.len() < 8 {
        return None;
    }
    let lower = banner.to_ascii_lowercase();
    let os = if lower.contains("windows") {
        Some(HostOs::Windows)
    } else if [
        "ubuntu", "debian", "linux", "centos", "rhel", "fedora", "arch", "alpine", "suse",
        "raspbian", "rocky", "alma",
    ]
    .iter()
    .any(|token| lower.contains(token))
    {
        Some(HostOs::Linux)
    } else {
        None
    };
    Some(SshInfo { banner, os })
}

fn parse_http_status_line(bytes: &[u8]) -> Option<u16> {
    let line = bytes.split(|&b| b == b'\n' || b == b'\r').next()?;
    let text = std::str::from_utf8(line).ok()?;
    let rest = text
        .strip_prefix("HTTP/1.1 ")
        .or_else(|| text.strip_prefix("HTTP/1.0 "))?;
    let code: u16 = rest.get(..3)?.parse().ok()?;
    (100..=599).contains(&code).then_some(code)
}

fn tls_client_hello() -> Vec<u8> {
    let random: [u8; 32] = rand::random();
    let ciphers: [u8; 12] = [
        0x13, 0x01, 0x13, 0x02, 0xc0, 0x2f, 0xc0, 0x30, 0x00, 0x9c, 0x00, 0x9d,
    ];
    let exts: [u8; 31] = [
        // supported_versions: TLS 1.3 + TLS 1.2
        0x00, 0x2b, 0x00, 0x05, 0x04, 0x03, 0x04, 0x03, 0x03,
        // supported_groups: x25519, secp256r1
        0x00, 0x0a, 0x00, 0x06, 0x00, 0x04, 0x00, 0x1d, 0x00, 0x17,
        // signature_algorithms
        0x00, 0x0d, 0x00, 0x08, 0x00, 0x06, 0x04, 0x03, 0x08, 0x04, 0x04, 0x01,
    ];
    let body_len = 2 + 32 + 1 + 2 + ciphers.len() + 2 + 2 + exts.len();
    let hs_len = 4 + body_len;
    let mut out = Vec::with_capacity(5 + hs_len);
    out.extend_from_slice(&[0x16, 0x03, 0x01]);
    out.extend_from_slice(&(hs_len as u16).to_be_bytes());
    out.push(0x01); // ClientHello
    out.extend_from_slice(&[
        ((body_len >> 16) & 0xff) as u8,
        ((body_len >> 8) & 0xff) as u8,
        (body_len & 0xff) as u8,
    ]);
    out.extend_from_slice(&[0x03, 0x03]); // TLS 1.2 legacy version
    out.extend_from_slice(&random);
    out.push(0x00); // session_id len
    out.extend_from_slice(&(ciphers.len() as u16).to_be_bytes());
    out.extend_from_slice(&ciphers);
    out.extend_from_slice(&[0x01, 0x00]); // null compression
    out.extend_from_slice(&(exts.len() as u16).to_be_bytes());
    out.extend_from_slice(&exts);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{Network, Tun};
    use socket2::Socket;
    use std::io;
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
                    0x03, 0x00, 0x00, 0x13, 0x0e, 0xd0, 0x00, 0x00, 0x12, 0x34, 0x00, 0x02, 0x2f,
                    0x08, 0x00, 0x02, 0x00, 0x00, 0x00,
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
        assert_eq!(
            host(&p, Ipv4Addr::LOCALHOST, &[]).await,
            Err(Code::Invalid)
        );
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
        let (ssh_res, ssh_os) = probe_host_port_bounded(&p, Ipv4Addr::LOCALHOST, ssh_port, true).await;
        assert!(ssh_res.open && ssh_res.kind == ServiceKind::Ssh);
        assert_eq!(ssh_os, Some(HostOs::Linux));

        let (http_res, _) = probe_host_port_bounded(&p, Ipv4Addr::LOCALHOST, http_port, true).await;
        assert!(http_res.open && http_res.kind == ServiceKind::Http);
        assert_eq!(http_res.http_status, Some(200));
        // Pure TCP connect latency must not include the 240ms passive SSH banner wait.
        assert!(http_res.rtt_ms.unwrap() < 150, "rtt={:?}", http_res.rtt_ms);

        let (tls_res, _) = probe_host_port_bounded(&p, Ipv4Addr::LOCALHOST, tls_port, true).await;
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
            probe_host_port_bounded(&p, Ipv4Addr::LOCALHOST, silent_port, true).await;
        assert!(silent_res.open && silent_res.kind == ServiceKind::Tcp);
    }
}
