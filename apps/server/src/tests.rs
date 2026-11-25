use super::*;
use netstack_smoltcp::smoltcp::{
    iface::{Config, Interface, SocketSet},
    phy::{self, Device, DeviceCapabilities, Medium},
    socket::tcp,
    time::Instant as PacketClock,
    wire::{HardwareAddress, IpAddress, IpCidr, Ipv4Address},
};
use fesen_core::{
    Command, Core, Status,
    platform::{Network, Platform, Tun, Underlay},
};
use reqwest::{Client, StatusCode, header};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io,
    net::Ipv4Addr,
    sync::{Arc, Mutex, atomic::AtomicBool},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, DuplexStream},
    net::TcpStream,
    task::JoinHandle,
    time::{sleep, timeout},
};

const PASSWORD: &str = "test-only-password-6a1";

#[cfg(target_os = "linux")]
mod webview;

#[test]
fn discovery_is_opt_in_and_legacy_frames_stay_compatible() {
    let config = Discovery::parse("", "", "").unwrap();
    assert!(config.listen.is_none() && config.public_port.is_none() && config.stun.is_none());
    let config = Discovery::parse("0.0.0.0:4455", "4456", "").unwrap();
    assert_eq!(config.listen.unwrap().port(), 4455);
    assert_eq!(config.public_port, Some(4456));
    let config = Discovery::parse("", "", "stun.example:3478").unwrap();
    assert_eq!(config.stun.as_deref(), Some("stun.example:3478"));
    assert!(Discovery::parse("", "4455", "").is_err());
    assert!(Discovery::parse("", "", "not-an-endpoint").is_err());
    assert!(Discovery::parse("0.0.0.0:4455", "0", "").is_err());
    assert!(Discovery::parse("0.0.0.0:4455", "", "stun.example:3478").is_err());

    let legacy =
        r#"{"kind":"welcome","device":"d","member":"m","token":null,"probe":"p","udp_port":4455}"#;
    let event = serde_json::from_str::<Event>(legacy).unwrap();
    assert!(matches!(
        &event,
        Event::Welcome {
            udp_port: 4455,
            stun: None,
            ..
        }
    ));
    assert_eq!(serde_json::to_string(&event).unwrap(), legacy);
    let legacy = r#"{"seq":0,"op":"candidates","local":["192.168.1.2:4000"]}"#;
    let request = serde_json::from_str::<Request>(legacy).unwrap();
    assert!(
        matches!(&request.call, fesen_proto::Call::Candidates { extra, .. } if extra.is_empty())
    );
    assert_eq!(serde_json::to_string(&request).unwrap(), legacy);
    let legacy = r#"{"kind":"punch","peer":"peer","candidates":["192.168.1.2:4000"]}"#;
    let event = serde_json::from_str::<Event>(legacy).unwrap();
    assert!(matches!(&event, Event::Punch { ipv6, .. } if ipv6.is_empty()));
    assert_eq!(serde_json::to_string(&event).unwrap(), legacy);
}

#[test]
fn tcp_only_center_exchanges_bounded_ipv6_candidates() {
    let app = App::new(
        rusqlite::Connection::open_in_memory().unwrap(),
        origin("http://127.0.0.1:8080").unwrap(),
        0,
        None,
    )
    .unwrap();
    let mut group = app.inner.lock().unwrap();
    let (access_out, mut access_in) = mpsc::channel(4);
    let (owner_out, mut owner_in) = mpsc::channel(4);
    let access = group
        .connect("access", "member-a".into(), "DeviceA".into(), vec![1], access_out)
        .unwrap();
    let owner = group
        .connect("owner", "member-b".into(), "DeviceB".into(), vec![2], owner_out)
        .unwrap();
    use fesen_proto::{Call, Publication};
    group
        .call(
            "owner",
            &owner,
            Call::Publish {
                service: Publication {
                    id: "0123456789abcdef0123456789abcdef".into(),
                    name: "HTTP".into(),
                    rules: fesen_acl::Rules::parse("198.51.100.10", "8080").unwrap(),
                    http: true,
                    kind: None,
                    host_os: None,
                },
            },
        )
        .unwrap();
    assert!(group.view().services[0].service.http);
    let register = |ipv6| Call::Candidates {
        local: vec![],
        extra: vec![],
        ipv6,
    };
    let mobile = "[2001:db8:1::1]:4000".parse().unwrap();
    let mac = "[2001:db8:2::1]:5000".parse().unwrap();
    assert_eq!(
        group.call(
            "access",
            &access,
            register(vec!["[::1]:4000".parse().unwrap()])
        ),
        Err(Code::Invalid)
    );
    assert_eq!(
        group.call(
            "access",
            &access,
            register(vec![mobile; fesen_proto::MAX_IPV6_CANDIDATES + 1])
        ),
        Err(Code::Invalid)
    );
    group
        .call("access", &access, register(vec![mobile]))
        .unwrap();
    group
        .call("owner", &owner, register(vec![mac, mac]))
        .unwrap();
    group
        .call(
            "access",
            &access,
            Call::Punch {
                peer: "owner".into(),
            },
        )
        .unwrap();
    assert!(
        matches!(owner_in.try_recv().unwrap(), Event::Punch { peer, candidates, ipv6 }
        if peer == "access" && candidates.is_empty() && ipv6 == vec![mobile])
    );
    assert!(
        matches!(access_in.try_recv().unwrap(), Event::Punch { peer, candidates, ipv6 }
        if peer == "owner" && candidates.is_empty() && ipv6 == vec![mac])
    );
}

#[derive(Default)]
struct Adapter {
    network: Mutex<Network>,
    tun: Mutex<Tun>,
    blocked_tcp_target: Mutex<Option<Ipv4Addr>>,
    from_app: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    to_app: Mutex<VecDeque<Vec<u8>>>,
}

impl Adapter {
    fn new(marker: &str, address: Ipv4Addr, protection: Option<String>) -> Arc<Self> {
        Arc::new(Self {
            network: Mutex::new(Network {
                marker: marker.into(),
                addresses: vec![address],
                routes: protection
                    .into_iter()
                    .map(|cidr| Underlay {
                        cidr: cidr.parse().unwrap(),
                        index: 1,
                        metric: 0,
                    })
                    .collect(),
                dns: vec![],
            }),
            ..Self::default()
        })
    }
}

