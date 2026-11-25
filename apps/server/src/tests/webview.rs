use super::*;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    process::Command as Process,
    sync::atomic::AtomicUsize,
};
use tokio::io::unix::AsyncFd;

const DEV: &str = "fs-web";
const TABLE: &str = "176";

struct LabTarget {
    ip: Ipv4Addr,
    port: u16,
    name: String,
}

impl LabTarget {
    fn from_env() -> Self {
        let ip = env::var("FESEN_LAB_TARGET_IP")
            .expect("需要设置 FESEN_LAB_TARGET_IP（或在仓库根目录配置 .env.local）")
            .parse()
            .expect("FESEN_LAB_TARGET_IP 不是有效的 IPv4 地址");
        let port = env::var("FESEN_LAB_TARGET_PORT")
            .expect("需要设置 FESEN_LAB_TARGET_PORT（或在仓库根目录配置 .env.local）")
            .parse()
            .expect("FESEN_LAB_TARGET_PORT 不是有效的端口");
        let name = env::var("FESEN_LAB_TARGET_NAME")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "lab-http".into());
        Self { ip, port, name }
    }

    fn url(&self) -> String {
        format!("http://{}:{}/", self.ip, self.port)
    }
}

fn ip(args: &[&str]) -> io::Result<()> {
    if Process::new("ip").args(args).status()?.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("ip {args:?} failed")))
    }
}

struct BrowserTun {
    fd: File,
    target: Ipv4Addr,
    port: u16,
    packets: Arc<AtomicUsize>,
    ready: AtomicBool,
}

impl BrowserTun {
    fn open(target: Ipv4Addr, port: u16) -> io::Result<Self> {
        let fd = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open("/dev/net/tun")?;
        // TUNSETIFF copies this fixed-size, zero-terminated ifreq.
        let mut req: libc::ifreq = unsafe { std::mem::zeroed() };
        for (dst, src) in req.ifr_name.iter_mut().zip(DEV.bytes()) {
            *dst = src as _;
        }
        req.ifr_ifru.ifru_flags = (libc::IFF_TUN | libc::IFF_NO_PI) as _;
        if unsafe { libc::ioctl(fd.as_raw_fd(), libc::TUNSETIFF, &mut req) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            fd,
            target,
            port,
            packets: Arc::default(),
            ready: false.into(),
        })
    }
}

impl Platform for BrowserTun {
    fn network(&self) -> io::Result<Network> {
        Ok(Network {
            marker: "webview-test".into(),
            addresses: vec![local_ipv4()],
            ..Network::default()
        })
    }
    fn protect(&self, _: &socket2::Socket, target: Option<Ipv4Addr>) -> io::Result<()> {
        if target.is_some_and(|ip| !ip.is_loopback()) {
            return Err(io::Error::other("reader may not dial the website locally"));
        }
        Ok(())
    }
    fn apply(&self, tun: &Tun) -> io::Result<()> {
        self.ready.store(false, Ordering::Release);
        ip(&["route", "flush", "table", TABLE, "dev", DEV])?;
        if let Some(addr) = tun.address {
            let host = format!("{}/32", self.target);
            if tun.routes.len() != 1 || tun.routes[0].to_string() != host {
                return Err(io::Error::other("expected only the original target /32"));
            }
            ip(&["addr", "replace", &format!("{addr}/32"), "dev", DEV])?;
            ip(&["link", "set", DEV, "up"])?;
            ip(&[
                "route",
                "add",
                &host,
                "dev",
                DEV,
                "src",
                &addr.to_string(),
                "table",
                TABLE,
            ])?;
            self.ready.store(true, Ordering::Release);
        }
        Ok(())
    }
    fn set_input(&self, input: mpsc::Sender<Vec<u8>>) {
        let file = self.fd.try_clone().unwrap();
        // SAFETY: AsyncFd owns this File; its descriptor is never replaced or closed separately.
        let fd = unsafe { AsyncFd::register(file) }.unwrap();
        let packets = self.packets.clone();
        let target = self.target;
        let port = self.port;
        tokio::spawn(async move {
            let mut buf = [0; fesen_core::platform::MTU];
            loop {
                let mut guard = tokio::select! {
                    _ = input.closed() => break,
                    ready = fd.readable() => match ready { Ok(g) => g, Err(_) => break },
                };
                match guard.try_io(|fd| fd.get_ref().read(&mut buf)) {
                    Ok(Ok(n)) if n >= 20 => {
                        let offset = usize::from(buf[0] & 15) * 4;
                        if buf[0] >> 4 == 4
                            && buf[9] == 6
                            && buf[16..20] == target.octets()
                            && n >= offset + 4
                            && buf[offset + 2..offset + 4] == port.to_be_bytes()
                        {
                            packets.fetch_add(1, Ordering::Relaxed);
                        }
                        if input.send(buf[..n].to_vec()).await.is_err() {
                            break;
                        }
                    }
                    Ok(Ok(_)) | Ok(Err(_)) => break,
                    Err(_) => continue,
                }
            }
        });
    }
    fn write_packet(&self, packet: &[u8]) {
        let _ = (&self.fd).write(packet);
    }
}

