use super::{DeviceInfo, Network, Platform, Tun, Underlay, datagram6, hash_machine_id};
use crate::{Command as CoreCommand, Core};
use fesen_acl::{Cidr, VIRTUAL};
use fesen_proto::DeviceOs;
use sha2::{Digest, Sha256};
use socket2::Socket;
use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    ffi::{OsStr, OsString},
    fs, io,
    net::{Ipv4Addr, Ipv6Addr, UdpSocket},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawSocket,
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};
use tokio::{runtime::Handle, sync::mpsc};
use windows_sys::Win32::{
    Foundation::{ERROR_CANCELLED, GetLastError, HWND, LPARAM, LRESULT, POINT, WPARAM},
    Globalization::GetUserDefaultUILanguage,
    Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
        DeleteObject, GetDC, ReleaseDC,
    },
    NetworkManagement::{
        IpHelper::*,
        Ndis::IfOperStatusUp,
    },
    Networking::WinSock::*,
    Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    },
    System::{
        Com::CoTaskMemFree,
        LibraryLoader::GetModuleHandleW,
        Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW},
        SystemInformation::{
            ComputerNameDnsHostname, GetComputerNameExW, GetSystemDirectoryW,
            GetWindowsDirectoryW,
        },
        Threading::GetCurrentThreadId,
    },
    UI::{
        Shell::{
            FOLDERID_LocalAppData, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
            NIM_MODIFY, NOTIFYICONDATAW, SE_ERR_ACCESSDENIED, SHGetKnownFolderPath,
            Shell_NotifyIconW, ShellExecuteW,
        },
        WindowsAndMessaging::*,
    },
};

const NRPT: &str = "fesen .fesen resolver";
const NO_WINDOW: u32 = 0x0800_0000;
const TRAY_MESSAGE: u32 = WM_APP + 1;
const TRAY_STATE_MESSAGE: u32 = WM_APP + 2;
const WINTUN_SHA256: [u8; 32] = [
    0xe5, 0xda, 0x84, 0x47, 0xdc, 0x2c, 0x32, 0x0e, 0xdc, 0x0f, 0xc5, 0x2f, 0xa0, 0x18, 0x85,
    0xc1, 0x03, 0xde, 0x8c, 0x11, 0x84, 0x81, 0xf6, 0x83, 0x64, 0x3c, 0xac, 0xc3, 0x22, 0x0d,
    0xaf, 0xce,
];

pub fn data_dir() -> io::Result<PathBuf> {
    known_folder(&FOLDERID_LocalAppData).map(|p| shell_path(&p).unwrap_or(p).join("fesen"))
}

fn known_folder(id: &windows_sys::core::GUID) -> io::Result<PathBuf> {
    let mut raw = std::ptr::null_mut::<u16>();
    let result = unsafe {
        let result = SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut raw);
        let path = if result >= 0 && !raw.is_null() {
            let len = (0..).find(|i| *raw.add(*i) == 0).unwrap();
            Ok(PathBuf::from(OsString::from_wide(
                std::slice::from_raw_parts(raw, len),
            )))
        } else {
            Err(io::Error::from_raw_os_error(result))
        };
        CoTaskMemFree(raw.cast());
        path
    };
    result
}

pub fn pin_profile(dir: &Path) -> io::Result<Vec<fs::File>> {
    let mut paths: Vec<_> = dir.ancestors().collect();
    paths.reverse();
    let mut handles = Vec::new();
    for path in paths {
        if path.as_os_str().is_empty() {
            continue;
        }
        if path == dir {
            match fs::create_dir(dir) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        let share = if path == dir {
            FILE_SHARE_READ | FILE_SHARE_WRITE
        } else {
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
        };
        let handle = fs::OpenOptions::new()
            .read(true)
            .share_mode(share)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let meta = handle.metadata()?;
        if !meta.is_dir() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::other("unexpected profile location"));
        }
        handles.push(handle);
    }
    Ok(handles)
}

fn os_dir(read: unsafe extern "system" fn(*mut u16, u32) -> u32) -> io::Result<PathBuf> {
    let mut out = [0u16; 1024];
    let size = unsafe { read(out.as_mut_ptr(), out.len() as u32) } as usize;
    if (1..out.len()).contains(&size) {
        Ok(OsString::from_wide(&out[..size]).into())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElevateError {
    Location,
    Missing,
    Denied,
    Failed,
}

impl ElevateError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Location => "core_location",
            Self::Missing => "core_missing",
            Self::Denied => "uac_denied",
            Self::Failed => "not_connected",
        }
    }
}