impl Platform for Adapter {
    fn network(&self) -> io::Result<Network> {
        Ok(self.network.lock().unwrap().clone())
    }
    fn protect(&self, _: &socket2::Socket, target: Option<Ipv4Addr>) -> io::Result<()> {
        if target.is_some() && target == *self.blocked_tcp_target.lock().unwrap() {
            return Err(io::Error::other("blocked TCP target"));
        }
        Ok(())
    }
    fn apply(&self, tun: &Tun) -> io::Result<()> {
        *self.tun.lock().unwrap() = tun.clone();
        Ok(())
    }
    fn write_packet(&self, packet: &[u8]) {
        self.to_app.lock().unwrap().push_back(packet.to_vec());
    }
    fn set_input(&self, sender: mpsc::Sender<Vec<u8>>) {
        *self.from_app.lock().unwrap() = Some(sender);
    }
}

struct Wire(Arc<Adapter>);
struct Read(Vec<u8>);
impl phy::RxToken for Read {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.0)
    }
}
struct Write(mpsc::Sender<Vec<u8>>);
impl phy::TxToken for Write {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut packet = vec![0; len];
        let result = f(&mut packet);
        self.0.try_send(packet).unwrap();
        result
    }
}
impl Device for Wire {
    type RxToken<'a> = Read;
    type TxToken<'a> = Write;
    fn receive(&mut self, _: PacketClock) -> Option<(Read, Write)> {
        let packet = self.0.to_app.lock().unwrap().pop_front()?;
        Some((Read(packet), self.transmit(PacketClock::ZERO).unwrap()))
    }
    fn transmit(&mut self, _: PacketClock) -> Option<Write> {
        self.0.from_app.lock().unwrap().clone().map(Write)
    }
    fn capabilities(&self) -> DeviceCapabilities {
        let mut cap = DeviceCapabilities::default();
        cap.medium = Medium::Ip;
        cap.max_transmission_unit = 1500;
        cap
    }
}

