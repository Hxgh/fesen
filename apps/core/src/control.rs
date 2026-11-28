use crate::{
    direct::Direct,
    local::Identity,
    node::Core,
    platform::{self, Platform},
};
use futures_util::{SinkExt, StreamExt};
use fesen_proto::{
    Call, Code, Credential, Event, HEARTBEAT, Hello, LEASE, MAX_CANDIDATES, MAX_FRAME,
    MAX_IPV6_CANDIDATES, PROBE_PREFIX, PROTOCOL, Request, candidate, candidate6, stun_endpoint,
};
use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::TcpStream,
    sync::{OnceCell, broadcast, mpsc, oneshot},
    time::{Instant, interval, timeout},
};
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, client_async_tls_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use tokio_util::sync::CancellationToken;
use url::Url;

pub(crate) async fn tls_client_config() -> Result<Arc<rustls::ClientConfig>, Code> {
    static CONFIG: OnceCell<Arc<rustls::ClientConfig>> = OnceCell::const_new();
    CONFIG
        .get_or_try_init(|| async {
            let native = tokio::task::spawn_blocking(rustls_native_certs::load_native_certs)
                .await
                .ok();
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            if let Some(native) = native {
                roots.add_parsable_certificates(native.certs);
            }
            let config = rustls::ClientConfig::builder_with_provider(
                crate::direct::crypto_provider(),
            )
            .with_safe_default_protocol_versions()
            .map_err(|_| Code::Internal)?
            .with_root_certificates(roots)
            .with_no_client_auth();
            Ok(Arc::new(config))
        })
        .await
        .cloned()
}

type Wire = WebSocketStream<MaybeTlsStream<TcpStream>>;
type Signals = broadcast::Sender<(String, Vec<SocketAddr>)>;
type Reply = oneshot::Sender<Result<(), Code>>;

pub struct Link {
    pub id: String,
    pub direct: Arc<Direct>,
    pub server_ip: Ipv4Addr,
    control: mpsc::Sender<(Call, Reply)>,
    signals: Signals,
}

pub struct Session {
    pub member: String,
    pub token: Option<String>,
    pub is_admin: bool,
    pub admin_token: Option<String>,
    pub link: Arc<Link>,
    worker: Worker,
}

struct Worker {
    ws: Wire,
    probe: Option<(SocketAddrV4, Vec<u8>)>,
    stun: Option<String>,
    calls: mpsc::Receiver<(Call, Reply)>,
}

impl Link {
    #[cfg(test)]
    pub fn isolated(direct: Arc<Direct>) -> Arc<Self> {
        Arc::new(Self {
            id: "owner".into(),
            direct,
            server_ip: Ipv4Addr::LOCALHOST,
            control: mpsc::channel(1).0,
            signals: broadcast::channel(1).0,
        })
    }

    pub async fn call(&self, call: Call) -> Result<(), Code> {
        let (tx, rx) = oneshot::channel();
        self.control
            .try_send((call, tx))
            .map_err(|_| Code::NotConnected)?;
        timeout(Duration::from_secs(6), rx)
            .await
            .map_err(|_| Code::NotConnected)?
            .map_err(|_| Code::NotConnected)?
    }

    pub async fn candidates(&self, peer: &str) -> Result<Vec<SocketAddr>, Code> {
        let mut signals = self.signals.subscribe();
        for _ in 0..2 {
            self.call(Call::Punch {
                peer: peer.to_owned(),
            })
            .await?;
            let result = timeout(Duration::from_secs(2), async {
                loop {
                    match signals.recv().await {
                        Ok((id, addresses)) if id == peer => return Ok(addresses),
                        Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(_) => return Err(Code::NotConnected),
                    }
                }
            })
            .await;
            if let Ok(result) = result {
                let addresses = result?;
                if !addresses.is_empty() {
                    return Ok(addresses);
                }
            }
            tokio::time::sleep(Duration::from_millis(1200)).await;
        }
        Err(Code::DirectFailed)
    }
}