fn check_regular_binary(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| {
        meta.is_file()
            && !meta.file_type().is_symlink()
            && (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) == 0
    })
}

fn verify_wintun(dll: &Path) -> io::Result<()> {
    let bytes = fs::read(dll)?;
    if Sha256::digest(&bytes).as_slice() != WINTUN_SHA256 {
        return Err(io::Error::other("wintun checksum mismatch"));
    }
    Ok(())
}

pub fn verify_core(core_exe: &Path) -> Result<(PathBuf, PathBuf), ElevateError> {
    let dll = core_exe.with_file_name("wintun.dll");
    if !check_regular_binary(core_exe) || !check_regular_binary(&dll) {
        return Err(ElevateError::Missing);
    }
    let launch_core = shell_path(core_exe).map_err(|_| ElevateError::Location)?;
    let launch_dll = shell_path(&dll).map_err(|_| ElevateError::Location)?;
    verify_wintun(&launch_dll).map_err(|_| ElevateError::Missing)?;
    Ok((launch_core, launch_dll))
}

fn check(code: u32) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code as i32))
    }
}

fn address(ip: Ipv4Addr) -> SOCKADDR_INET {
    SOCKADDR_INET {
        Ipv4: SOCKADDR_IN {
            sin_family: AF_INET,
            sin_addr: IN_ADDR {
                S_un: IN_ADDR_0 {
                    S_addr: u32::from_ne_bytes(ip.octets()),
                },
            },
            ..SOCKADDR_IN::default()
        },
    }
}

fn ipv4(address: SOCKADDR_INET) -> Ipv4Addr {
    Ipv4Addr::from(unsafe { address.Ipv4.sin_addr.S_un.S_addr }.to_ne_bytes())
}

fn route_row(index: u32, cidr: Cidr) -> MIB_IPFORWARD_ROW2 {
    let mut route = MIB_IPFORWARD_ROW2::default();
    unsafe {
        InitializeIpForwardEntry(&mut route);
    }
    route.InterfaceIndex = index;
    route.DestinationPrefix = IP_ADDRESS_PREFIX {
        Prefix: address(cidr.ip),
        PrefixLength: cidr.prefix,
    };
    route.NextHop = address(Ipv4Addr::UNSPECIFIED);
    route.Metric = 0;
    route.Protocol = MIB_IPPROTO_NETMGMT;
    route
}

fn nrpt(dns: Option<Ipv4Addr>) -> io::Result<()> {
    let power = os_dir(GetSystemDirectoryW)?.join("WindowsPowerShell/v1.0/powershell.exe");
    let mut script = format!(
        "$ErrorActionPreference='Stop'; Get-DnsClientNrptRule | Where-Object {{ $_.Comment -eq '{NRPT}' }} | Remove-DnsClientNrptRule -Force;"
    );
    if let Some(dns) = dns {
        script.push_str(&format!("Add-DnsClientNrptRule -Namespace '.fesen' -NameServers '{dns}' -DisplayName 'Fesen' -Comment '{NRPT}' | Out-Null;"));
    }
    script.push_str("Clear-DnsClientCache -ErrorAction SilentlyContinue | Out-Null;");
    let status = Command::new(power)
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(NO_WINDOW)
        .output()?
        .status;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("split DNS rule failed"))
    }
}

struct Adapter {
    session: Arc<wintun::Session>,
    reader: Option<std::thread::JoinHandle<()>>,
    index: u32,
    config: Tun,
    nrpt_lock: Arc<Mutex<bool>>,
}

impl Drop for Adapter {
    fn drop(&mut self) {
        for route in &self.config.routes {
            unsafe {
                DeleteIpForwardEntry2(&route_row(self.index, *route));
            }
        }
        if self.config.dns.is_some() {
            let mut cleaned = self.nrpt_lock.lock().unwrap();
            let _ = nrpt(None);
            *cleaned = true;
        }
        let _ = self.session.shutdown();
        if let Some(t) = self.reader.take() {
            let _ = t.join();
        }
    }
}

pub struct Windows {
    adapter: RwLock<Option<Adapter>>,
    own_index: AtomicU32,
    input: OnceLock<mpsc::Sender<Vec<u8>>>,
    dll: PathBuf,
    network: RwLock<Network>,
    device: OnceLock<DeviceInfo>,
    nrpt_lock: Arc<Mutex<bool>>,
}

