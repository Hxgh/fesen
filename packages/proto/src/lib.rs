use fesen_acl::{Rules, VIRTUAL};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    net::{SocketAddrV4, SocketAddrV6},
    time::Duration,
};
use url::Url;

pub const PROTOCOL: u8 = 1;
pub const LEASE: Duration = Duration::from_secs(60);
pub const HEARTBEAT: Duration = Duration::from_secs(8);
pub const PRESENCE_TIMEOUT: Duration = Duration::from_secs(24);
pub const ALPN: &[u8] = b"fesen/1";
pub const CERT_NAME: &str = "peer.fesen";
pub const PROBE_PREFIX: &[u8] = b"fesen-probe/1:";
pub const MAX_FRAME: usize = 1024 * 1024;
pub const MAX_REQUEST: usize = 16 * 1024;
pub const MAX_PUBLICATIONS: usize = 256;
pub const MAX_DEVICES: usize = 128;
pub const MAX_CANDIDATES: usize = 8;
pub const MAX_IPV6_CANDIDATES: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    Offline,
    DirectFailed,
    Conflict,
    Unauthorized,
    TargetTimeout,
    TargetUnavailable,
    HttpInvalid,
    ProtocolMismatch,
    Invalid,
    Limit,
    NetworkChanged,
    NotConnected,
    Busy,
    Internal,
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Offline => "设备离线",
            Self::DirectFailed => "无法直连",
            Self::Conflict => "地址冲突",
            Self::Unauthorized => "未授权",
            Self::TargetTimeout => "目标超时",
            Self::TargetUnavailable => "目标不可达",
            Self::HttpInvalid => "HTTP 响应无效或不完整",
            Self::ProtocolMismatch => "目标协议不匹配",
            Self::Invalid => "输入无效",
            Self::Limit => "已达到组容量上限",
            Self::NetworkChanged => "网络已变更，共享已暂停",
            Self::NotConnected => "未连接服务器",
            Self::Busy => "设备忙",
            Self::Internal => "操作失败",
        })
    }
}
impl std::error::Error for Code {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceKind {
    Tcp,
    Http,
    Https,
    Ssh,
    Rdp,
    Smb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostOs {
    Linux,
    Windows,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Publication {
    pub id: String,
    pub name: String,
    pub rules: Rules,
    #[serde(default)]
    pub http: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ServiceKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_os: Option<HostOs>,
}

impl Publication {
    pub fn single_ip(&self) -> Option<std::net::Ipv4Addr> {
        match self.rules.ips.as_slice() {
            [r] if r.start == r.end => Some(r.start),
            _ => None,
        }
    }

    pub fn effective_kind(&self) -> ServiceKind {
        match self.kind {
            Some(kind) => kind,
            None if self.http => ServiceKind::Http,
            None => ServiceKind::Tcp,
        }
    }

    pub fn checked(mut self) -> Result<Self, Code> {
        if !valid_id(&self.id) || !valid_label(&self.name) {
            return Err(Code::Invalid);
        }
        self.rules = self.rules.checked().map_err(|_| Code::Invalid)?;
        if let Some(kind) = self.kind {
            self.http = kind == ServiceKind::Http;
        }
        if self.single_ip().is_none() {
            self.host_os = None;
        }
        Ok(self)
    }
}

pub fn valid_label(s: &str) -> bool {
    !s.trim().is_empty() && s.len() <= 96 && !s.chars().any(char::is_control)
}

pub fn valid_id(s: &str) -> bool {
    s.len() == 32
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

pub fn valid_username(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
}

pub fn valid_password(s: &str) -> bool {
    (8..=256).contains(&s.len())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offering {
    pub owner: String,
    pub member: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(flatten)]
    pub service: Publication,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    pub id: String,
    pub certificate: Vec<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct View {
    pub peers: Vec<Peer>,
    pub services: Vec<Offering>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Credential {
    Password {
        username: String,
        password: String,
        label: String,
    },
    Resume {
        token: String,
    },
}

#[derive(Serialize, Deserialize)]
pub struct Hello {
    pub version: u8,
    pub certificate: Vec<u8>,
    pub credential: Credential,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Call {
    Publish {
        service: Publication,
    },
    Unpublish {
        id: String,
    },
    Candidates {
        local: Vec<SocketAddrV4>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        extra: Vec<SocketAddrV4>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        ipv6: Vec<SocketAddrV6>,
    },
    Punch {
        peer: String,
    },
    Ping,
    Logout,
}

#[derive(Serialize, Deserialize)]
pub struct Request {
    pub seq: u64,
    #[serde(flatten)]
    pub call: Call,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    Welcome {
        device: String,
        member: String,
        token: Option<String>,
        probe: String,
        udp_port: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stun: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_admin: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        admin_token: Option<String>,
    },
    Result {
        seq: u64,
        error: Option<Code>,
    },
    View {
        view: View,
    },
    Punch {
        peer: String,
        candidates: Vec<SocketAddrV4>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        ipv6: Vec<SocketAddrV6>,
    },
    Rejected {
        error: Code,
    },
    Revoked,
}

#[derive(Serialize, Deserialize)]
pub struct Open {
    pub service: String,
    pub target: SocketAddrV4,
}

pub fn candidate(addr: SocketAddrV4) -> bool {
    let ip = *addr.ip();
    (1..224).contains(&ip.octets()[0])
        && !ip.is_loopback()
        && !VIRTUAL.contains(ip)
        && addr.port() != 0
}

pub fn candidate6(addr: SocketAddrV6) -> bool {
    addr.port() != 0 && addr.scope_id() == 0 && addr.ip().octets()[0] & 0xe0 == 0x20
}

pub fn stun_endpoint(value: &str) -> Option<(&str, u16)> {
    let (host, port) = value.rsplit_once(':')?;
    let port = port.parse::<u16>().ok()?;
    let name = host.strip_suffix('.').unwrap_or(host);
    if port == 0 || name.is_empty() || name.len() > 253 {
        return None;
    }
    name.split('.')
        .all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        .then_some((host, port))
}

pub fn origin(address: &str) -> Result<Url, Code> {
    let address = address.trim();
    let url = if address.starts_with("fesen://") {
        let link = Url::parse(address).map_err(|_| Code::Invalid)?;
        let fields: Vec<_> = link.query_pairs().collect();
        if link.host_str() != Some("join")
            || !matches!(link.path(), "" | "/")
            || !link.username().is_empty()
            || link.password().is_some()
            || link.port().is_some()
            || link.fragment().is_some()
            || fields.len() != 1
            || fields[0].0 != "server"
        {
            return Err(Code::Invalid);
        }
        Url::parse(&fields[0].1).map_err(|_| Code::Invalid)?
    } else {
        Url::parse(address).map_err(|_| Code::Invalid)?
    };
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1"));
    if !(url.scheme() == "https" || (loopback && url.scheme() == "http"))
        || url.host_str().is_none()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(Code::Invalid);
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ipv6_candidates_must_be_unscoped_global_unicast() {
        assert!(candidate6("[2001:db8::1]:4000".parse().unwrap()));
        for invalid in [
            "[::1]:4000",
            "[fe80::1]:4000",
            "[ff02::1]:4000",
            "[fc00::1]:4000",
            "[::ffff:192.0.2.1]:4000",
            "[2001:db8::1]:0",
        ] {
            assert!(!candidate6(invalid.parse().unwrap()));
        }
    }

    #[test]
    fn join_only_contains_origin() {
        assert_eq!(
            origin("fesen://join?server=https%3A%2F%2Fgroup.example")
                .unwrap()
                .as_str(),
            "https://group.example/"
        );
        assert!(origin("fesen://join?server=https://example.com&token=bad").is_err());
        assert!(origin("http://example.com").is_err());
        assert!(origin("https://guest@example.com").is_err());
        assert!(origin("http://127.0.0.1:8080").is_ok());
    }

    #[test]
    fn stun_requires_an_explicit_ipv4_or_dns_endpoint() {
        assert_eq!(
            stun_endpoint("stun.example:3478"),
            Some(("stun.example", 3478))
        );
        assert_eq!(stun_endpoint("127.0.0.1:3478"), Some(("127.0.0.1", 3478)));
        for bad in [
            "stun.example",
            "stun.example:0",
            "[::1]:3478",
            "https://stun.example:3478",
            "stun.example:3478/path",
            "user@stun.example:3478",
            "-stun.example:3478",
        ] {
            assert!(stun_endpoint(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn publication_normalizes_kind_http_and_single_host_os() {
        let single = Publication {
            id: "0123456789abcdef0123456789abcdef".into(),
            name: "host".into(),
            rules: Rules::parse("198.51.100.35", "22").unwrap(),
            http: true,
            kind: Some(ServiceKind::Ssh),
            host_os: Some(HostOs::Windows),
        }
        .checked()
        .unwrap();
        assert!(!single.http);
        assert_eq!(single.effective_kind(), ServiceKind::Ssh);
        assert_eq!(single.host_os, Some(HostOs::Windows));

        let http_kind = Publication {
            id: "0123456789abcdef0123456789abcdef".into(),
            name: "web".into(),
            rules: Rules::parse("198.51.100.35", "80").unwrap(),
            http: false,
            kind: Some(ServiceKind::Http),
            host_os: None,
        }
        .checked()
        .unwrap();
        assert!(http_kind.http);
        assert_eq!(http_kind.effective_kind(), ServiceKind::Http);

        let range = Publication {
            id: "0123456789abcdef0123456789abcdef".into(),
            name: "range".into(),
            rules: Rules::parse("198.51.100.0/29", "80").unwrap(),
            http: true,
            kind: None,
            host_os: Some(HostOs::Linux),
        }
        .checked()
        .unwrap();
        assert_eq!(range.host_os, None);
        assert_eq!(range.effective_kind(), ServiceKind::Http);
    }
}
