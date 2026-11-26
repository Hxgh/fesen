use super::{DeviceInfo, Network, Platform, Tun, datagram, datagram6, hash_machine_id};
use crate::{Core, ipc, local::Local};
use fesen_proto::DeviceOs;
use jni::{
    JNIEnv, JavaVM,
    objects::{GlobalRef, JObject, JString, JValue},
};
use serde::Deserialize;
use socket2::Socket;
use std::{
    fs::File,
    io::{self, Read},
    net::{Ipv4Addr, Ipv6Addr, UdpSocket},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::mpsc;

struct Device {
    wire: File,
    wake_write: OwnedFd,
    done: Arc<AtomicBool>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Device {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
        let signal: u8 = 1;
        unsafe {
            libc::write(
                self.wake_write.as_raw_fd(),
                (&signal as *const u8).cast(),
                1,
            );
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn wake_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [-1 as libc::c_int; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let read_fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let write_fd = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    for fd in [read_fd.as_raw_fd(), write_fd.as_raw_fd()] {
        unsafe {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }
    }
    Ok((read_fd, write_fd))
}

struct Phone {
    vm: JavaVM,
    service: GlobalRef,
    input: OnceLock<mpsc::Sender<Vec<u8>>>,
    device: RwLock<Option<Device>>,
    underlay: RwLock<Underlay>,
    info: OnceLock<DeviceInfo>,
}

#[derive(Deserialize, Default)]
struct Underlay {
    handle: String,
    alternate: Option<Alternate>,
    #[serde(default)]
    ipv6: Vec<Ipv6Addr>,
}

#[derive(Clone, Deserialize)]
struct Alternate {
    handle: String,
    addresses: Vec<Ipv4Addr>,
}

impl Phone {
    fn error(e: impl std::fmt::Display) -> io::Error {
        io::Error::other(e.to_string())
    }

    fn bind(&self, socket: &Socket, target: Option<Ipv4Addr>, alternate: &str) -> io::Result<()> {
        let mut env = self.vm.attach_current_thread().map_err(Self::error)?;
        let handle = env
            .new_string(&self.underlay.read().unwrap().handle)
            .map_err(Self::error)?;
        let target = env
            .new_string(target.map_or_else(String::new, |ip| ip.to_string()))
            .map_err(Self::error)?;
        let alternate = env.new_string(alternate).map_err(Self::error)?;
        let allowed = env
            .call_method(
                self.service.as_obj(),
                "protectSocket",
                "(ILjava/lang/String;Ljava/lang/String;Ljava/lang/String;)Z",
                &[
                    JValue::Int(socket.as_raw_fd()),
                    JValue::Object(&handle),
                    JValue::Object(&target),
                    JValue::Object(&alternate),
                ],
            )
            .and_then(|r| r.z())
            .map_err(Self::error)?;
        if allowed {
            Ok(())
        } else {
            Err(io::Error::other("cannot protect socket"))
        }
    }
}

impl Platform for Phone {
    fn keep_empty_tun(&self) -> bool {
        true
    }
    fn access_ready(&self) -> bool {
        self.device.read().unwrap().is_some()
    }
    fn device_info(&self) -> DeviceInfo {
        self.info
            .get_or_init(|| {
                #[derive(Deserialize)]
                struct RawDevice {
                    machine_id: String,
                    name: String,
                }
                let parsed = (|| -> io::Result<RawDevice> {
                    let mut env = self.vm.attach_current_thread().map_err(Self::error)?;
                    let value = env
                        .call_method(
                            self.service.as_obj(),
                            "deviceJson",
                            "()Ljava/lang/String;",
                            &[],
                        )
                        .and_then(|r| r.l())
                        .map_err(Self::error)?;
                    let string = JString::from(value);
                    let text: String = env.get_string(&string).map_err(Self::error)?.into();
                    serde_json::from_str(&text).map_err(Self::error)
                })();
                match parsed {
                    Ok(raw) => {
                        let name = if raw.name.trim().is_empty() {
                            "Android".to_owned()
                        } else {
                            raw.name.trim().chars().take(64).collect()
                        };
                        let seed = if raw.machine_id.trim().is_empty() {
                            name.as_str()
                        } else {
                            raw.machine_id.trim()
                        };
                        DeviceInfo {
                            machine_id: hash_machine_id(DeviceOs::Android, seed),
                            os: DeviceOs::Android,
                            name,
                        }
                    }
                    Err(_) => DeviceInfo {
                        machine_id: hash_machine_id(DeviceOs::Android, "android"),
                        os: DeviceOs::Android,
                        name: "Android".into(),
                    },
                }
            })
            .clone()
    }
    fn network(&self) -> io::Result<Network> {
        let mut env = self.vm.attach_current_thread().map_err(Self::error)?;
        let value = env
            .call_method(
                self.service.as_obj(),
                "networkJson",
                "()Ljava/lang/String;",
                &[],
            )
            .and_then(|r| r.l())
            .map_err(Self::error)?;
        let string = JString::from(value);
        let text: String = env.get_string(&string).map_err(Self::error)?.into();
        #[derive(Deserialize)]
        struct Snapshot {
            #[serde(flatten)]
            network: Network,
            #[serde(flatten)]
            underlay: Underlay,
        }
        let snapshot: Snapshot = serde_json::from_str(&text).map_err(Self::error)?;
        *self.underlay.write().unwrap() = snapshot.underlay;
        Ok(snapshot.network)
    }

    fn protect(&self, socket: &Socket, target: Option<Ipv4Addr>) -> io::Result<()> {
        self.bind(socket, target, "")
    }

    fn alternate_udp(&self) -> Option<(UdpSocket, Vec<Ipv4Addr>)> {
        let alternate = self.underlay.read().unwrap().alternate.clone()?;
        let socket = datagram(|socket| self.bind(socket, None, &alternate.handle)).ok()?;
        Some((socket, alternate.addresses))
    }

    fn ipv6_udp(&self) -> Option<(UdpSocket, Vec<Ipv6Addr>)> {
        let addresses = self.underlay.read().unwrap().ipv6.clone();
        if addresses.is_empty() {
            return None;
        }
        let socket = datagram6(|socket| self.bind(socket, None, "")).ok()?;
        Some((socket, addresses))
    }

    fn apply(&self, tun: &Tun) -> io::Result<()> {
        let mut env = self.vm.attach_current_thread().map_err(Self::error)?;
        let setup = env
            .new_string(serde_json::to_string(tun).map_err(Self::error)?)
            .map_err(Self::error)?;
        let fd = env
            .call_method(
                self.service.as_obj(),
                "configureTun",
                "(Ljava/lang/String;)I",
                &[JValue::Object(&setup)],
            )
            .and_then(|r| r.i())
            .map_err(Self::error)?;
        if tun.address.is_none() {
            self.device.write().unwrap().take();
            return Ok(());
        }
        if fd < 0 {
            return Err(io::Error::other("VpnService failed to establish"));
        }
        let wire = unsafe { File::from_raw_fd(fd) };
        let mut reader = wire.try_clone()?;
        let input = self
            .input
            .get()
            .cloned()
            .ok_or_else(|| io::Error::other("packet input closed"))?;
        let (wake_read, wake_write) = wake_pipe()?;
        let done = Arc::new(AtomicBool::new(false));
        let stop = done.clone();
        let handle = std::thread::Builder::new()
            .name("fesen-tun".into())
            .spawn(move || {
                let mut packet = [0; super::MTU];
                let mut fds = [
                    libc::pollfd {
                        fd: reader.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: wake_read.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                ];
                while !stop.load(Ordering::Acquire) {
                    fds[0].revents = 0;
                    fds[1].revents = 0;
                    let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
                    if ready < 0 {
                        if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                            continue;
                        }
                        break;
                    }
                    if stop.load(Ordering::Acquire) || fds[1].revents != 0 {
                        break;
                    }
                    if fds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                        break;
                    }
                    if fds[0].revents & libc::POLLIN == 0 {
                        continue;
                    }
                    // Drain all queued non-blocking TUN packets before polling again.
                    loop {
                        match reader.read(&mut packet) {
                            Ok(0) => return,
                            Ok(n) => {
                                let _ = input.try_send(packet[..n].to_vec());
                            }
                            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                            Err(_) => return,
                        }
                    }
                }
            })?;
        *self.device.write().unwrap() = Some(Device {
            wire,
            wake_write,
            done,
            reader: Some(handle),
        });
        Ok(())
    }

    fn write_packet(&self, packet: &[u8]) {
        if let Some(device) = self.device.read().unwrap().as_ref() {
            unsafe {
                libc::write(
                    device.wire.as_raw_fd(),
                    packet.as_ptr().cast(),
                    packet.len(),
                );
            }
        }
    }

    fn set_input(&self, input: mpsc::Sender<Vec<u8>>) {
        let _ = self.input.set(input);
    }
}

pub struct Engine {
    core: Arc<Core>,
    runtime: tokio::runtime::Runtime,
}

impl Engine {
    pub fn can_resume(path: &Path) -> bool {
        Local::load(path).is_ok_and(|local| local.session.is_some())
    }

    pub fn running(&self) -> bool {
        self.core.running()
    }

    pub fn start(
        mut env: JNIEnv<'_>,
        service: JObject<'_>,
        path: JString<'_>,
    ) -> Option<Box<Self>> {
        let path: String = env.get_string(&path).ok()?.into();
        let vm = env.get_java_vm().ok()?;
        let service = env.new_global_ref(service).ok()?;
        let host = Arc::new(Phone {
            vm,
            service,
            input: OnceLock::new(),
            device: RwLock::new(None),
            underlay: RwLock::new(Underlay::default()),
            info: OnceLock::new(),
        });
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .ok()?;
        let core = {
            let _enter = runtime.enter();
            Core::new(PathBuf::from(path), host.clone(), true).ok()?
        };
        let watched = core.clone();
        runtime.spawn(async move {
            watched.wait_stopped().await;
            if let Ok(mut env) = host.vm.attach_current_thread() {
                let _ = env.call_method(host.service.as_obj(), "coreExpired", "()V", &[]);
            }
        });
        Some(Box::new(Self { core, runtime }))
    }

    pub fn request(&self, request: &str) -> String {
        let bytes = self
            .runtime
            .block_on(ipc::dispatch_bytes(&self.core, request.as_bytes()));
        String::from_utf8(bytes).unwrap_or_else(|_| "{}".to_owned())
    }

    pub fn halt(&self) {
        self.core.stop();
    }

    pub fn shutdown(self) {
        self.core.stop();
        self.runtime.block_on(self.core.wait_stopped());
        self.runtime.shutdown_timeout(Duration::from_secs(3));
    }
}