impl Windows {
    pub fn new(dll: PathBuf) -> Self {
        let nrpt_lock = Arc::new(Mutex::new(false));
        let cleanup_lock = nrpt_lock.clone();
        let _ = std::thread::Builder::new()
            .name("fesen-nrpt-init".into())
            .spawn(move || {
                let mut cleaned = cleanup_lock.lock().unwrap();
                if !*cleaned {
                    let _ = nrpt(None);
                    *cleaned = true;
                }
            });
        Self {
            adapter: RwLock::new(None),
            own_index: AtomicU32::new(0),
            input: OnceLock::new(),
            dll,
            network: RwLock::new(Network::default()),
            device: OnceLock::new(),
            nrpt_lock,
        }
    }

    fn adapter(&self, ip: Ipv4Addr) -> io::Result<Adapter> {
        verify_wintun(&self.dll)?;
        let network = unsafe { wintun::load_from_path(&self.dll) }.map_err(io::Error::other)?;
        let adapter = wintun::Adapter::create(&network, "Fesen", "Fesen", None)
            .map_err(io::Error::other)?;
        let index = adapter.get_adapter_index().map_err(io::Error::other)?;
        adapter.set_mtu(super::MTU).map_err(io::Error::other)?;
        let mut row = MIB_UNICASTIPADDRESS_ROW::default();
        unsafe {
            InitializeUnicastIpAddressEntry(&mut row);
        }
        row.InterfaceIndex = index;
        row.Address = address(ip);
        row.OnLinkPrefixLength = 32;
        check(unsafe { CreateUnicastIpAddressEntry(&row) })?;
        let session = Arc::new(
            adapter
                .start_session(4 * 1024 * 1024)
                .map_err(io::Error::other)?,
        );
        let source = session.clone();
        let input = self
            .input
            .get()
            .cloned()
            .ok_or_else(|| io::Error::other("packet input closed"))?;
        let reader = std::thread::Builder::new()
            .name("fesen-wintun".into())
            .spawn(move || {
                while let Ok(packet) = source.receive_blocking() {
                    let _ = input.try_send(packet.bytes().to_vec());
                }
            })?;
        Ok(Adapter {
            session,
            reader: Some(reader),
            index,
            config: Tun {
                address: Some(ip),
                ..Tun::default()
            },
            nrpt_lock: self.nrpt_lock.clone(),
        })
    }
}

fn windows_machine_guid() -> Option<String> {
    let subkey = wide("SOFTWARE\\Microsoft\\Cryptography");
    let value = wide("MachineGuid");
    let mut buf = [0u16; 128];
    let mut size = std::mem::size_of_val(&buf) as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if rc == 0 && size >= 2 {
        let len = (size as usize / 2).saturating_sub(1);
        let text = String::from_utf16_lossy(&buf[..len]);
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    None
}

fn windows_computer_name() -> String {
    let mut buf = [0u16; 256];
    let mut len = buf.len() as u32;
    let ok = unsafe { GetComputerNameExW(ComputerNameDnsHostname, buf.as_mut_ptr(), &mut len) };
    if ok != 0 && len > 0 {
        let s = String::from_utf16_lossy(&buf[..len as usize]);
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return trimmed.chars().take(64).collect();
        }
    }
    "Windows".into()
}

impl Platform for Windows {
    fn device_info(&self) -> DeviceInfo {
        self.device
            .get_or_init(|| {
                let name = windows_computer_name();
                let seed = windows_machine_guid().unwrap_or_else(|| name.clone());
                DeviceInfo {
                    machine_id: hash_machine_id(DeviceOs::Windows, &seed),
                    os: DeviceOs::Windows,
                    name,
                }
            })
            .clone()
    }

    fn local_listeners(&self, target_ip: Ipv4Addr) -> Vec<(u16, Option<String>)> {
        let Ok(sys) = os_dir(GetSystemDirectoryW) else {
            return Vec::new();
        };
        let Ok(output) = Command::new(sys.join("netstat.exe"))
            .args(["-ano", "-p", "tcp"])
            .creation_flags(NO_WINDOW)
            .output()
        else {
            return Vec::new();
        };
        if !output.status.success() {
            return Vec::new();
        }
        let target_str = target_ip.to_string();
        let text = String::from_utf8_lossy(&output.stdout);
        let mut found = std::collections::BTreeSet::new();
        for line in text.lines() {
            let cols: Vec<_> = line.split_whitespace().collect();
            if cols.len() < 4 || cols[0] != "TCP" || !cols[3].eq_ignore_ascii_case("LISTENING") {
                continue;
            }
            let Some((host, port_str)) = cols[1].rsplit_once(':') else {
                continue;
            };
            if host != "0.0.0.0" && host != target_str {
                continue;
            }
            let Ok(port) = port_str.parse::<u16>() else {
                continue;
            };
            if port > 0 && port < 49152 && !matches!(port, 135 | 139 | 5040 | 7680) {
                found.insert(port);
            }
        }
        found.into_iter().take(24).map(|p| (p, None)).collect()
    }