impl Session {
    pub async fn connect(
        platform: &dyn Platform,
        addr: &Url,
        identity: Identity,
        credential: Credential,
        cancel: CancellationToken,
    ) -> Result<Self, Code> {
        let host = addr.host_str().ok_or(Code::Invalid)?;
        let port = addr.port_or_known_default().ok_or(Code::Invalid)?;
        let mut url = addr.join("/control").map_err(|_| Code::Invalid)?;
        url.set_scheme(if addr.scheme() == "https" {
            "wss"
        } else {
            "ws"
        })
        .map_err(|_| Code::Invalid)?;
        let (mut ws, server_ip) = timeout(Duration::from_secs(11), async {
            let addresses = tokio::net::lookup_host((host, port))
                .await
                .map_err(|_| Code::NotConnected)?;
            let mut connected = None;
            for target in addresses
                .filter_map(|a| match a {
                    SocketAddr::V4(a) => Some(a),
                    _ => None,
                })
                .take(4)
            {
                if let Ok(conn) =
                    timeout(Duration::from_secs(3), platform::tcp(platform, target)).await
                    && let Ok(conn) = conn
                {
                    connected = Some((conn, *target.ip()));
                    break;
                }
            }
            let (tcp, ip) = connected.ok_or(Code::NotConnected)?;
            let tls = if url.scheme() == "wss" {
                Some(Connector::Rustls(tls_client_config().await?))
            } else {
                Some(Connector::Plain)
            };
            let options = WebSocketConfig::default()
                .max_message_size(Some(MAX_FRAME))
                .max_frame_size(Some(MAX_FRAME));
            let (ws, _) = client_async_tls_with_config(url.as_str(), tcp, Some(options), tls)
                .await
                .map_err(|_| Code::NotConnected)?;
            Ok((ws, ip))
        })
        .await
        .map_err(|_| Code::NotConnected)??;
        let info = platform.device_info();
        send(
            &mut ws,
            &Hello {
                version: PROTOCOL,
                certificate: identity.cert.clone(),
                credential,
                machine_id: fesen_proto::valid_machine_id(&info.machine_id)
                    .then_some(info.machine_id),
                os: Some(info.os),
            },
        )
        .await?;
        let event = timeout(HEARTBEAT, ws.next())
            .await
            .map_err(|_| Code::NotConnected)?;
        let Some(Ok(Message::Text(text))) = event else {
            return Err(Code::NotConnected);
        };
        let (id, member, token, probe, udp_port, stun, is_admin, admin_token) =
            match serde_json::from_str(&text).map_err(|_| Code::Invalid)? {
                Event::Welcome {
                    device,
                    member,
                    token,
                    probe,
                    udp_port,
                    stun,
                    is_admin,
                    admin_token,
                } => (
                    device,
                    member,
                    token,
                    probe,
                    udp_port,
                    stun,
                    is_admin,
                    admin_token,
                ),
                Event::Rejected { error } => return Err(error),
                _ => return Err(Code::NotConnected),
            };
        let probe = match (udp_port, probe.len()) {
            (0, _) => None,
            (_, 64) => Some((
                SocketAddrV4::new(server_ip, udp_port),
                [PROBE_PREFIX, probe.as_bytes()].concat(),
            )),
            _ => return Err(Code::Invalid),
        };
        let stun = stun.filter(|s| probe.is_none() && stun_endpoint(s).is_some());
        let direct =
            Direct::new(identity, platform, cancel, stun.is_some()).map_err(|_| Code::Internal)?;
        let (control, calls) = mpsc::channel(64);
        let (signals, _) = broadcast::channel(64);
        let link = Arc::new(Link {
            id,
            direct,
            server_ip,
            control,
            signals,
        });
        Ok(Self {
            member,
            token,
            is_admin,
            admin_token,
            link,
            worker: Worker {
                ws,
                probe,
                stun,
                calls,
            },
        })
    }

    pub fn spawn(self, core: Arc<Core>) {
        tokio::spawn(self.worker.run(core, self.link));
    }
}

async fn send(ws: &mut Wire, value: &impl serde::Serialize) -> Result<u64, Code> {
    let text = serde_json::to_string(value).map_err(|_| Code::Internal)?;
    let bytes = text.len() as u64;
    timeout(Duration::from_secs(3), ws.send(Message::Text(text.into())))
        .await
        .map_err(|_| Code::NotConnected)?
        .map_err(|_| Code::NotConnected)?;
    Ok(bytes)
}

fn candidates(host: &[SocketAddrV4], observed: &[SocketAddrV4], ipv6: &[SocketAddrV6]) -> Call {
    let private = |a: &SocketAddrV4| a.ip().is_private() || a.ip().is_link_local();
    // Keep both observed sockets, without displacing the first local path.
    let mut all = Vec::with_capacity(MAX_CANDIDATES);
    for &addr in host
        .iter()
        .take(1)
        .chain(observed)
        .chain(host.iter().skip(1))
    {
        if all.len() == MAX_CANDIDATES {
            break;
        }
        if !all.contains(&addr) {
            all.push(addr);
        }
    }
    let (local, extra) = all.into_iter().partition(private);
    Call::Candidates {
        local,
        extra,
        ipv6: ipv6.to_vec(),
    }
}