async fn tcp_from_packet_device(adapter: Arc<Adapter>, to: SocketAddrV4, tag: u8) {
    let started = Instant::now();
    let local = adapter.tun.lock().unwrap().address.unwrap();
    let mut wire = Wire(adapter);
    let mut iface = Interface::new(
        Config::new(HardwareAddress::Ip),
        &mut wire,
        PacketClock::ZERO,
    );
    iface.update_ip_addrs(|list| list.push(IpCidr::new(IpAddress::Ipv4(local), 32)).unwrap());
    iface
        .routes_mut()
        .add_default_ipv4_route(Ipv4Address::new(198, 18, 0, 1))
        .unwrap();
    let mut sockets = SocketSet::new(vec![]);
    let socket = sockets.add(tcp::Socket::new(
        tcp::SocketBuffer::new(vec![0; 8192]),
        tcp::SocketBuffer::new(vec![0; 8192]),
    ));
    sockets
        .get_mut::<tcp::Socket>(socket)
        .connect(
            iface.context(),
            (IpAddress::Ipv4(*to.ip()), to.port()),
            48484,
        )
        .unwrap();
    let mut sent = false;
    let mut data = Vec::new();
    timeout(Duration::from_secs(18), async {
        loop {
            iface.poll(
                PacketClock::from_millis(started.elapsed().as_millis() as i64),
                &mut wire,
                &mut sockets,
            );
            let stream = sockets.get_mut::<tcp::Socket>(socket);
            if !sent && stream.can_send() {
                stream.send_slice(b"hello").unwrap();
                sent = true;
            }
            if stream.can_recv() {
                let mut payload = [0; 6];
                let n = stream.recv_slice(&mut payload).unwrap();
                data.extend_from_slice(&payload[..n]);
                if data.len() >= 6 {
                    break;
                }
            }
            sleep(Duration::from_millis(4)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "packet TCP {local} → {to}: state={:?}, sent={sent}, received={}",
            sockets.get::<tcp::Socket>(socket).state(),
            data.len()
        )
    });
    assert_eq!(data, [tag, b'h', b'e', b'l', b'l', b'o']);
}

async fn eventually(mut f: impl FnMut() -> bool) {
    timeout(Duration::from_secs(10), async {
        while !f() {
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("state did not converge");
}

async fn echo(ip: Ipv4Addr, tag: u8) -> (u16, JoinHandle<()>) {
    let listener = TcpListener::bind((ip, 0)).await.unwrap();
    (
        listener.local_addr().unwrap().port(),
        tokio::spawn(async move {
            loop {
                let (mut client, _) = listener.accept().await.expect("echo accept");
                tokio::spawn(async move {
                    let mut data = [0; 5];
                    while client.read_exact(&mut data).await.is_ok() {
                        let mut answer = [tag; 6];
                        answer[1..].copy_from_slice(&data);
                        if client.write_all(&answer).await.is_err() {
                            break;
                        }
                    }
                });
            }
        }),
    )
}

async fn preset(core: &Arc<Core>, name: &str, ip: Ipv4Addr, port: u16, http: bool) -> String {
    core.execute(Command::Save {
        id: None,
        name: name.into(),
        ips: ip.to_string(),
        ports: port.to_string(),
        http,
        kind: None,
        host_os: None,
    })
    .await
    .unwrap()
    .shares
    .last()
    .unwrap()
    .id
    .clone()
}

async fn publish(
    core: &Arc<Core>,
    name: &str,
    ip: Ipv4Addr,
    port: u16,
) -> (String, Result<Status, Code>) {
    let id = preset(core, name, ip, port, false).await;
    (
        id.clone(),
        core.execute(Command::Share { id, enabled: true }).await,
    )
}

fn flow(core: Arc<Core>, target: SocketAddrV4) -> (DuplexStream, JoinHandle<Result<(), Code>>) {
    let (client, peer) = tokio::io::duplex(8192);
    (
        client,
        tokio::spawn(async move { core.forward(peer, target).await }),
    )
}

async fn exchange(stream: &mut DuplexStream, tag: u8) {
    stream.write_all(b"hello").await.unwrap();
    let mut reply = [0; 6];
    timeout(Duration::from_secs(15), stream.read_exact(&mut reply))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reply, [tag, b'h', b'e', b'l', b'l', b'o']);
}

async fn closed(mut stream: DuplexStream) {
    let mut b = [0; 1];
    let read = timeout(Duration::from_secs(3), stream.read(&mut b))
        .await
        .expect("stream still open");
    assert_eq!(read.unwrap(), 0);
}

fn local_ipv4() -> Ipv4Addr {
    // Determine the host's IPv4 without sending a probe.
    let route = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    route.connect("192.0.2.1:9").unwrap();
    let SocketAddr::V4(local) = route.local_addr().unwrap() else {
        panic!("IPv4 required")
    };
    *local.ip()
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_account_devices_exchange_published_sites_and_verify_real_http() {
    use std::sync::atomic::AtomicUsize;

    let dir = tempfile::tempdir().unwrap();
    let db = store::db(&dir.path().join("db")).unwrap();
    store::create(
        &db,
        "member",
        &store::password_hash(PASSWORD).unwrap(),
        false,
    )
    .unwrap();
    let control = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", control.local_addr().unwrap());
    let center = App::new(db, origin(&base).unwrap(), 0, None).unwrap();
    let control_task = tokio::spawn(axum::serve(control, router(center)).into_future());

    let ip = local_ipv4();
    let mac_net = Adapter::new("mac-lan", ip, None);
    let phone_net = Adapter::new("phone-net", Ipv4Addr::new(10, 27, 9, 1), None);
    *phone_net.blocked_tcp_target.lock().unwrap() = Some(ip);
    let mac = Core::new(dir.path().join("mac"), mac_net, false).unwrap();
    let phone = Core::new(dir.path().join("phone"), phone_net.clone(), false).unwrap();
    for (core, label) in [(&mac, "Mac"), (&phone, "Android")] {
        core.execute(Command::Join {
            address: base.clone(),
        })
        .await
        .unwrap();
        core.execute(Command::Login {
            username: "member".into(),
            password: PASSWORD.into(),
            label: label.into(),
        })
        .await
        .unwrap();
        eventually(|| core.status().online).await;
    }

    let get_count = Arc::new(AtomicUsize::new(0));
    let server = |hits: Arc<AtomicUsize>| {
        axum::routing::get(move || {
            let hits = hits.clone();
            async move {
                hits.fetch_add(1, Ordering::Relaxed);
                "fesen-http-fixture"
            }
        })
    };
    let good = TcpListener::bind((ip, 0)).await.unwrap();
    let bad = TcpListener::bind((ip, 0)).await.unwrap();
    let (site_port, bad_port) = (
        good.local_addr().unwrap().port(),
        bad.local_addr().unwrap().port(),
    );
    let site_task = tokio::spawn(
        axum::serve(good, Router::new().route("/", server(get_count.clone()))).into_future(),
    );
    let bad_task = tokio::spawn(
        axum::serve(
            bad,
            Router::new().route("/", get(|| async { StatusCode::SERVICE_UNAVAILABLE })),
        )
        .into_future(),
    );
    let site = preset(&mac, "HTTP 200", ip, site_port, true).await;
    let error_site = preset(&mac, "HTTP 503", ip, bad_port, true).await;
    let local_test = |id: &str, port| Command::TestLocalSite {
        id: id.into(),
        ip,
        port,
    };
    assert!(matches!(
        phone.execute(local_test(&site, site_port)).await,
        Err(Code::Offline)
    ));
    assert!(matches!(
        mac.execute(local_test(&site, bad_port)).await,
        Err(Code::Unauthorized)
    ));
    assert_eq!(
        mac.execute(local_test(&error_site, bad_port))
            .await
            .unwrap()
            .http_status,
        Some(503)
    );
    assert_eq!(
        mac.execute(local_test(&site, site_port))
            .await
            .unwrap()
            .http_status,
        Some(200)
    );
    assert_eq!(get_count.swap(0, Ordering::Relaxed), 1);
    assert!(mac.status().services.is_empty() && phone.status().services.is_empty());
    for id in [&site, &error_site] {
        mac.execute(Command::Share {
            id: id.clone(),
            enabled: true,
        })
        .await
        .unwrap();
    }
    eventually(|| {
        phone.status().services.len() == 2 && phone_net.tun.lock().unwrap().address.is_some()
    })
    .await;
    assert!(phone.status().active.is_empty());
    assert!(mac.status().services.is_empty());
    let (phone_port, phone_target) = echo(ip, b'P').await;
    let (reverse, publication) = publish(&phone, "Phone publishes", ip, phone_port).await;
    publication.unwrap();
    eventually(|| {
        mac.status()
            .services
            .iter()
            .any(|s| s.service.id == reverse)
    })
    .await;
    assert!(
        phone
            .status()
            .services
            .iter()
            .all(|s| s.service.id != reverse)
    );

    for (id, port, code) in [(&site, site_port, 200), (&error_site, bad_port, 503)] {
        let reply = phone
            .execute(Command::TestConnection {
                id: id.clone(),
                ip,
                port,
            })
            .await
            .unwrap();
        assert_eq!(reply.http_status, Some(code));
    }
    assert_eq!(get_count.load(Ordering::Relaxed), 1);
    assert!(phone.status().virtual_ips.is_empty());
    let addr = SocketAddrV4::new(ip, site_port);
    let (mut original, conn) = flow(phone.clone(), addr);
    original
        .write_all(
            format!("GET / HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut body = Vec::new();
    timeout(Duration::from_secs(6), original.read_to_end(&mut body))
        .await
        .unwrap()
        .unwrap();
    assert!(body.starts_with(b"HTTP/1.1 200"));
    assert!(body.ends_with(b"fesen-http-fixture"));
    assert_eq!(get_count.load(Ordering::Relaxed), 2);
    drop(original);
    conn.await.unwrap().unwrap();

    mac.execute(Command::Share {
        id: site.clone(),
        enabled: false,
    })
    .await
    .unwrap();
    eventually(|| {
        phone
            .status()
            .services
            .iter()
            .all(|offer| offer.service.id != site)
    })
    .await;
    assert!(
        mac.status()
            .services
            .iter()
            .any(|s| s.service.id == reverse)
    );
    for core in [&mac, &phone] {
        core.stop();
        core.wait_stopped().await;
    }
    for task in [control_task, site_task, bad_task] {
        task.abort();
    }
    phone_target.abort();
}

#[tokio::test]
async fn admin_entry_loads_page_and_assets() {
    let dir = tempfile::tempdir().unwrap();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", http.local_addr().unwrap());
    let db = store::db(&dir.path().join("db")).unwrap();
    let app = App::new(db, origin(&base).unwrap(), 0, None).unwrap();
    let web = tokio::spawn(async move { axum::serve(http, router(app)).await.unwrap() });
    let client = Client::new();
    for path in ["", "admin", "admin/"] {
        let page = client.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(page.url().path(), "/admin/");
        assert_eq!(page.status(), StatusCode::OK);
        assert!(page.headers().contains_key(header::CONTENT_SECURITY_POLICY));
    }
    // Non-canonical Host (e.g. apex domain vs www) redirects to canonical origin /admin/ before login.
    let no_redirect = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let alt_host = no_redirect
        .get(format!("{base}admin/"))
        .header(header::HOST, "example.com:8866")
        .send()
        .await
        .unwrap();
    assert_eq!(alt_host.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        alt_host.headers()[header::LOCATION],
        format!("{base}admin/")
    );
    for path in [
        "admin/style.css",
        "admin/app.js",
        "admin/icons.js",
        "admin/theme.js",
        "admin/i18n.js",
        "admin/popover.js",
        "admin/host-card.js",
        "admin/settings.js",
        "admin/admin-view.js",
    ] {
        let asset = client.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(asset.status(), StatusCode::OK, "{path}");
    }
    for (preferred, expected, text) in [
        ("zh-CN,en;q=0.8", "zh-CN", "接入组"),
        ("en-US,zh-CN;q=0.8", "en", "Open Fesen to join"),
    ] {
        let response = client
            .get(format!("{base}join"))
            .header(header::ACCEPT_LANGUAGE, preferred)
            .send()
            .await
            .unwrap();
        assert_eq!(response.headers()[header::CONTENT_LANGUAGE], expected);
        assert!(response.text().await.unwrap().contains(text));
    }
    web.abort();
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stun_discovery_connects_when_no_local_candidate_is_usable() {
    let dir = tempfile::tempdir().unwrap();
    let db = store::db(&dir.path().join("db")).unwrap();
    let hash = store::password_hash(PASSWORD).unwrap();
    for who in ["publisher", "reader"] {
        store::create(&db, who, &hash, false).unwrap();
    }
    let ip = local_ipv4();
    let stun = UdpSocket::bind((ip, 0)).await.unwrap();
    let stun_addr = stun.local_addr().unwrap().to_string();
    let ready = Arc::new(AtomicBool::new(false));
    let stun_task = tokio::spawn({
        let ready = ready.clone();
        async move {
            let mut packet = [0; 128];
            loop {
                let (len, peer) = stun.recv_from(&mut packet).await.unwrap();
                let SocketAddr::V4(peer) = peer else { continue };
                assert_eq!(len, 20);
                assert_eq!(&packet[..8], &[0, 1, 0, 0, 0x21, 0x12, 0xa4, 0x42]);
                if !ready.load(Ordering::Acquire) {
                    continue;
                }
                packet[0] = 1;
                packet[3] = 12;
                packet[20..26].copy_from_slice(&[0, 0x20, 0, 8, 0, 1]);
                packet[26..28].copy_from_slice(&(peer.port() ^ 0x2112).to_be_bytes());
                for (i, byte) in peer.ip().octets().into_iter().enumerate() {
                    packet[28 + i] = byte ^ packet[4 + i];
                }
                stun.send_to(&packet[..32], peer).await.unwrap();
            }
        }
    });
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", http.local_addr().unwrap());
    let app = App::new(db, origin(&base).unwrap(), 0, Some(stun_addr)).unwrap();
    let web = tokio::spawn(axum::serve(http, router(app)).into_future());
    let setup = |who: &'static str| {
        let base = base.clone();
        let profile = dir.path().join(who);
        async move {
            let net = Adapter::new(who, Ipv4Addr::LOCALHOST, None);
            let core = Core::new(profile, net.clone(), false).unwrap();
            core.execute(Command::Join { address: base }).await.unwrap();
            core.execute(Command::Login {
                username: who.into(),
                password: PASSWORD.into(),
                label: who.into(),
            })
            .await
            .unwrap();
            eventually(|| core.status().online).await;
            (core, net)
        }
    };
    let ((publisher, _), (reader, net)) = tokio::join!(setup("publisher"), setup("reader"));
    let (port, echo) = echo(ip, b'S').await;
    let (id, result) = publish(&publisher, "STUN only", ip, port).await;
    result.unwrap();
    eventually(|| reader.status().services.len() == 1 && net.tun.lock().unwrap().address.is_some())
        .await;
    ready.store(true, Ordering::Release);
    timeout(Duration::from_secs(12), async {
        loop {
            match reader
                .execute(Command::TestConnection {
                    id: id.clone(),
                    ip,
                    port,
                })
                .await
            {
                Ok(_) => break,
                Err(Code::DirectFailed) => sleep(Duration::from_millis(50)).await,
                Err(error) => panic!("unexpected STUN connection error: {error}"),
            }
        }
    })
    .await
    .unwrap();
    let (mut tcp, handle) = flow(reader.clone(), SocketAddrV4::new(ip, port));
    exchange(&mut tcp, b'S').await;
    drop(tcp);
    timeout(Duration::from_secs(4), handle)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    for core in [publisher, reader] {
        core.stop();
        core.wait_stopped().await;
    }
    for task in [web, stun_task] {
        task.abort();
    }
    echo.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "requires local non-loopback TCP; run in a Linux container"
)]
async fn http_admin_direct_tcp_and_revocation() {
    let dir = tempfile::tempdir().unwrap();
    let db = store::db(&dir.path().join("db")).unwrap();
    store::create(&db, "admin", &store::password_hash(PASSWORD).unwrap(), true).unwrap();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", http.local_addr().unwrap());
    let app = App::new(db, origin(&base).unwrap(), 0, None).unwrap();
    let web = tokio::spawn(async move {
        axum::serve(http, router(app)).await.unwrap();
    });
    let api = Client::new();
    let login = api
        .post(format!("{base}admin/login"))
        .header("x-fesen-admin", "1")
        .json(&json!({"username":"admin","password":PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::NO_CONTENT);
    let cookie = login.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    assert_eq!(
        api.post(format!("{base}register"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let action = |body: Value| {
        api.post(format!("{base}admin/action"))
            .header("x-fesen-admin", "1")
            .header(header::COOKIE, &cookie)
            .json(&body)
    };
    for who in ["alice", "bob", "reader", "overlap"] {
        assert_eq!(
            action(json!({"op":"create","username":who,"password":PASSWORD}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
    }
    assert_eq!(
        action(json!({"op":"create","username":"reader","password":PASSWORD}))
            .header(header::ORIGIN, "https://untrusted.example")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    let ip = local_ipv4();
    let (p1, echo1) = echo(ip, b'A').await;
    let (p2, echo2) = echo(ip, b'B').await;
    let first_net = Adapter::new("lan-a", ip, None);
    let second_net = Adapter::new("lan-b", ip, None);
    let reader_net = Adapter::new("consumer", Ipv4Addr::new(10, 2, 7, 1), None);
    let overlap_net = Adapter::new("overlap", ip, Some(format!("{ip}/32")));
    let mut nodes = vec![];
    for (name, platform) in [
        ("alice", &first_net),
        ("bob", &second_net),
        ("reader", &reader_net),
        ("overlap", &overlap_net),
    ] {
        let core = Core::new(dir.path().join(name), platform.clone(), false).unwrap();
        let link = format!("fesen://join?server={base}");
        core.execute(Command::Join { address: link }).await.unwrap();
        if name == "alice" {
            assert!(matches!(
                core.execute(Command::Login {
                    username: name.into(),
                    password: "incorrect-password".into(),
                    label: name.into(),
                })
                .await,
                Err(Code::Unauthorized)
            ));
        }
        core.execute(Command::Login {
            username: name.into(),
            password: PASSWORD.into(),
            label: format!("node-{name}"),
        })
        .await
        .unwrap();
        eventually(|| core.status().online).await;
        assert!(core.status().active.is_empty());
        nodes.push(core);
    }
    let (alice, bob, reader, overlap) = (&nodes[0], &nodes[1], &nodes[2], &nodes[3]);
    let (id1, a) = publish(alice, "A", ip, p1).await;
    let (id2, b) = publish(bob, "B", ip, p2).await;
    assert!(a.is_ok() && b.is_ok());
    assert!(matches!(
        publish(bob, "collision", ip, p1).await.1,
        Err(Code::Conflict)
    ));
    eventually(|| {
        reader.status().services.len() == 2
            && reader_net.tun.lock().unwrap().address.is_some()
            && !overlap.status().virtual_ips.is_empty()
            && overlap_net.tun.lock().unwrap().address.is_some()
    })
    .await;
    assert!(reader.status().active.is_empty());
    assert!(
        !overlap_net
            .tun
            .lock()
            .unwrap()
            .routes
            .iter()
            .any(|r| r.range().contains(ip))
    );
    assert!(
        reader_net
            .tun
            .lock()
            .unwrap()
            .routes
            .iter()
            .any(|r| r.range().contains(ip))
    );
    let v4 = overlap.status().virtual_ips[0].virtual_ip.start;
    overlap
        .execute(Command::Customize {
            id: id1.clone(),
            remark: "只改名称".into(),
            alias: "test.fesen".into(),
            ip: Some(ip),
        })
        .await
        .unwrap();
    eventually(|| overlap.status().aliases.get("test.fesen") == Some(&v4)).await;

    let target = TcpListener::bind((ip, 0)).await.unwrap();
    let port = target.local_addr().unwrap().port();
    let (probe_id, result) = publish(alice, "Connect test", ip, port).await;
    result.unwrap();
    eventually(|| reader.status().services.len() == 3 && overlap.status().services.len() == 3)
        .await;
    *overlap_net.blocked_tcp_target.lock().unwrap() = Some(ip);
    let probe = |port| Command::TestConnection {
        id: probe_id.clone(),
        ip,
        port,
    };
    assert!(matches!(
        reader.execute(probe(p2)).await,
        Err(Code::Unauthorized)
    ));
    for node in [reader, overlap] {
        let reply = fesen_core::ipc::dispatch(
            node,
            &serde_json::to_vec(&json!({
                "op": "test_connection", "id": probe_id, "ip": ip, "port": port
            }))
            .unwrap(),
        )
        .await;
        assert_eq!(reply["data"]["online"], true, "{reply}");
        let (mut tcp, _) = timeout(Duration::from_secs(3), target.accept())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            timeout(Duration::from_secs(3), tcp.read(&mut [0]))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    }
    drop(target);
    assert!(matches!(
        overlap.execute(probe(port)).await,
        Err(Code::TargetUnavailable)
    ));
    alice
        .execute(Command::Share {
            id: probe_id.clone(),
            enabled: false,
        })
        .await
        .unwrap();
    eventually(|| reader.status().services.len() == 2 && overlap.status().services.len() == 2)
        .await;
    assert!(matches!(
        reader.execute(probe(port)).await,
        Err(Code::Offline)
    ));
    *overlap_net.blocked_tcp_target.lock().unwrap() = None;

    let (mut a, as_task) = flow(reader.clone(), SocketAddrV4::new(ip, p1));
    let (mut b, bs_task) = flow(reader.clone(), SocketAddrV4::new(ip, p2));
    eprintln!("checking QUIC peer streams");
    tokio::select! {
        result = as_task => panic!("first peer flow stopped: {result:?}"),
        result = bs_task => panic!("second peer flow stopped: {result:?}"),
        () = async {
            exchange(&mut a, b'A').await;
            exchange(&mut b, b'B').await;
        } => {}
    }
    eprintln!("checking packet TCP");
    tcp_from_packet_device(reader_net.clone(), SocketAddrV4::new(ip, p1), b'A').await;
    tcp_from_packet_device(overlap_net.clone(), SocketAddrV4::new(v4, p2), b'B').await;
    let (_, denied) = flow(overlap.clone(), SocketAddrV4::new(v4, 0));
    assert_eq!(denied.await.unwrap(), Err(Code::Unauthorized));

    alice
        .execute(Command::Share {
            id: id1.clone(),
            enabled: false,
        })
        .await
        .unwrap();
    eprintln!("checking unpublish");
    closed(a).await;
    exchange(&mut b, b'B').await;
    eventually(|| !reader.status().services.iter().any(|s| s.service.id == id1)).await;

    bob.execute(Command::Detach).await.unwrap();
    eprintln!("checking UI detach");
    reader.execute(Command::Detach).await.unwrap();
    assert!(bob.status().background && reader.status().background);
    sleep(Duration::from_millis(2300)).await;
    exchange(&mut b, b'B').await;
    reader.execute(Command::Attach).await.unwrap();
    bob.execute(Command::Attach).await.unwrap();
    let session: Value = api
        .get(format!("{base}admin/session"))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bob_id = session["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["member"] == "bob")
        .unwrap()["id"]
        .clone();
    assert_eq!(
        action(json!({"op":"revoke","device":bob_id}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    eprintln!("checking revocation");
    closed(b).await;
    eventually(|| {
        bob.status().member.is_none()
            && !reader.status().services.iter().any(|s| s.service.id == id2)
    })
    .await;
    assert!(matches!(
        bob.execute(Command::Login {
            username: "bob".into(),
            password: PASSWORD.into(),
            label: "bob".into()
        })
        .await,
        Err(Code::Unauthorized)
    ));

    alice
        .execute(Command::Share {
            id: id1.clone(),
            enabled: true,
        })
        .await
        .unwrap();
    eprintln!("checking network change");
    eventually(|| reader.status().services.len() == 1).await;
    let (mut a, _) = flow(reader.clone(), SocketAddrV4::new(ip, p1));
    exchange(&mut a, b'A').await;
    first_net.network.lock().unwrap().marker = "unrecognized-lan".into();
    eventually(|| {
        alice.status().error == Some(Code::NetworkChanged) && alice.status().active.is_empty()
    })
    .await;
    closed(a).await;
    eventually(|| reader.status().services.is_empty()).await;

    assert_eq!(
        action(json!({"op":"enable","username":"reader","enabled":false}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    eventually(|| reader.status().member.is_none()).await;
    assert_eq!(
        action(json!({"op":"reset","username":"alice","password":"test-password-replaced"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    eventually(|| alice.status().member.is_none()).await;
    assert!(matches!(
        alice
            .execute(Command::Login {
                username: "alice".into(),
                password: PASSWORD.into(),
                label: "alice".into()
            })
            .await,
        Err(Code::Unauthorized)
    ));
    for core in nodes {
        core.stop();
        core.wait_stopped().await;
    }
    assert_eq!(*overlap_net.tun.lock().unwrap(), Tun::default());
    for name in ["alice", "bob", "reader", "overlap"] {
        let logs =
            std::fs::read_to_string(dir.path().join(name).join("events.log")).unwrap_or_default();
        assert!(!logs.contains(PASSWORD) && !logs.contains("hello"));
    }
    web.abort();
    echo1.abort();
    echo2.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "requires local non-loopback TCP; run in a Linux container"
)]
async fn publisher_expires_established_flow_with_half_open_control() {
    let dir = tempfile::tempdir().unwrap();
    let db = store::db(&dir.path().join("db")).unwrap();
    let hash = store::password_hash(PASSWORD).unwrap();
    for username in ["publisher", "reader"] {
        store::create(&db, username, &hash, false).unwrap();
    }
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = http.local_addr().unwrap();
    let gate = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gate_addr = gate.local_addr().unwrap();
    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let base = origin(&format!("http://{server}")).unwrap();
    let app = App::new(db, base, udp.local_addr().unwrap().port(), None).unwrap();
    let discovery = tokio::spawn(discovery(udp, app.clone()));
    let web = tokio::spawn(axum::serve(http, router(app)).into_future());
    let muted = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let proxy = {
        let muted = muted.clone();
        let dropped = dropped.clone();
        tokio::spawn(async move {
            let (client, _) = gate.accept().await.unwrap();
            let upstream = TcpStream::connect(server).await.unwrap();
            let (mut from_client, mut to_client) = client.into_split();
            let (mut from_server, mut to_server) = upstream.into_split();
            let down = async {
                let mut buf = [0; 8192];
                loop {
                    let n = from_server.read(&mut buf).await?;
                    if n == 0 {
                        return Ok::<_, io::Error>(());
                    }
                    if muted.load(Ordering::Acquire) {
                        dropped.store(true, Ordering::Release);
                    } else {
                        to_client.write_all(&buf[..n]).await?;
                    }
                }
            };
            tokio::select! {
                result = tokio::io::copy(&mut from_client, &mut to_server) => result.map(|_| ()),
                result = down => result,
            }
        })
    };
    let ip = local_ipv4();
    let (port, target) = echo(ip, b'L').await;
    let publisher = Core::new(
        dir.path().join("publisher"),
        Adapter::new("publisher", ip, None),
        false,
    )
    .unwrap();
    let reader_net = Adapter::new("reader", Ipv4Addr::new(10, 2, 7, 1), None);
    let reader = Core::new(dir.path().join("reader"), reader_net.clone(), false).unwrap();
    for (core, username, address) in [
        (&publisher, "publisher", gate_addr),
        (&reader, "reader", server),
    ] {
        core.execute(Command::Join {
            address: format!("http://{address}"),
        })
        .await
        .unwrap();
        core.execute(Command::Login {
            username: username.into(),
            password: PASSWORD.into(),
            label: username.into(),
        })
        .await
        .unwrap();
        eventually(|| core.status().online).await;
    }
    publish(&publisher, "lease", ip, port).await.1.unwrap();
    eventually(|| {
        reader.status().services.len() == 1 && reader_net.tun.lock().unwrap().address.is_some()
    })
    .await;
    let (mut stream, mut forward) = flow(reader.clone(), SocketAddrV4::new(ip, port));
    tokio::select! {
        outcome = &mut forward => panic!("flow stopped before the first exchange: {outcome:?}"),
        () = exchange(&mut stream, b'L') => {}
    }

    let started = Instant::now();
    muted.store(true, Ordering::Release);
    let lease = fesen_proto::LEASE;
    let retained = timeout(lease + Duration::from_secs(1), async {
        loop {
            reader.execute(Command::Status { ui: true }).await.unwrap();
            let mut response = [0; 6];
            if let Err(error) = stream.write_all(b"hello").await {
                assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
                break;
            }
            if let Err(error) = stream.read_exact(&mut response).await {
                assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
                break;
            }
            assert_eq!(&response, b"Lhello");
            sleep(Duration::from_millis(250)).await;
        }
        started.elapsed()
    })
    .await
    .expect("already-open QUIC/TCP flow survived publisher lease expiry");
    assert!(
        retained >= lease - HEARTBEAT * 2,
        "flow closed early: {retained:?}"
    );
    assert!(dropped.load(Ordering::Acquire));
    assert!(reader.status().online, "the reader must not lose control");
    eventually(|| !publisher.status().online && reader.status().services.is_empty()).await;
    eprintln!("publisher control blackholed; existing TCP flow stopped after {retained:.2?}");

    let _ = timeout(Duration::from_secs(2), forward)
        .await
        .unwrap()
        .unwrap();
    for core in [publisher, reader] {
        core.stop();
        core.wait_stopped().await;
    }
    for task in [discovery, proxy] {
        task.abort();
    }
    web.abort();
    target.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires FESEN_LAB_TARGET_IP in local LAN"]
async fn lab_host_discovery_and_quic_tunnel() {
    use fesen_core::HostServiceInput;
    use fesen_proto::ServiceKind;

    let target_ip: Ipv4Addr = std::env::var("FESEN_LAB_TARGET_IP")
        .expect("set FESEN_LAB_TARGET_IP")
        .parse()
        .expect("valid IPv4");
    let local_ip = local_ipv4();

    let dir = tempfile::tempdir().unwrap();
    let db = store::db(&dir.path().join("db")).unwrap();
    let hash = store::password_hash(PASSWORD).unwrap();
    for username in ["publisher", "reader"] {
        store::create(&db, username, &hash, false).unwrap();
    }
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = http.local_addr().unwrap();
    let base = origin(&format!("http://{server}")).unwrap();
    let app = App::new(db, base.clone(), 0, None).unwrap();
    let web = tokio::spawn(axum::serve(http, router(app)).into_future());

    let pub_net = Adapter::new("lab-pub", local_ip, None);
    let read_net = Adapter::new("lab-read", local_ip, None);
    *read_net.blocked_tcp_target.lock().unwrap() = Some(target_ip);

    let publisher = Core::new(dir.path().join("pub"), pub_net, false).unwrap();
    let reader = Core::new(dir.path().join("read"), read_net, true).unwrap();
    for (core, user) in [(&publisher, "publisher"), (&reader, "reader")] {
        core.execute(Command::Join {
            address: base.to_string(),
        })
        .await
        .unwrap();
        core.execute(Command::Login {
            username: user.into(),
            password: PASSWORD.into(),
            label: user.into(),
        })
        .await
        .unwrap();
    }
    eventually(|| publisher.status().online && reader.status().online).await;

    let probed = publisher
        .execute(Command::ProbeHost {
            ip: target_ip,
            extra_ports: vec![],
            ports: vec![],
        })
        .await
        .unwrap()
        .host_probe
        .expect("host_probe payload");
    eprintln!(
        "HOST_PROBE ip={} os={:?} duration={}ms ports={:?}",
        probed.ip, probed.host_os, probed.duration_ms, probed.ports
    );
    let open: Vec<_> = probed.ports.iter().filter(|p| p.open).collect();
    assert!(!open.is_empty(), "expected at least one open standard port");
    if let Some(smb) = probed.ports.iter().find(|p| p.port == 445 && p.open) {
        assert!(!smb.recommended, "SMB 445 must stay opt-in by default");
    }

    // Publish all open standard ports except 445 (so we also test ACL rejection on unshared 445).
    let chosen: Vec<_> = open
        .iter()
        .filter(|p| p.port != 445)
        .map(|p| HostServiceInput {
            port: p.port,
            kind: p.kind,
        })
        .collect();
    assert!(!chosen.is_empty());
    let saved = publisher
        .execute(Command::SaveHost {
            ip: target_ip,
            name: "LabHost".into(),
            host_os: probed.host_os,
            services: chosen,
            publish: true,
        })
        .await
        .unwrap();
    let expected_count = saved.active.len();
    assert!(expected_count >= 1);
    eventually(|| {
        let s = reader.status();
        s.services.len() == expected_count && s.routes_ready
    })
    .await;

    // Verify end-to-end QUIC protocol probes from reader (with direct TCP to target_ip blocked!).
    let services = reader.status().services;
    for offering in &services {
        let port = offering.service.rules.ports[0].start;
        assert_eq!(offering.service.host_os, probed.host_os);
        let reply = reader
            .execute(Command::TestConnection {
                id: offering.service.id.clone(),
                ip: target_ip,
                port,
            })
            .await
            .unwrap();
        let outcome = reply.probe.expect("probe outcome");
        eprintln!(
            "E2E_PROBE port={} kind={:?} rtt={}ms http={:?} os={:?} detail={:?}",
            port, outcome.kind, outcome.rtt_ms, outcome.http_status, outcome.host_os, outcome.detail
        );
        assert!(outcome.rtt_ms > 0);
        if outcome.kind == ServiceKind::Http {
            assert!(matches!(outcome.http_status, Some(200..=399)));
        }
        // Unshared port on the same service must be rejected by ACL.
        assert_eq!(
            reader
                .execute(Command::TestConnection {
                    id: offering.service.id.clone(),
                    ip: target_ip,
                    port: 445,
                })
                .await
                .err(),
            Some(Code::Unauthorized)
        );
    }

    // Verify real data forwarding through QUIC on SSH (port 22) if present.
    if services
        .iter()
        .any(|o| o.service.effective_kind() == ServiceKind::Ssh && o.service.rules.allows(target_ip, 22))
    {
        let (mut app, stream) = tokio::io::duplex(4096);
        let fwd = tokio::spawn({
            let reader = reader.clone();
            async move { reader.forward(stream, SocketAddrV4::new(target_ip, 22)).await }
        });
        let mut buf = [0u8; 128];
        let n = timeout(Duration::from_secs(5), app.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        let banner = String::from_utf8_lossy(&buf[..n]);
        eprintln!("E2E_FORWARD_SSH_BANNER {}", banner.trim());
        assert!(banner.starts_with("SSH-2.0-"));
        drop(app);
        let _ = timeout(Duration::from_secs(3), fwd).await;
    }

    // Verify ShareMany turns off the entire host cleanly in one call.
    let ids: Vec<_> = saved.shares.iter().map(|s| s.id.clone()).collect();
    publisher
        .execute(Command::ShareMany {
            ids,
            enabled: false,
        })
        .await
        .unwrap();
    eventually(|| publisher.status().active.is_empty() && reader.status().services.is_empty()).await;

    for core in [publisher, reader] {
        core.stop();
        core.wait_stopped().await;
    }
    web.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_core_admin_bridge_follows_single_account_role() {
    let dir = tempfile::tempdir().unwrap();
    let db = store::db(&dir.path().join("db")).unwrap();
    let hash = store::password_hash(PASSWORD).unwrap();
    store::create(&db, "boss", &hash, true).unwrap();
    store::create(&db, "member", &hash, false).unwrap();

    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = http.local_addr().unwrap();
    let base = origin(&format!("http://{server}")).unwrap();
    let app = App::new(db, base.clone(), 0, None).unwrap();
    let web = tokio::spawn(axum::serve(http, router(app)).into_future());

    // 1. Admin account unlocks admin_ready directly on normal device login (single login).
    let boss_net = Adapter::new("boss", Ipv4Addr::LOCALHOST, None);
    let boss = Core::new(dir.path().join("boss"), boss_net, false).unwrap();
    boss.execute(Command::Join {
        address: base.to_string(),
    })
    .await
    .unwrap();
    let s = boss
        .execute(Command::Login {
            username: "boss".into(),
            password: PASSWORD.into(),
            label: "BossMac".into(),
        })
        .await
        .unwrap();
    assert!(s.is_admin && s.admin_ready);
    let snap = boss.execute(Command::AdminSession).await.unwrap();
    let data = snap.admin_data.expect("admin_data");
    assert_eq!(data["admin"], "boss");
    assert_eq!(data["users"].as_array().unwrap().len(), 2);

    // Create a new member and confirm self-demote is rejected.
    boss.execute(Command::AdminAction {
        action: json!({"op": "create", "username": "rookie", "password": PASSWORD}),
    })
    .await
    .unwrap();
    let snap2 = boss.execute(Command::AdminSession).await.unwrap();
    assert_eq!(snap2.admin_data.unwrap()["users"].as_array().unwrap().len(), 3);
    assert_eq!(
        boss.execute(Command::AdminAction {
            action: json!({"op": "set_admin", "username": "boss", "admin": false}),
        })
        .await
        .err(),
        Some(Code::Invalid)
    );

    // 2. Normal member has no admin module; once promoted, resuming its own session unlocks admin directly.
    let mem_net = Adapter::new("mem", Ipv4Addr::LOCALHOST, None);
    let mem = Core::new(dir.path().join("mem"), mem_net, false).unwrap();
    mem.execute(Command::Join {
        address: base.to_string(),
    })
    .await
    .unwrap();
    let ms = mem
        .execute(Command::Login {
            username: "member".into(),
            password: PASSWORD.into(),
            label: "MemMac".into(),
        })
        .await
        .unwrap();
    assert!(!ms.is_admin && !ms.admin_ready);
    assert_eq!(
        mem.execute(Command::AdminSession).await.err(),
        Some(Code::Unauthorized)
    );

    boss.execute(Command::AdminAction {
        action: json!({"op": "set_admin", "username": "member", "admin": true}),
    })
    .await
    .unwrap();
    mem.execute(Command::Stop).await.unwrap();
    let ms2 = mem.execute(Command::Start).await.unwrap();
    assert!(ms2.is_admin && ms2.admin_ready);
    assert_eq!(
        mem.execute(Command::AdminSession)
            .await
            .unwrap()
            .admin_data
            .unwrap()["admin"],
        "member"
    );

    // Demoting revokes admin_sessions immediately and clears role on resume.
    boss.execute(Command::AdminAction {
        action: json!({"op": "set_admin", "username": "member", "admin": false}),
    })
    .await
    .unwrap();
    assert_eq!(
        mem.execute(Command::AdminSession).await.err(),
        Some(Code::Unauthorized)
    );
    mem.execute(Command::Stop).await.unwrap();
    let ms3 = mem.execute(Command::Start).await.unwrap();
    assert!(!ms3.is_admin && !ms3.admin_ready);

    for core in [boss, mem] {
        core.stop();
        core.wait_stopped().await;
    }
    web.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_account_supports_multiple_concurrent_devices() {
    let dir = tempfile::tempdir().unwrap();
    let db = store::db(&dir.path().join("db")).unwrap();
    let hash = store::password_hash(PASSWORD).unwrap();
    store::create(&db, "boss", &hash, true).unwrap();

    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = http.local_addr().unwrap();
    let base = origin(&format!("http://{server}")).unwrap();
    let app = App::new(db, base.clone(), 0, None).unwrap();
    let web = tokio::spawn(axum::serve(http, router(app)).into_future());

    let mac = Core::new(
        dir.path().join("mac"),
        Adapter::new("mac", Ipv4Addr::new(192, 0, 2, 10), None),
        false,
    )
    .unwrap();
    let phone = Core::new(
        dir.path().join("phone"),
        Adapter::new("phone", Ipv4Addr::new(198, 51, 100, 20), None),
        false,
    )
    .unwrap();

    for (core, label) in [(&mac, "BossMac"), (&phone, "BossPhone")] {
        core.execute(Command::Join {
            address: base.to_string(),
        })
        .await
        .unwrap();
        let st = core
            .execute(Command::Login {
                username: "boss".into(),
                password: PASSWORD.into(),
                label: label.into(),
            })
            .await
            .unwrap();
        assert!(st.is_admin && st.admin_ready);
        eventually(|| core.status().online).await;
    }

    // Both concurrent devices under the same account show up online in admin session.
    let snap = mac
        .execute(Command::AdminSession)
        .await
        .unwrap()
        .admin_data
        .unwrap();
    let online_devices: Vec<_> = snap["devices"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["member"] == "boss" && d["online"] == true)
        .collect();
    assert_eq!(online_devices.len(), 2);

    // Mac publishes a service; same-account Phone receives it while Mac does not duplicate itself.
    let saved = mac
        .execute(Command::Save {
            id: None,
            name: "Office".into(),
            ips: "192.0.2.35".into(),
            ports: "80".into(),
            http: true,
            kind: Some(fesen_proto::ServiceKind::Http),
            host_os: None,
        })
        .await
        .unwrap();
    let share_id = saved.shares[0].id.clone();
    mac.execute(Command::Share {
        id: share_id.clone(),
        enabled: true,
    })
    .await
    .unwrap();
    eventually(|| phone.status().services.iter().any(|o| o.service.id == share_id)).await;
    let phone_view = phone.status().services;
    let offered = phone_view.iter().find(|o| o.service.id == share_id).unwrap();
    assert_eq!(offered.member, "boss");
    assert_eq!(offered.device.as_deref(), Some("BossMac"));
    assert!(mac.status().services.is_empty());

    // Logging out Phone only logs out Phone; Mac stays online with active share & valid admin session.
    phone.execute(Command::Logout).await.unwrap();
    assert!(phone.status().member.is_none());
    let mac_st = mac.status();
    assert!(mac_st.online && mac_st.active.contains(&share_id));
    assert!(mac.execute(Command::AdminSession).await.is_ok());

    for core in [mac, phone] {
        core.stop();
        core.wait_stopped().await;
    }
    web.abort();
}