    fn network(&self) -> io::Result<Network> {
        let own = match self.own_index.load(Ordering::Acquire) {
            0 => None,
            idx => Some(idx),
        };
        let mut active_if = HashSet::new();
        let mut loopback_if = HashSet::new();
        let mut if_metrics = HashMap::new();
        let mut routes = Vec::new();
        let mut gateways = Vec::new();
        let mut ips = Vec::new();
        unsafe {
            let mut if_table = std::ptr::null_mut::<MIB_IF_TABLE2>();
            check(GetIfTable2(&mut if_table))?;
            for row in std::slice::from_raw_parts(
                (*if_table).Table.as_ptr(),
                (*if_table).NumEntries as usize,
            ) {
                if Some(row.InterfaceIndex) == own || row.OperStatus != IfOperStatusUp {
                    continue;
                }
                if row.Type == IF_TYPE_SOFTWARE_LOOPBACK {
                    loopback_if.insert(row.InterfaceIndex);
                } else {
                    active_if.insert(row.InterfaceIndex);
                }
            }
            FreeMibTable(if_table.cast());

            let mut ip_if_table = std::ptr::null_mut::<MIB_IPINTERFACE_TABLE>();
            if GetIpInterfaceTable(AF_INET, &mut ip_if_table) == 0 && !ip_if_table.is_null() {
                for row in std::slice::from_raw_parts(
                    (*ip_if_table).Table.as_ptr(),
                    (*ip_if_table).NumEntries as usize,
                ) {
                    if_metrics.insert(row.InterfaceIndex, row.Metric);
                }
                FreeMibTable(ip_if_table.cast());
            }

            let mut table = std::ptr::null_mut::<MIB_IPFORWARD_TABLE2>();
            check(GetIpForwardTable2(AF_INET, &mut table))?;
            for r in
                std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize)
            {
                if Some(r.InterfaceIndex) == own {
                    continue;
                }
                let is_active = active_if.contains(&r.InterfaceIndex);
                let is_loopback = loopback_if.contains(&r.InterfaceIndex);
                if !is_active && !is_loopback {
                    continue;
                }
                let prefix = r.DestinationPrefix;
                let Ok(cidr) = Cidr::new(ipv4(prefix.Prefix), prefix.PrefixLength) else {
                    continue;
                };
                if cidr.ip.is_multicast()
                    || cidr.ip == Ipv4Addr::BROADCAST
                    || (cidr.ip.is_link_local() && cidr.prefix >= 16)
                {
                    continue;
                }
                let metric = if_metrics
                    .get(&r.InterfaceIndex)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(r.Metric);
                routes.push(Underlay {
                    cidr,
                    index: r.InterfaceIndex,
                    metric,
                });
                if is_active && cidr.prefix == 0 {
                    gateways.push((r.InterfaceIndex, ipv4(r.NextHop)));
                }
            }
            FreeMibTable(table.cast());
            let mut table = std::ptr::null_mut::<MIB_UNICASTIPADDRESS_TABLE>();
            check(GetUnicastIpAddressTable(AF_INET, &mut table))?;
            for row in
                std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize)
            {
                let ip = ipv4(row.Address);
                if active_if.contains(&row.InterfaceIndex)
                    && !ip.is_loopback()
                    && !ip.is_link_local()
                    && !VIRTUAL.contains(ip)
                {
                    ips.push(ip);
                }
            }
            FreeMibTable(table.cast());
            let mut table = std::ptr::null_mut::<MIB_IPNET_TABLE2>();
            let mut macs = Vec::new();
            if GetIpNetTable2(AF_INET, &mut table) == 0 {
                for row in std::slice::from_raw_parts(
                    (*table).Table.as_ptr(),
                    (*table).NumEntries as usize,
                ) {
                    let ip = ipv4(row.Address);
                    if gateways.contains(&(row.InterfaceIndex, ip))
                        && (1..=32).contains(&row.PhysicalAddressLength)
                    {
                        macs.push((
                            row.InterfaceIndex,
                            ip,
                            hex::encode(&row.PhysicalAddress[..row.PhysicalAddressLength as usize]),
                        ));
                    }
                }
                FreeMibTable(table.cast());
            }
            gateways.sort_unstable();
            macs.sort_unstable();
            ips.sort_unstable();
            ips.dedup();
            routes.sort_unstable_by_key(|r| (r.cidr.ip, r.cidr.prefix, r.metric, r.index));
            routes.dedup();
            let marker = if gateways.is_empty() {
                String::new()
            } else {
                format!("{gateways:?}/{macs:?}")
            };
            let info = Network {
                routes,
                marker,
                addresses: ips,
                dns: vec![],
            };
            *self.network.write().unwrap() = info.clone();
            Ok(info)
        }
    }

    fn protect(&self, socket: &Socket, target: Option<Ipv4Addr>) -> io::Result<()> {
        let index = self.network.read().unwrap().bind_index(target);
        if index == 0 {
            return Err(io::Error::other("no underlay"));
        }
        let index = index.to_be();
        if unsafe {
            setsockopt(
                socket.as_raw_socket() as usize,
                IPPROTO_IP,
                IP_UNICAST_IF,
                (&index as *const u32).cast(),
                std::mem::size_of_val(&index) as i32,
            )
        } == 0
        {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }))
        }
    }

    fn ipv6_udp(&self) -> Option<(UdpSocket, Vec<Ipv6Addr>)> {
        let index = self.network.read().unwrap().bind_index(None);
        if index == 0 {
            return None;
        }
        let mut addresses = Vec::new();
        unsafe {
            let mut table = std::ptr::null_mut::<MIB_UNICASTIPADDRESS_TABLE>();
            if GetUnicastIpAddressTable(AF_INET6, &mut table) == 0 && !table.is_null() {
                for row in std::slice::from_raw_parts(
                    (*table).Table.as_ptr(),
                    (*table).NumEntries as usize,
                ) {
                    if row.InterfaceIndex != index || row.DadState != IpDadStatePreferred {
                        continue;
                    }
                    addresses.push(Ipv6Addr::from(row.Address.Ipv6.sin6_addr.u.Byte));
                }
                FreeMibTable(table.cast());
            }
        }
        if addresses.is_empty() {
            return None;
        }
        let socket = datagram6(|sock| {
            // WinSock IPV6_UNICAST_IF takes host byte order (unlike IP_UNICAST_IF).
            if unsafe {
                setsockopt(
                    sock.as_raw_socket() as usize,
                    IPPROTO_IPV6,
                    IPV6_UNICAST_IF,
                    (&index as *const u32).cast(),
                    std::mem::size_of_val(&index) as i32,
                )
            } == 0
            {
                Ok(())
            } else {
                Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }))
            }
        })
        .ok()?;
        Some((socket, addresses))
    }

    fn apply(&self, tun: &Tun) -> io::Result<()> {
        let mut active = self.adapter.write().unwrap();
        if active
            .as_ref()
            .is_some_and(|a| a.config.address != tun.address)
        {
            self.own_index.store(0, Ordering::Release);
            active.take();
        }
        let Some(ip) = tun.address else { return Ok(()) };
        if active.is_none() {
            let created = self.adapter(ip)?;
            self.own_index.store(created.index, Ordering::Release);
            *active = Some(created);
        }
        let adapter = active.as_mut().unwrap();
        let result = (|| {
            let desired: HashSet<Cidr> = tun.routes.iter().copied().collect();
            for route in &adapter.config.routes {
                if !desired.contains(route) {
                    unsafe {
                        DeleteIpForwardEntry2(&route_row(adapter.index, *route));
                    }
                }
            }
            adapter.config.routes.retain(|r| desired.contains(r));
            let mut installed: HashSet<Cidr> = adapter.config.routes.iter().copied().collect();
            for route in &tun.routes {
                if installed.insert(*route) {
                    check(unsafe { CreateIpForwardEntry2(&route_row(adapter.index, *route)) })?;
                    adapter.config.routes.push(*route);
                }
            }
            if adapter.config.dns != tun.dns {
                let mut cleaned = self.nrpt_lock.lock().unwrap();
                nrpt(tun.dns)?;
                *cleaned = true;
                adapter.config.dns = tun.dns;
            }
            Ok(())
        })();
        if result.is_err() {
            self.own_index.store(0, Ordering::Release);
            active.take();
        }
        result
    }

    fn write_packet(&self, bytes: &[u8]) {
        if let Some(adapter) = self.adapter.read().unwrap().as_ref()
            && let Ok(mut packet) = adapter.session.allocate_send_packet(bytes.len() as u16)
        {
            packet.bytes_mut().copy_from_slice(bytes);
            adapter.session.send_packet(packet);
        }
    }

    fn set_input(&self, input: mpsc::Sender<Vec<u8>>) {
        let _ = self.input.set(input);
    }
}

fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}

fn strip_verbatim_drive_units(units: &[u16]) -> Option<&[u16]> {
    // Strip `\\?\` only for local drive paths (`\\?\C:\...`); reject UNC/device namespaces.
    const PREFIX: [u16; 4] = [
        b'\\' as u16,
        b'\\' as u16,
        b'?' as u16,
        b'\\' as u16,
    ];
    if units.starts_with(&PREFIX) {
        let drive = *units.get(4)?;
        if units.get(5) == Some(&(b':' as u16))
            && units.get(6) == Some(&(b'\\' as u16))
            && u8::try_from(drive).is_ok_and(|b| b.is_ascii_alphabetic())
        {
            return Some(&units[4..]);
        }
        return None;
    }
    Some(units)
}

fn shell_path(path: &Path) -> io::Result<PathBuf> {
    let canonical = path.canonicalize()?;
    let units: Vec<u16> = canonical.as_os_str().encode_wide().collect();
    let trimmed = strip_verbatim_drive_units(&units)
        .ok_or_else(|| io::Error::other("unsupported core path"))?;
    Ok(PathBuf::from(OsString::from_wide(trimmed)))
}

pub fn elevate(path: &Path) -> Result<(), ElevateError> {
    // ShellExecuteW does not accept `\\?\` extended-length paths from Path::canonicalize().
    let (launch, _) = verify_core(path)?;
    let dir = launch.parent().map(wide);
    let verb = wide("runas");
    let file = wide(&launch);
    let code = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            dir.as_ref().map_or(std::ptr::null(), |d| d.as_ptr()),
            SW_HIDE,
        )
    } as isize;
    if code > 32 {
        Ok(())
    } else if code == SE_ERR_ACCESSDENIED as isize
        || unsafe { GetLastError() } == ERROR_CANCELLED
    {
        Err(ElevateError::Denied)
    } else {
        Err(ElevateError::Failed)
    }
}