impl Worker {
    async fn run(mut self, core: Arc<Core>, link: Arc<Link>) {
        let mut pending: HashMap<u64, (Instant, Reply)> = HashMap::new();
        let mut seq = 1u64;
        let mut registered = interval(HEARTBEAT / 2);
        let mut last_ping = Instant::now() - HEARTBEAT;
        let mut expiry = Instant::now() + LEASE;
        let host = link.direct.candidates(core.network().addresses);
        let local_ipv6 = link.direct.candidates6();
        let (observed, mut observations) = mpsc::channel(1);
        if let Some(stun) = self.stun.take() {
            let direct = link.direct.clone();
            tokio::spawn(async move {
                let mut tick = interval(HEARTBEAT);
                loop {
                    let result = tokio::select! {
                        _ = direct.cancel.cancelled() => break,
                        _ = tick.tick() => tokio::select! {
                            _ = direct.cancel.cancelled() => break,
                            result = direct.discover(&stun) => result,
                        },
                    };
                    if observed.send(result).await.is_err() {
                        break;
                    }
                }
            });
        } else {
            drop(observed);
        }
        let mut mapped = vec![];
        if let Some((server, probe)) = &self.probe {
            link.direct.probe(probe, *server);
        }
        let result = async {
            let n = send(&mut self.ws, &Request { seq: 0, call: candidates(&host, &[], local_ipv6) }).await?;
            core.traffic.add_control_tx(n);
            loop {
                tokio::select! {
                    biased;
                    _ = link.direct.cancel.cancelled() => return Ok(()),
                    _ = tokio::time::sleep_until(expiry) => return Err(Code::NotConnected),
                    data = self.ws.next() => {
                        let Some(Ok(message)) = data else { return Err(Code::NotConnected) };
                        match message {
                            Message::Text(text) => {
                                core.traffic.add_control_rx(text.len() as u64);
                                match serde_json::from_str::<Event>(&text).map_err(|_| Code::Invalid)? {
                                Event::View { view } => {
                                    expiry = Instant::now() + LEASE;
                                    core.update_view(&link, view);
                                }
                                Event::Sync { prefs, profiles } => {
                                    core.update_sync(&link, prefs, profiles);
                                }
                                Event::Result { seq, error } => if let Some((_, ret)) = pending.remove(&seq) {
                                    let _ = ret.send(error.map_or(Ok(()), Err));
                                },
                                Event::Punch { peer, candidates, ipv6 } => {
                                    if candidates.len() <= MAX_CANDIDATES + 1
                                        && ipv6.len() <= MAX_IPV6_CANDIDATES
                                        && link.direct.membership.peer(&peer).is_some()
                                    {
                                        let addresses: Vec<_> = candidates.into_iter().filter(|&a| candidate(a)
                                            || (self.probe.is_some() && link.server_ip.is_loopback()
                                                && a.ip().is_loopback() && a.port() != 0))
                                            .map(SocketAddr::V4)
                                            .chain(ipv6.into_iter().filter(|&a| candidate6(a)).map(SocketAddr::V6))
                                            .collect();
                                        link.direct.punch(&addresses);
                                        let _ = link.signals.send((peer, addresses));
                                    }
                                },
                                Event::Revoked | Event::Rejected { error: Code::DeviceRevoked } => return Err(Code::DeviceRevoked),
                                Event::Rejected { error: Code::Unauthorized } => return Err(Code::Unauthorized),
                                _ => return Err(Code::Invalid),
                                }
                            }
                            Message::Ping(_) | Message::Pong(_) => {},
                            _ => return Err(Code::NotConnected),
                        }
                    }
                    Some(result) = observations.recv() => {
                        if mapped != result {
                            mapped = result;
                            let n = send(&mut self.ws, &Request { seq: 0, call: candidates(&host, &mapped, local_ipv6) }).await?;
                            core.traffic.add_control_tx(n);
                        }
                    }
                    _ = registered.tick() => {
                        if let Some((server, probe)) = &self.probe {
                            link.direct.probe(probe, *server);
                        }
                        if last_ping.elapsed() >= HEARTBEAT {
                            last_ping = Instant::now();
                            let n = send(&mut self.ws, &Request { seq: 0, call: Call::Ping }).await?;
                            core.traffic.add_control_tx(n);
                        }
                        pending.retain(|_, (sent, _)| sent.elapsed() < Duration::from_secs(7));
                    }
                    Some((call, ret)) = self.calls.recv() => {
                        if ret.is_closed() { continue; }
                        let n = send(&mut self.ws, &Request { seq, call }).await?;
                        core.traffic.add_control_tx(n);
                        pending.insert(seq, (Instant::now(), ret));
                        seq = seq.wrapping_add(1).max(1);
                    }
                }
            }
        }.await;
        link.direct.close();
        core.link_ended(&link, result.err());
    }
}
