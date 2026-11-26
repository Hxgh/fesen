use super::*;
use crate::control::discovery;
use fesen_core::{
    Command, Core, DeviceInfo, DeviceOs, Status,
    platform::{Network, Platform, Tun, Underlay, hash_machine_id},
};
use fesen_proto::{Code, Event, HEARTBEAT, Request};
use netstack_smoltcp::smoltcp::{
    iface::{Config, Interface, SocketSet},
    phy::{self, Device, DeviceCapabilities, Medium},
    socket::tcp,
    time::Instant as PacketClock,
    wire::{HardwareAddress, IpAddress, IpCidr, Ipv4Address},
};
use reqwest::{Client, StatusCode, header};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io,
    net::Ipv4Addr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, DuplexStream},
    net::TcpStream,
    sync::mpsc,
    task::JoinHandle,
    time::{Instant, sleep, timeout},
};

const PASSWORD: &str = "test-only-password-6a1";

mod admin;
mod direct;
mod sync;
#[cfg(target_os = "linux")]
mod webview;

#[derive(Default)]
struct Adapter {
    network: Mutex<Network>,
    tun: Mutex<Tun>,
    device: Option<DeviceInfo>,
    blocked_tcp_target: Mutex<Option<Ipv4Addr>>,
    from_app: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    to_app: Mutex<VecDeque<Vec<u8>>>,
}

impl Adapter {
    fn new(marker: &str, address: Ipv4Addr, protection: Option<String>) -> Arc<Self> {
        Self::with_device(marker, address, protection, DeviceOs::Linux, marker)
    }

    fn with_device(
        marker: &str,
        address: Ipv4Addr,
        protection: Option<String>,
        os: DeviceOs,
        machine_seed: &str,
    ) -> Arc<Self> {
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
            device: Some(DeviceInfo {
                machine_id: hash_machine_id(os, machine_seed),
                os,
                name: marker.into(),
            }),
            ..Self::default()
        })
    }
}

impl Platform for Adapter {
    fn device_info(&self) -> DeviceInfo {
        self.device.clone().unwrap_or_else(|| DeviceInfo {
            machine_id: hash_machine_id(DeviceOs::Linux, "adapter"),
            os: DeviceOs::Linux,
            name: "Adapter".into(),
        })
    }
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
        republish: false,
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