unsafe fn light_taskbar() -> bool {
    let key = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
    let value = wide("SystemUsesLightTheme");
    let mut data = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    err == 0 && data != 0
}

// Bagua 32x32 monochrome tray icon:
// 2 = Gen ☶ (0b100, Life Gate / connected), 1 = Kan ☵ (0b010, connecting), 0 = Kun ☷ (0b000, Death Gate / idle).
unsafe fn gen_tray_icon(visual: u8, light: bool) -> (HICON, bool) {
    let (mask, alpha): (u8, u8) = match visual {
        2 => (0b100, 255),
        1 => (0b010, 215),
        _ => (0b000, if light { 140 } else { 132 }),
    };
    let (r, g, b) = if light {
        (17u8, 21u8, 19u8)
    } else {
        (248u8, 250u8, 252u8)
    };
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: 32,
            biHeight: -32,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..BITMAPINFOHEADER::default()
        },
        ..BITMAPINFO::default()
    };
    let mut bits = std::ptr::null_mut();
    let dc = unsafe { GetDC(std::ptr::null_mut()) };
    let color = unsafe {
        CreateDIBSection(
            dc,
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        )
    };
    if !dc.is_null() {
        unsafe { ReleaseDC(std::ptr::null_mut(), dc) };
    }
    if color.is_null() || bits.is_null() {
        if !color.is_null() {
            unsafe { DeleteObject(color) };
        }
        return (unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) }, false);
    }
    let pixels = unsafe { std::slice::from_raw_parts_mut(bits.cast::<u8>(), 32 * 32 * 4) };
    pixels.fill(0);
    let mut fill_rect = |x0: usize, y0: usize, x1: usize, y1: usize, a: u8| {
        let pm = |c: u8| ((u16::from(c) * u16::from(a) + 127) / 255) as u8;
        let bgra = [pm(b), pm(g), pm(r), a];
        for y in y0..y1 {
            for x in x0..x1 {
                let i = (y * 32 + x) * 4;
                pixels[i..i + 4].copy_from_slice(&bgra);
            }
        }
    };
    for (bit, y0, y1) in [(4u8, 4usize, 8usize), (2, 14, 18), (1, 24, 28)] {
        if (mask & bit) != 0 {
            fill_rect(3, y0, 29, y1, alpha);
        } else {
            fill_rect(3, y0, 13, y1, alpha);
            fill_rect(19, y0, 29, y1, alpha);
        }
    }
    let mask_bits = [0u8; 32 * 4];
    let hbm_mask = unsafe { CreateBitmap(32, 32, 1, 1, mask_bits.as_ptr().cast()) };
    let icon = if hbm_mask.is_null() {
        std::ptr::null_mut()
    } else {
        let ii = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: hbm_mask,
            hbmColor: color,
        };
        unsafe { CreateIconIndirect(&ii) }
    };
    if !hbm_mask.is_null() {
        unsafe { DeleteObject(hbm_mask) };
    }
    unsafe { DeleteObject(color) };
    if icon.is_null() {
        (unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) }, false)
    } else {
        (icon, true)
    }
}