async fn browser(expectation: &'static str, url: String) {
    let status = tokio::task::spawn_blocking(move || {
        Process::new("timeout")
            .args([
                "-k",
                "3",
                "40",
                "setpriv",
                "--reuid=65534",
                "--regid=65534",
                "--clear-groups",
                "--no-new-privs",
                "dbus-run-session",
                "--",
                "xvfb-run",
                "-a",
                "/usr/bin/python3",
                concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/webview/site.py"),
                expectation,
            ])
            .env("FESEN_LAB_TARGET_URL", url)
            .env("HOME", "/tmp/ph-web-home")
            .env("XDG_RUNTIME_DIR", "/tmp/ph-web-home")
            .env("NO_AT_BRIDGE", "1")
            .env("LIBGL_ALWAYS_SOFTWARE", "1")
            .status()
    })
    .await
    .unwrap()
    .unwrap();
    assert!(status.success(), "WebView: {expectation}: {status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real website, Linux TUN and WebKitGTK; run scripts/test-webview.sh"]
async fn original_address_renders_in_webview() {
    assert_eq!(env::var("FESEN_WEBVIEW_LAB").as_deref(), Ok("1"));
    assert!(std::path::Path::new("/.dockerenv").is_file());
    let target = LabTarget::from_env();
    let url = target.url();
    let dir = tempfile::tempdir().unwrap();
    let db = store::db(&dir.path().join("db")).unwrap();
    let pass = store::secret();
    store::create(
        &db,
        "browser-test",
        &store::password_hash(&pass).unwrap(),
        false,
    )
    .unwrap();
    let listen = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listen.local_addr().unwrap());
    let control = App::new(db, origin(&base).unwrap(), 0, None).unwrap();
    let web = tokio::spawn(axum::serve(listen, router(control)).into_future());
    let tun = Arc::new(BrowserTun::open(target.ip, target.port).unwrap());
    let publisher = Core::new(
        dir.path().join("publisher"),
        Adapter::new("lan", local_ipv4(), None),
        true,
    )
    .unwrap();
    let reader = Core::new(dir.path().join("reader"), tun.clone(), true).unwrap();
    for (name, core) in [("publisher", &publisher), ("reader", &reader)] {
        core.execute(Command::Join {
            address: base.clone(),
        })
        .await
        .unwrap();
        core.execute(Command::Login {
            username: "browser-test".into(),
            password: pass.clone(),
            label: name.into(),
        })
        .await
        .unwrap();
        eventually(|| core.status().online).await;
    }
    let id = preset(&publisher, &target.name, target.ip, target.port, true).await;
    browser("blocked", url.clone()).await;
    publisher
        .execute(Command::Share {
            id: id.clone(),
            enabled: true,
        })
        .await
        .unwrap();
    eventually(|| tun.ready.load(Ordering::Acquire) && reader.status().services.len() == 1).await;
    assert!(reader.status().virtual_ips.is_empty());
    assert_eq!(
        reader
            .execute(Command::TestConnection {
                id: id.clone(),
                ip: target.ip,
                port: target.port
            })
            .await
            .unwrap()
            .http_status,
        Some(200)
    );
    let before = tun.packets.load(Ordering::Relaxed);
    browser("rendered", url.clone()).await;
    assert!(
        tun.packets.load(Ordering::Relaxed) > before,
        "no browser packets on the original-IP TUN route"
    );
    publisher
        .execute(Command::Share { id, enabled: false })
        .await
        .unwrap();
    eventually(|| !tun.ready.load(Ordering::Acquire) && reader.status().services.is_empty()).await;
    browser("blocked", url).await;
    for core in [reader, publisher] {
        core.stop();
        core.wait_stopped().await;
    }
    web.abort();
}
