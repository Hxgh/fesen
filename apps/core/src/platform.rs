use fesen_acl::Cidr;
use fesen_proto::{Code, DeviceOs};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, UdpSocket},
    time::Duration,
};
use tokio::{
    net::{TcpSocket, TcpStream},
    sync::mpsc,
    time::timeout,
};

#[cfg(target_os = "android")]
pub mod android;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
pub mod windows;

pub const MTU: usize = 1500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Network {
    pub marker: String,
    pub addresses: Vec<Ipv4Addr>,
    pub routes: Vec<Underlay>,
    pub dns: Vec<Ipv4Addr>,
}

impl Network {
    pub fn bind_index(&self, target: Option<Ipv4Addr>) -> u32 {
        self.routes
            .iter()
            .filter(|r| target.map_or(r.cidr.prefix == 0, |ip| r.cidr.range().contains(ip)))
            .max_by_key(|r| (r.cidr.prefix, std::cmp::Reverse(r.metric)))
            .map_or(0, |r| r.index)
    }

    pub fn known(&self) -> bool {
        !self.marker.is_empty() && !self.addresses.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Underlay {
    pub cidr: Cidr,
    pub index: u32,
    #[serde(default)]
    pub metric: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Tun {
    pub address: Option<Ipv4Addr>,
    pub dns: Option<Ipv4Addr>,
    pub routes: Vec<Cidr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub machine_id: String,
    pub os: DeviceOs,
    pub name: String,
}

pub fn hash_machine_id(os: DeviceOs, raw: &str) -> String {
    let tag = match os {
        DeviceOs::Macos => "macos",
        DeviceOs::Windows => "windows",
        DeviceOs::Android => "android",
        DeviceOs::Linux => "linux",
    };
    let digest = Sha256::digest(format!("fesen-v1:{tag}:{raw}").as_bytes());
    hex::encode(&digest[..16])
}

#[cfg(unix)]
pub(crate) fn unix_hostname() -> String {
    let mut buf = [0u8; 256];
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc == 0 {
        let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        if let Ok(s) = std::str::from_utf8(&buf[..len]) {
            let trimmed = s.trim().trim_end_matches(".local").trim();
            if !trimmed.is_empty() {
                return trimmed.chars().take(64).collect();
            }
        }
    }
    String::new()
}

pub trait Platform: Send + Sync + 'static {
    fn keep_empty_tun(&self) -> bool {
        false
    }
    fn access_ready(&self) -> bool {
        true
    }
    fn device_info(&self) -> DeviceInfo {
        #[cfg(unix)]
        {
            let name = unix_hostname();
            let raw = std::fs::read_to_string("/etc/machine-id")
                .or_else(|_| std::fs::read_to_string("/var/lib/dbus/machine-id"))
                .unwrap_or_else(|_| name.clone());
            let trimmed = raw.trim();
            let fallback_name = if name.is_empty() {
                "Linux".to_owned()
            } else {
                name
            };
            let id_seed = if trimmed.is_empty() {
                fallback_name.as_str()
            } else {
                trimmed
            };
            DeviceInfo {
                machine_id: hash_machine_id(DeviceOs::Linux, id_seed),
                os: DeviceOs::Linux,
                name: fallback_name,
            }
        }
        #[cfg(not(unix))]
        DeviceInfo {
            machine_id: hash_machine_id(DeviceOs::Linux, "unknown"),
            os: DeviceOs::Linux,
            name: "Device".into(),
        }
    }
    fn local_listeners(&self, ip: Ipv4Addr) -> Vec<(u16, Option<String>)> {
        let _ = ip;
        #[cfg(target_os = "linux")]
        {
            linux_proc_listeners(ip)
        }
        #[cfg(not(target_os = "linux"))]
        Vec::new()
    }
    fn network(&self) -> io::Result<Network>;
    fn protect(&self, socket: &Socket, target: Option<Ipv4Addr>) -> io::Result<()>;
    fn system_routing(&self) -> bool {
        true
    }
    fn alternate_udp(&self) -> Option<(UdpSocket, Vec<Ipv4Addr>)> {
        None
    }
    fn ipv6_udp(&self) -> Option<(UdpSocket, Vec<Ipv6Addr>)> {
        None
    }
    fn apply(&self, tun: &Tun) -> io::Result<()>;
    fn write_packet(&self, packet: &[u8]);
    fn set_input(&self, input: mpsc::Sender<Vec<u8>>);
}

#[cfg(target_os = "linux")]
fn linux_proc_listeners(target_ip: Ipv4Addr) -> Vec<(u16, Option<String>)> {
    let Ok(raw) = std::fs::read_to_string("/proc/net/tcp") else {
        return Vec::new();
    };
    let target_le = u32::from_le_bytes(target_ip.octets());
    let mut ports = std::collections::BTreeSet::new();
    for line in raw.lines().skip(1) {
        let cols: Vec<_> = line.split_whitespace().collect();
        if cols.len() < 4 || cols[3] != "0A" {
            continue;
        }
        let Some((ip_hex, port_hex)) = cols[1].split_once(':') else {
            continue;
        };
        let Ok(ip_val) = u32::from_str_radix(ip_hex, 16) else {
            continue;
        };
        let Ok(port) = u16::from_str_radix(port_hex, 16) else {
            continue;
        };
        if (ip_val == 0 || ip_val == target_le) && port > 0 && port < 49152 {
            ports.insert(port);
        }
    }
    ports.into_iter().take(24).map(|p| (p, None)).collect()
}

pub async fn tcp(platform: &dyn Platform, target: SocketAddrV4) -> Result<TcpStream, Code> {
    tcp_timeout(platform, target, Duration::from_secs(7)).await
}

pub async fn tcp_timeout(
    platform: &dyn Platform,
    target: SocketAddrV4,
    limit: Duration,
) -> Result<TcpStream, Code> {
    let raw =
        Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)).map_err(|_| Code::Internal)?;
    platform
        .protect(&raw, Some(*target.ip()))
        .map_err(|_| Code::TargetUnavailable)?;
    raw.set_nonblocking(true).map_err(|_| Code::Internal)?;
    let socket = TcpSocket::from_std_stream(raw.into());
    let conn = timeout(limit, socket.connect(SocketAddr::V4(target)))
        .await
        .map_err(|_| Code::TargetTimeout)?
        .map_err(|e| match e.kind() {
            io::ErrorKind::TimedOut => Code::TargetTimeout,
            _ => Code::TargetUnavailable,
        })?;
    let _ = conn.set_nodelay(true);
    Ok(conn)
}

pub fn udp(platform: &dyn Platform, target: Option<Ipv4Addr>) -> io::Result<UdpSocket> {
    datagram(|socket| platform.protect(socket, target))
}

pub(crate) fn datagram(protect: impl FnOnce(&Socket) -> io::Result<()>) -> io::Result<UdpSocket> {
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    protect(&sock)?;
    sock.set_nonblocking(true)?;
    sock.bind(&SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).into())?;
    Ok(sock.into())
}

#[cfg(any(target_os = "android", target_os = "macos", test))]
pub(crate) fn datagram6(protect: impl FnOnce(&Socket) -> io::Result<()>) -> io::Result<UdpSocket> {
    let sock = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_only_v6(true)?;
    protect(&sock)?;
    sock.set_nonblocking(true)?;
    sock.bind(&SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)).into())?;
    Ok(sock.into())
}