fn core_visual_state(core: &Core) -> u8 {
    core.visual_state()
}

struct TrayState {
    core: Arc<Core>,
    runtime: Handle,
    gui: PathBuf,
    visual: Cell<u8>,
    icon: Cell<HICON>,
    custom_icon: Cell<bool>,
}

unsafe fn refresh_tray_icon(window: HWND, state: &TrayState, visual: u8) {
    state.visual.set(visual);
    let (next, owned) = unsafe { gen_tray_icon(visual, light_taskbar()) };
    let data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: 1,
        uFlags: NIF_ICON,
        hIcon: next,
        ..NOTIFYICONDATAW::default()
    };
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
    let old = state.icon.replace(next);
    if state.custom_icon.replace(owned) && !old.is_null() {
        unsafe { DestroyIcon(old) };
    }
}

unsafe extern "system" fn tray_window(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if message == WM_NCCREATE {
            let creator = &*(l as *const CREATESTRUCTW);
            SetWindowLongPtrW(window, GWLP_USERDATA, creator.lpCreateParams as isize);
        }
        if let Some(state) =
            (GetWindowLongPtrW(window, GWLP_USERDATA) as *const TrayState).as_ref()
        {
            if message == TRAY_STATE_MESSAGE {
                refresh_tray_icon(window, state, (w & 0xff) as u8);
                return 0;
            }
            if message == WM_SETTINGCHANGE {
                refresh_tray_icon(window, state, state.visual.get());
                return 0;
            }
            if message == TRAY_MESSAGE {
                let open = || {
                    if let Ok(root) = os_dir(GetWindowsDirectoryW) {
                        let _ = Command::new(root.join("explorer.exe"))
                            .arg(&state.gui)
                            .spawn();
                    }
                };
                match l as u32 {
                    WM_LBUTTONUP => open(),
                    WM_RBUTTONUP => {
                        let menu = CreatePopupMenu();
                        let chinese = GetUserDefaultUILanguage() & 0x03ff == 0x04;
                        let status = state.core.status();
                        let (show, toggle, quit) = if chinese {
                            (
                                "打开飞弦",
                                if status.online {
                                    "断开连接"
                                } else if status.running {
                                    "取消连接"
                                } else {
                                    "连接"
                                },
                                "退出",
                            )
                        } else {
                            (
                                "Open Fesen",
                                if status.online {
                                    "Disconnect"
                                } else if status.running {
                                    "Cancel connecting"
                                } else {
                                    "Connect"
                                },
                                "Quit",
                            )
                        };
                        AppendMenuW(menu, MF_STRING, 1, wide(show).as_ptr());
                        AppendMenuW(menu, MF_STRING, 2, wide(toggle).as_ptr());
                        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
                        AppendMenuW(menu, MF_STRING, 3, wide(quit).as_ptr());
                        let mut at = POINT::default();
                        GetCursorPos(&mut at);
                        SetForegroundWindow(window);
                        match TrackPopupMenu(
                            menu,
                            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                            at.x,
                            at.y,
                            0,
                            window,
                            std::ptr::null(),
                        ) {
                            1 => open(),
                            2 => {
                                let core = state.core.clone();
                                if status.online || status.running {
                                    state.runtime.spawn(async move {
                                        let _ = core.execute(CoreCommand::Stop).await;
                                    });
                                } else if status.member.is_some() {
                                    state.runtime.spawn(async move {
                                        let _ = core.execute(CoreCommand::Start).await;
                                    });
                                } else {
                                    open();
                                }
                            }
                            3 => state.core.stop(),
                            _ => {}
                        }
                        DestroyMenu(menu);
                    }
                    _ => {}
                }
            }
        }
        DefWindowProcW(window, message, w, l)
    }
}

