mod host;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub use host::host;
pub use host::{host_with_mode, ports};

use fesen_proto::{Code, HostOs, ServiceKind};
use http_body_util::{BodyExt, Empty};
use hyper::{Request, body::Bytes, client::conn::http1};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    time::timeout,
};

pub const DEFAULT_HOST_PORTS: [u16; 5] = [22, 3389, 445, 80, 443];
pub const COMMON_HOST_PORTS: [u16; 33] = [
    21, 22, 80, 443, 445, 1080, 1433, 3000, 3001, 3306, 3389, 4000, 5000, 5173, 5432, 5900, 6379,
    7000, 7890, 8000, 8008, 8080, 8081, 8088, 8090, 8100, 8443, 8888, 9000, 9001, 9090, 9200,
    27017,
];
pub const MAX_EXTRA_PORTS: usize = 5;
pub const MAX_HOST_PROBE_PORTS: usize = 24;

// Well-known non-HTTP binary/database/mail/proxy ports: never send synthetic HTTP HEAD noise to them.
const NON_HTTP_PORTS: [u16; 20] = [
    21, 25, 53, 110, 143, 465, 587, 993, 995, 1080, 1433, 1521, 3306, 5432, 5672, 5900, 6379, 9092,
    11211, 27017,
];

// Noisy OS RPC/background ports skipped during automatic deep sweeps (still addable manually).
const NOISY_OS_PORTS: [u16; 6] = [135, 137, 138, 139, 5040, 7680];

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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cancelled: bool,
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
        Some((buf, n))
    })
    .await;
    match res {
        Ok(Some((_, 0))) => HeadOutcome::ClosedOrAlert,
        Ok(Some((buf, n))) => {
            let bytes = &buf[..n];
            if let Some(code) = parse_http_status_line(bytes) {
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
