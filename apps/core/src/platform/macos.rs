use super::{DeviceInfo, Network, Platform, Tun, Underlay, datagram6, hash_machine_id, unix_hostname};
use crate::{Core, ipc};
use fesen_acl::{Cidr, VIRTUAL};
use fesen_proto::DeviceOs;
use socket2::Socket;
use std::{
    collections::HashMap,
    ffi::{CStr, c_char},
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, UdpSocket},
    os::fd::AsRawFd,
    path::PathBuf,
    process::Command,
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::mpsc;

type Callback = unsafe extern "C" fn(usize, *const u8, usize);
type Apply = unsafe extern "C" fn(usize, *const u8, usize) -> i32;
type Expired = unsafe extern "C" fn(usize);

const NET_RT_DUMP2: libc::c_int = 7;
const RTF_UP: libc::c_int = 0x1;
const RTF_WASCLONED: libc::c_int = 0x20000;
const RTF_IFSCOPE: libc::c_int = 0x1000000;
const RTA_DST: libc::c_int = 0x1;
const RTA_GATEWAY: libc::c_int = 0x2;
const RTA_NETMASK: libc::c_int = 0x4;

#[repr(C)]
#[derive(Clone, Copy)]
struct RtMsghdr2 {
    rtm_msglen: u16,
    rtm_version: u8,
    rtm_type: u8,
    rtm_index: u16,
    rtm_flags: libc::c_int,
    rtm_addrs: libc::c_int,
    rtm_refcnt: i32,
    rtm_parentflags: libc::c_int,
    rtm_reserved: libc::c_int,
    rtm_use: libc::c_int,
    rtm_inits: u32,
    rtm_rmx: [u32; 14],
}

struct TunnelIo {
    context: usize,
    adjust: Apply,
    output: Callback,
    input: OnceLock<mpsc::Sender<Vec<u8>>>,
}

struct Apple {
    tunnel: Option<TunnelIo>,
    state: RwLock<Network>,
    snapshot: Mutex<()>,
    our_ip: Mutex<Option<Ipv4Addr>>,
    physical_event: AtomicU64,
    device: OnceLock<DeviceInfo>,
}

impl Apple {
    fn new(tunnel: Option<TunnelIo>) -> Self {
        Self {
            tunnel,
            state: RwLock::new(Network::default()),
            snapshot: Mutex::new(()),
            our_ip: Mutex::new(None),
            physical_event: AtomicU64::new(0),
            device: OnceLock::new(),
        }
    }
}

fn hardware_uuid() -> Option<String> {
    let mut uuid = [0u8; 16];
    let ts = libc::timespec {
        tv_sec: 1,
        tv_nsec: 0,
    };
    let rc = unsafe { libc::gethostuuid(uuid.as_mut_ptr(), &ts) };
    if rc == 0 && uuid.iter().any(|&b| b != 0) {
        Some(hex::encode(uuid))
    } else {
        None
    }
}

fn computer_name() -> String {
    if let Ok(raw) = command("/usr/sbin/scutil", &["--get", "ComputerName"]) {
        let name = raw.trim();
        if !name.is_empty() {
            return name.chars().take(64).collect();
        }
    }
    let host = unix_hostname();
    if host.is_empty() { "Mac".into() } else { host }
}