pub async fn tray(core: Arc<Core>, gui: PathBuf) {
    let (ready, rx) = tokio::sync::oneshot::channel::<(u32, usize)>();
    let initial = core_visual_state(&core);
    let tray_core = core.clone();
    let runtime = Handle::current();
    let thread = std::thread::spawn(move || unsafe {
        let state = TrayState {
            core: tray_core,
            runtime,
            gui,
            visual: Cell::new(initial),
            icon: Cell::new(std::ptr::null_mut()),
            custom_icon: Cell::new(false),
        };
        let instance = GetModuleHandleW(std::ptr::null());
        let name = wide("fesen-core-menu");
        let class = WNDCLASSW {
            hInstance: instance,
            lpszClassName: name.as_ptr(),
            lpfnWndProc: Some(tray_window),
            ..WNDCLASSW::default()
        };
        if RegisterClassW(&class) == 0 {
            return;
        }
        let window = CreateWindowExW(
            0,
            name.as_ptr(),
            name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            (&state as *const TrayState).cast(),
        );
        if window.is_null() {
            return;
        }
        let mut msg = MSG::default();
        PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
        let (hicon, owned) = gen_tray_icon(initial, light_taskbar());
        state.icon.set(hicon);
        state.custom_icon.set(owned);
        let mut icon = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: window,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: TRAY_MESSAGE,
            hIcon: hicon,
            ..NOTIFYICONDATAW::default()
        };
        let chinese = GetUserDefaultUILanguage() & 0x03ff == 0x04;
        let tip = wide(if chinese { "飞弦" } else { "Fesen" });
        icon.szTip[..tip.len()].copy_from_slice(&tip);
        Shell_NotifyIconW(NIM_ADD, &icon);
        let _ = ready.send((GetCurrentThreadId(), window as usize));
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        Shell_NotifyIconW(NIM_DELETE, &icon);
        let old = state.icon.replace(std::ptr::null_mut());
        if state.custom_icon.replace(false) && !old.is_null() {
            DestroyIcon(old);
        }
        DestroyWindow(window);
    });
    if let Ok((id, hwnd)) = rx.await {
        let mut last = initial;
        let mut changes = core.subscribe_changes();
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let stopped = core.wait_stopped();
        tokio::pin!(stopped);
        loop {
            tokio::select! {
                () = &mut stopped => break,
                Ok(()) = changes.changed() => {}
                _ = tick.tick() => {}
            }
            let current = core_visual_state(&core);
            if current != last {
                last = current;
                unsafe { PostMessageW(hwnd as HWND, TRAY_STATE_MESSAGE, usize::from(current), 0) };
            }
        }
        unsafe {
            PostThreadMessageW(id, WM_QUIT, 0, 0);
        }
    }
    let _ = tokio::task::spawn_blocking(move || thread.join().is_ok()).await;
}

#[cfg(test)]
mod tests {
    use super::{ElevateError, strip_verbatim_drive_units, verify_core};
    use std::fs;
    use tempfile::tempdir;

    fn utf16(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn strip_verbatim_drive_units_accepts_only_drive_and_plain_paths() {
        assert_eq!(
            strip_verbatim_drive_units(&utf16(r"\\?\C:\Program Files\Fesen\fesen-core.exe")),
            Some(utf16(r"C:\Program Files\Fesen\fesen-core.exe").as_slice())
        );
        assert_eq!(
            strip_verbatim_drive_units(&utf16(r"C:\Program Files\Fesen\fesen-core.exe")),
            Some(utf16(r"C:\Program Files\Fesen\fesen-core.exe").as_slice())
        );
        assert_eq!(
            strip_verbatim_drive_units(&utf16(r"\\?\UNC\server\share\fesen-core.exe")),
            None
        );
        assert_eq!(
            strip_verbatim_drive_units(&utf16(r"\\?\Volume{1234}\Fesen\fesen-core.exe")),
            None
        );
    }

    #[test]
    fn verify_core_rejects_missing_or_corrupt_sibling_binaries() {
        let dir = tempdir().unwrap();
        let core = dir.path().join("fesen-core.exe");
        assert_eq!(verify_core(&core), Err(ElevateError::Missing));

        fs::write(&core, b"stub").unwrap();
        assert_eq!(verify_core(&core), Err(ElevateError::Missing));

        fs::write(dir.path().join("wintun.dll"), b"bad-dll").unwrap();
        assert_eq!(verify_core(&core), Err(ElevateError::Missing));
    }
}