fn command(program: &str, args: &[&str]) -> io::Result<String> {
    let output = Command::new(program).args(args).output()?;
    if !output.status.success() {
        return Err(io::Error::other("route snapshot failed"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn roundup_sa(sa_len: usize) -> usize {
    const ALIGN: usize = std::mem::size_of::<u32>();
    if sa_len == 0 {
        ALIGN
    } else {
        1 + ((sa_len - 1) | (ALIGN - 1))
    }
}

fn sysctl_ipv4_routes(
    own_if: Option<u32>,
    if_names: &HashMap<u32, String>,
) -> io::Result<(Vec<Underlay>, Vec<String>)> {
    let mut mib = [
        libc::CTL_NET,
        libc::PF_ROUTE,
        0,
        libc::AF_INET,
        NET_RT_DUMP2,
        0,
    ];
    let mut len: usize = 0;
    let mut buf = Vec::new();
    for _ in 0..3 {
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as libc::c_uint,
                std::ptr::null_mut(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        if len == 0 {
            return Ok((Vec::new(), Vec::new()));
        }
        // Pad slightly so concurrent route additions don't trigger ENOMEM.
        len = len.saturating_add(len / 8 + 4096);
        buf.resize(len, 0u8);
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as libc::c_uint,
                buf.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc == 0 {
            buf.truncate(len);
            break;
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::ENOMEM) {
            return Err(err);
        }
    }

    let hdr_size = std::mem::size_of::<RtMsghdr2>();
    let mut routes = Vec::with_capacity(64);
    let mut identity = Vec::new();
    let mut offset = 0usize;
    while offset + hdr_size <= buf.len() {
        let hdr = unsafe { std::ptr::read_unaligned(buf.as_ptr().add(offset).cast::<RtMsghdr2>()) };
        let msglen = usize::from(hdr.rtm_msglen);
        if msglen < hdr_size || offset + msglen > buf.len() {
            break;
        }
        let msg = &buf[offset..offset + msglen];
        offset += msglen;

        let index = u32::from(hdr.rtm_index);
        if index == 0
            || own_if == Some(index)
            || (hdr.rtm_flags & RTF_UP) == 0
            || (hdr.rtm_flags & RTF_WASCLONED) != 0
        {
            continue;
        }

        let mut cursor = hdr_size;
        let mut dst: Option<Ipv4Addr> = None;
        let mut gateway: Option<String> = None;
        let mut prefix: Option<u8> = None;

        for bit in [RTA_DST, RTA_GATEWAY, RTA_NETMASK] {
            if (hdr.rtm_addrs & bit) == 0 {
                continue;
            }
            if cursor >= msg.len() {
                break;
            }
            let sa_len = usize::from(msg[cursor]);
            let step = roundup_sa(sa_len);
            let end = cursor.saturating_add(sa_len).min(msg.len());
            let sa = &msg[cursor..end];
            cursor = cursor.saturating_add(step);

            match bit {
                RTA_DST => {
                    if sa.len() >= 8 && i32::from(sa[1]) == libc::AF_INET {
                        dst = Some(Ipv4Addr::new(sa[4], sa[5], sa[6], sa[7]));
                    }
                }
                RTA_GATEWAY => {
                    if sa.len() >= 8 && i32::from(sa[1]) == libc::AF_INET {
                        gateway = Some(Ipv4Addr::new(sa[4], sa[5], sa[6], sa[7]).to_string());
                    } else if sa.len() >= 4 && i32::from(sa[1]) == libc::AF_LINK {
                        let sdl_idx = u16::from_ne_bytes([sa[2], sa[3]]);
                        gateway = Some(format!("link#{sdl_idx}"));
                    }
                }
                RTA_NETMASK => {
                    if sa_len == 0 {
                        prefix = Some(0);
                    } else {
                        let mut mask = [0u8; 4];
                        let avail = sa.len().saturating_sub(4).min(4);
                        mask[..avail].copy_from_slice(&sa[4..4 + avail]);
                        let bits = u32::from_be_bytes(mask);
                        let ones = bits.leading_ones();
                        if bits.count_ones() == ones {
                            prefix = Some(ones as u8);
                        }
                    }
                }
                _ => {}
            }
        }

        let Some(dst_ip) = dst else { continue };
        let prefix = prefix.unwrap_or(32);
        let Ok(cidr) = Cidr::new(dst_ip, prefix) else {
            continue;
        };
        if cidr.prefix == 0 {
            let if_name = if_names.get(&index).cloned().unwrap_or_else(|| {
                let mut name_buf = [0 as c_char; libc::IFNAMSIZ];
                let ptr = unsafe { libc::if_indextoname(index, name_buf.as_mut_ptr()) };
                if ptr.is_null() {
                    String::new()
                } else {
                    unsafe { CStr::from_ptr(ptr) }
                        .to_string_lossy()
                        .into_owned()
                }
            });
            if !if_name.is_empty() && !if_name.starts_with("utun")
                && let Some(gw) = gateway
            {
                identity.push(format!("{if_name}:{gw}"));
            }
        }
        routes.push(Underlay {
            cidr,
            index,
            metric: if (hdr.rtm_flags & RTF_IFSCOPE) != 0 {
                1
            } else {
                0
            },
        });
    }
    Ok((routes, identity))
}

fn interfaces() -> io::Result<Vec<(String, IpAddr, u32)>> {
    unsafe {
        let mut list = std::ptr::null_mut();
        if libc::getifaddrs(&mut list) != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut indices: HashMap<String, u32> = HashMap::new();
        let mut position = list;
        while let Some(row) = position.as_ref() {
            position = row.ifa_next;
            if row.ifa_addr.is_null() || (*row.ifa_addr).sa_family as i32 != libc::AF_LINK {
                continue;
            }
            let dl = &*row.ifa_addr.cast::<libc::sockaddr_dl>();
            let idx = u32::from(dl.sdl_index);
            if idx != 0 {
                let name = CStr::from_ptr(row.ifa_name).to_string_lossy().into_owned();
                indices.insert(name, idx);
            }
        }
        let mut position = list;
        let mut out = Vec::new();
        while let Some(row) = position.as_ref() {
            position = row.ifa_next;
            if row.ifa_addr.is_null() || row.ifa_flags & (libc::IFF_UP as u32) == 0 {
                continue;
            }
            let addr = match (*row.ifa_addr).sa_family as i32 {
                libc::AF_INET => {
                    let ip = &*row.ifa_addr.cast::<libc::sockaddr_in>();
                    IpAddr::V4(Ipv4Addr::from(u32::from_be(ip.sin_addr.s_addr)))
                }
                libc::AF_INET6 => {
                    let ip = &*row.ifa_addr.cast::<libc::sockaddr_in6>();
                    IpAddr::V6(Ipv6Addr::from(ip.sin6_addr.s6_addr))
                }
                _ => continue,
            };
            let name = CStr::from_ptr(row.ifa_name).to_string_lossy().into_owned();
            let idx = *indices
                .entry(name.clone())
                .or_insert_with(|| libc::if_nametoindex(row.ifa_name));
            out.push((name, addr, idx));
        }
        libc::freeifaddrs(list);
        Ok(out)
    }
}

fn bind_interface(socket: &Socket, index: u32, level: i32, option: i32) -> io::Result<()> {
    if index == 0 {
        return Err(io::Error::other("no underlay"));
    }
    let result = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            level,
            option,
            (&index as *const u32).cast(),
            std::mem::size_of_val(&index) as libc::socklen_t,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

impl Platform for Apple {
    fn system_routing(&self) -> bool {
        self.tunnel.is_some()
    }

    fn device_info(&self) -> DeviceInfo {
        self.device
            .get_or_init(|| {
                let name = computer_name();
                let seed = hardware_uuid().unwrap_or_else(|| name.clone());
                DeviceInfo {
                    machine_id: hash_machine_id(DeviceOs::Macos, &seed),
                    os: DeviceOs::Macos,
                    name,
                }
            })
            .clone()
    }

    fn local_listeners(&self, target_ip: Ipv4Addr) -> Vec<(u16, Option<String>)> {
        let Ok(out) = command("/usr/sbin/lsof", &["-nP", "+c", "0", "-i4TCP", "-sTCP:LISTEN"])
        else {
            return Vec::new();
        };
        let target_str = target_ip.to_string();
        let mut found = std::collections::BTreeMap::<u16, Option<String>>::new();
        for line in out.lines().skip(1) {
            let cols: Vec<_> = line.split_whitespace().collect();
            if cols.len() < 9 {
                continue;
            }
            let proc_name = cols[0].replace("\\x20", " ");
            let name_col = cols[cols.len() - 2];
            let Some((host, port_str)) = name_col.rsplit_once(':') else {
                continue;
            };
            if host != "*" && host != target_str {
                continue;
            }
            let Ok(port) = port_str.parse::<u16>() else {
                continue;
            };
            if port > 0 && port < 49152 {
                found.entry(port).or_insert_with(|| {
                    (!proc_name.is_empty()).then(|| proc_name.chars().take(24).collect())
                });
            }
        }
        found.into_iter().take(24).collect()
    }

    fn network(&self) -> io::Result<Network> {
        let _lock = self.snapshot.lock().unwrap();
        let own_ip = *self.our_ip.lock().unwrap();
        let ifaces = interfaces()?;
        let own_if = ifaces
            .iter()
            .find(|(_, ip, _)| Some(*ip) == own_ip.map(IpAddr::V4))
            .map(|entry| entry.2);
        let mut if_names = HashMap::with_capacity(ifaces.len());
        for (name, _, idx) in &ifaces {
            if *idx != 0 {
                if_names.entry(*idx).or_insert_with(|| name.clone());
            }
        }
        let (mut routes, mut identity) = sysctl_ipv4_routes(own_if, &if_names)?;
        // Some user-space VPNs add scoped routes without marking them as cloned.
        routes.sort_unstable_by_key(|r| (r.cidr.ip, r.cidr.prefix, r.metric, r.index));
        routes.dedup();
        identity.sort_unstable();
        let mut addresses: Vec<_> = ifaces
            .into_iter()
            .filter_map(|(_, ip, _)| {
                if let IpAddr::V4(ip) = ip {
                    Some(ip)
                } else {
                    None
                }
            })
            .filter(|ip| !ip.is_loopback() && !VIRTUAL.contains(*ip) && !ip.is_link_local())
            .collect();
        addresses.sort_unstable();
        addresses.dedup();
        let known = !identity.is_empty() && !addresses.is_empty();
        let net = Network {
            marker: if known {
                format!(
                    "{identity:?}/{}",
                    self.physical_event.load(Ordering::Acquire)
                )
            } else {
                String::new()
            },
            addresses,
            routes,
            dns: vec![],
        };
        *self.state.write().unwrap() = net.clone();
        Ok(net)
    }

    fn protect(&self, socket: &Socket, target: Option<Ipv4Addr>) -> io::Result<()> {
        // The publisher does not install routes of its own to protect against.
        if self.tunnel.is_none() {
            return Ok(());
        }
        let state = self.state.read().unwrap();
        let index = if target.is_some_and(|ip| ip.is_loopback() || state.addresses.contains(&ip)) {
            unsafe { libc::if_nametoindex(c"lo0".as_ptr()) }
        } else {
            state.bind_index(target)
        };
        bind_interface(socket, index, libc::IPPROTO_IP, libc::IP_BOUND_IF)
    }

    fn ipv6_udp(&self) -> Option<(UdpSocket, Vec<Ipv6Addr>)> {
        let index = self.state.read().unwrap().bind_index(None);
        let addresses: Vec<_> = interfaces()
            .ok()?
            .into_iter()
            .filter_map(|(name, ip, i)| match ip {
                IpAddr::V6(ip) if i == index && !name.starts_with("utun") => Some(ip),
                _ => None,
            })
            .collect();
        if addresses.is_empty() {
            return None;
        }
        let socket =
            datagram6(|sock| bind_interface(sock, index, libc::IPPROTO_IPV6, libc::IPV6_BOUND_IF))
                .ok()?;
        Some((socket, addresses))
    }

    fn apply(&self, tun: &Tun) -> io::Result<()> {
        let tunnel = self
            .tunnel
            .as_ref()
            .ok_or_else(|| io::Error::other("no tunnel"))?;
        let _lock = self.snapshot.lock().unwrap();
        let old = std::mem::replace(&mut *self.our_ip.lock().unwrap(), tun.address);
        let data = serde_json::to_vec(tun).map_err(io::Error::other)?;
        let result = unsafe { (tunnel.adjust)(tunnel.context, data.as_ptr(), data.len()) };
        if result != 0 {
            return Ok(());
        }
        *self.our_ip.lock().unwrap() = old;
        Err(io::Error::other("Network Extension route update failed"))
    }

    fn write_packet(&self, packet: &[u8]) {
        if let Some(tunnel) = &self.tunnel {
            unsafe { (tunnel.output)(tunnel.context, packet.as_ptr(), packet.len()) }
        }
    }

    fn set_input(&self, input: mpsc::Sender<Vec<u8>>) {
        if let Some(tunnel) = &self.tunnel {
            let _ = tunnel.input.set(input);
        }
    }
}

fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
}

pub fn publisher_dir() -> io::Result<PathBuf> {
    std::env::home_dir()
        .filter(|p| p.is_absolute())
        .map(|p| p.join("Library/Application Support/io.fesen.client/publisher"))
        .ok_or_else(|| io::Error::other("home directory unavailable"))
}

struct Publisher {
    host: Arc<Apple>,
    core: Arc<Core>,
}

static PUBLISHER: OnceLock<Publisher> = OnceLock::new();
static STOP_PUBLISHER: AtomicBool = AtomicBool::new(false);

fn serve_publisher() -> io::Result<()> {
    let dir = publisher_dir()?;
    let runtime = runtime()?;
    let result = runtime.block_on(async {
        let endpoint = ipc::Endpoint::bind(&dir).await?;
        let host = Arc::new(Apple::new(None));
        let core = Core::new(dir, host.clone(), false)?;
        PUBLISHER
            .set(Publisher {
                host,
                core: core.clone(),
            })
            .map_err(|_| io::Error::other("publisher already started"))?;
        if STOP_PUBLISHER.load(Ordering::Acquire) {
            core.stop();
        }
        endpoint.serve(core).await;
        Ok(())
    });
    runtime.shutdown_timeout(Duration::from_secs(4));
    result
}

#[unsafe(no_mangle)]
pub extern "C" fn ph_publisher_run() {
    let _ = serve_publisher();
}

#[unsafe(no_mangle)]
pub extern "C" fn ph_publisher_changed() {
    if let Some(p) = PUBLISHER.get() {
        p.host.physical_event.fetch_add(1, Ordering::Release);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ph_publisher_stop() {
    STOP_PUBLISHER.store(true, Ordering::Release);
    if let Some(p) = PUBLISHER.get() {
        p.core.stop();
    }
}

pub struct Engine {
    core: Arc<Core>,
    host: Arc<Apple>,
    runtime: tokio::runtime::Runtime,
}

/// # Safety
/// The path is a NUL-terminated string. Callbacks and their context must remain valid until stop returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ph_core_start(
    path: *const c_char,
    context: usize,
    adjust: Apply,
    output: Callback,
    expired: Expired,
) -> *mut Engine {
    if path.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(path) = (unsafe { CStr::from_ptr(path) }).to_str() else {
        return std::ptr::null_mut();
    };
    let host = Arc::new(Apple::new(Some(TunnelIo {
        context,
        adjust,
        output,
        input: OnceLock::new(),
    })));
    let Ok(runtime) = runtime() else {
        return std::ptr::null_mut();
    };
    let core = {
        let _enter = runtime.enter();
        match Core::new(PathBuf::from(path), host.clone(), false) {
            Ok(core) => core,
            Err(_) => return std::ptr::null_mut(),
        }
    };
    let watched = core.clone();
    runtime.spawn(async move {
        watched.wait_stopped().await;
        unsafe { expired(context) };
    });
    Box::into_raw(Box::new(Engine {
        core,
        host,
        runtime,
    }))
}

/// # Safety
/// Engine must be live. Bytes must be readable for len bytes; reply must retain its context until called.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ph_core_call(
    engine: *mut Engine,
    bytes: *const u8,
    len: usize,
    context: usize,
    reply: Callback,
) {
    if engine.is_null() || bytes.is_null() || len > 65536 {
        unsafe { reply(context, b"{}".as_ptr(), 2) };
        return;
    }
    let engine = unsafe { &*engine };
    let data = unsafe { std::slice::from_raw_parts(bytes, len) }.to_vec();
    let core = engine.core.clone();
    engine.runtime.spawn(async move {
        let bytes = ipc::dispatch_bytes(&core, &data).await;
        unsafe { reply(context, bytes.as_ptr(), bytes.len()) };
    });
}

/// # Safety
/// Engine must be live and bytes readable for len bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ph_core_packet(engine: *mut Engine, bytes: *const u8, len: usize) {
    if engine.is_null() || bytes.is_null() || !(20..=super::MTU).contains(&len) {
        return;
    }
    let e = unsafe { &*engine };
    if let Some(tunnel) = &e.host.tunnel
        && let Some(tx) = tunnel.input.get()
    {
        let _ = tx.try_send(unsafe { std::slice::from_raw_parts(bytes, len) }.to_vec());
    }
}

/// # Safety
/// Engine must be live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ph_core_changed(engine: *mut Engine) {
    if let Some(e) = unsafe { engine.as_ref() } {
        e.host.physical_event.fetch_add(1, Ordering::Release);
    }
}

/// # Safety
/// Stop once after all foreign calls into this engine have returned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ph_core_stop(engine: *mut Engine) {
    if engine.is_null() {
        return;
    }
    let e = unsafe { Box::from_raw(engine) };
    e.core.stop();
    e.runtime.block_on(e.core.wait_stopped());
    e.runtime.shutdown_timeout(Duration::from_secs(4));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_netstat_cidr(destination: &str) -> Option<Cidr> {
        if destination == "default" {
            return Cidr::new(Ipv4Addr::UNSPECIFIED, 0).ok();
        }
        let (ip, prefix) = if let Some((ip, prefix)) = destination.split_once('/') {
            (ip, Some(prefix.parse().ok()?))
        } else {
            (destination, None)
        };
        let mut octets = [0u8; 4];
        let mut length = 0;
        for (i, v) in ip.split('.').enumerate() {
            *octets.get_mut(i)? = v.parse().ok()?;
            length += 1;
        }
        Cidr::new(octets.into(), prefix.unwrap_or(8 * length)).ok()
    }

    #[test]
    fn sysctl_route_snapshot_matches_live_macos_network() {
        let apple = Apple::new(None);
        let t0 = std::time::Instant::now();
        let net = apple.network().unwrap();
        let elapsed = t0.elapsed();

        let table = command("/usr/sbin/netstat", &["-rn", "-f", "inet"]).unwrap();
        let mut expected_routes = Vec::new();
        let mut expected_identity = Vec::new();
        for line in table.lines() {
            let row: Vec<_> = line.split_whitespace().collect();
            if row.len() < 4 || row[2].contains('W') || !row[2].contains('U') {
                continue;
            }
            let Some(cidr) = parse_netstat_cidr(row[0]) else {
                continue;
            };
            let name = std::ffi::CString::new(row[3]).unwrap();
            let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
            if index == 0 {
                continue;
            }
            if cidr.prefix == 0 && !row[3].starts_with("utun") {
                expected_identity.push(format!("{}:{}", row[3], row[1]));
            }
            expected_routes.push(Underlay {
                cidr,
                index,
                metric: if row[2].contains('I') { 1 } else { 0 },
            });
        }
        expected_routes.sort_unstable_by_key(|r| (r.cidr.ip, r.cidr.prefix, r.metric, r.index));
        expected_routes.dedup();
        expected_identity.sort_unstable();

        let expected_marker = if !expected_identity.is_empty() && !net.addresses.is_empty() {
            format!("{expected_identity:?}/0")
        } else {
            String::new()
        };
        eprintln!(
            "sysctl_route_snapshot elapsed={elapsed:?}, routes={} (netstat={})",
            net.routes.len(),
            expected_routes.len()
        );
        assert_eq!(net.marker, expected_marker);
        assert_eq!(net.routes, expected_routes);
        assert!(
            elapsed < Duration::from_millis(50),
            "sysctl network snapshot took too long: {elapsed:?}"
        );
    }
}
