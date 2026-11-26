use crate::{
    local::Identity,
    platform::{self, Platform},
    stun,
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use fesen_proto::{
    ALPN, CERT_NAME, Code, MAX_CANDIDATES, MAX_IPV6_CANDIDATES, Open, Peer, candidate, candidate6,
    stun_endpoint,
};
use quinn::{
    Connection, Endpoint, RecvStream, Runtime, SendStream,
    crypto::rustls::{QuicClientConfig, QuicServerConfig},
};
use rustls::{
    DigitallySignedStruct, DistinguishedName, RootCertStore, SignatureScheme,
    client::danger::HandshakeSignatureValid,
    crypto::{ring, verify_tls12_signature, verify_tls13_signature},
    pki_types::{CertificateDer, UnixTime},
    server::danger::{ClientCertVerified, ClientCertVerifier},
};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    collections::HashMap,
    io,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, SocketAddrV6, UdpSocket},
    sync::{Arc, Mutex, OnceLock, RwLock},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::Semaphore,
    time::{Instant, timeout},
};
use tokio_util::sync::CancellationToken;

pub(crate) fn crypto_provider() -> Arc<rustls::crypto::CryptoProvider> {
    static PROVIDER: OnceLock<Arc<rustls::crypto::CryptoProvider>> = OnceLock::new();
    PROVIDER
        .get_or_init(|| Arc::new(ring::default_provider()))
        .clone()
}

#[derive(Debug, Clone)]
pub struct Allowed {
    pub certificate: Arc<[u8]>,
    pub cancel: CancellationToken,
}

#[derive(Debug, Default)]
struct Lease {
    peers: HashMap<String, Allowed>,
    by_cert: HashMap<Arc<[u8]>, String>,
    expires: Option<Instant>,
}

#[derive(Debug, Default)]
pub struct Membership(RwLock<Lease>);

impl Membership {
    pub fn renew(&self, peers: Vec<Peer>) {
        let mut lease = self.0.write().unwrap();
        let mut old = std::mem::take(&mut lease.peers);
        lease.by_cert.clear();
        lease.peers.reserve(peers.len());
        lease.by_cert.reserve(peers.len());
        for p in peers {
            let prior = old.remove(&p.id);
            let valid = match prior {
                Some(v)
                    if v.certificate.as_ref() == p.certificate.as_slice()
                        && !v.cancel.is_cancelled() =>
                {
                    v
                }
                stale => {
                    if let Some(v) = stale {
                        v.cancel.cancel();
                    }
                    Allowed {
                        certificate: Arc::from(p.certificate),
                        cancel: CancellationToken::new(),
                    }
                }
            };
            lease.by_cert.insert(valid.certificate.clone(), p.id.clone());
            lease.peers.insert(p.id, valid);
        }
        lease.expires = Some(Instant::now() + fesen_proto::LEASE);
        drop(lease);
        for p in old.into_values() {
            p.cancel.cancel();
        }
    }

    pub fn invalidate(&self) {
        let mut lease = self.0.write().unwrap();
        lease.expires = None;
        lease.by_cert.clear();
        for p in lease.peers.drain().map(|(_, p)| p) {
            p.cancel.cancel();
        }
    }

    pub fn is_fresh(&self) -> bool {
        self.0
            .read()
            .unwrap()
            .expires
            .is_some_and(|expires| Instant::now() < expires)
    }

    pub fn peer(&self, peer: &str) -> Option<Allowed> {
        let lease = self.0.read().unwrap();
        lease.expires.filter(|until| Instant::now() < *until)?;
        lease.peers.get(peer).cloned()
    }

    pub fn certificate(&self, cert: &[u8]) -> Option<Allowed> {
        let lease = self.0.read().unwrap();
        lease.expires.filter(|until| Instant::now() < *until)?;
        let id = lease.by_cert.get(cert)?;
        lease.peers.get(id).cloned()
    }
}

impl ClientCertVerifier for Membership {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        leaf: &CertificateDer<'_>,
        chain: &[CertificateDer<'_>],
        _: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        if chain.is_empty() && self.certificate(leaf.as_ref()).is_some() {
            Ok(ClientCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::UnknownIssuer,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        msg: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            msg,
            cert,
            dss,
            &crypto_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        msg: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            msg,
            cert,
            dss,
            &crypto_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        static SCHEMES: OnceLock<Vec<SignatureScheme>> = OnceLock::new();
        SCHEMES
            .get_or_init(|| {
                crypto_provider()
                    .signature_verification_algorithms
                    .supported_schemes()
            })
            .clone()
    }
}

type ConnectionSlot = Arc<tokio::sync::Mutex<Option<Connection>>>;

fn tune_quic_udp(raw: &UdpSocket) {
    let sock = socket2::SockRef::from(raw);
    #[cfg(target_os = "android")]
    const TARGETS: &[usize] = &[2 * 1024 * 1024, 1024 * 1024, 512 * 1024];
    #[cfg(not(target_os = "android"))]
    const TARGETS: &[usize] = &[4 * 1024 * 1024, 2 * 1024 * 1024, 1024 * 1024, 512 * 1024];

    let cur_rcv = sock.recv_buffer_size().unwrap_or(0);
    for &size in TARGETS {
        if cur_rcv >= size || sock.set_recv_buffer_size(size).is_ok() {
            break;
        }
    }
    let cur_snd = sock.send_buffer_size().unwrap_or(0);
    for &size in TARGETS {
        if cur_snd >= size || sock.set_send_buffer_size(size).is_ok() {
            break;
        }
    }
}

struct Path {
    endpoint: Endpoint,
    raw: UdpSocket,
    address: SocketAddr,
    stun: Option<Arc<stun::Socket>>,
}

impl Path {
    fn new(raw: UdpSocket, config: quinn::ServerConfig, observe: bool) -> io::Result<Self> {
        tune_quic_udp(&raw);
        let address = raw.local_addr()?;
        let runtime = Arc::new(quinn::TokioRuntime);
        let socket = runtime.wrap_udp_socket(raw.try_clone()?)?;
        let stun = observe.then(|| stun::Socket::new(socket.clone()));
        let endpoint = Endpoint::new_with_abstract_socket(
            quinn::EndpointConfig::default(),
            Some(config),
            stun.as_ref().map_or(socket, |s| s.clone()),
            runtime,
        )?;
        Ok(Self {
            endpoint,
            raw,
            address,
            stun,
        })
    }

    fn port(&self) -> u16 {
        self.address.port()
    }

    fn matches(&self, address: SocketAddr) -> bool {
        self.address.is_ipv4() == address.is_ipv4()
    }

    async fn observe(&self, server: SocketAddrV4) -> Option<SocketAddrV4> {
        self.stun.as_ref()?.observe(&self.raw, server).await
    }
}

pub struct Direct {
    primary: Path,
    alternate: Option<(Path, Vec<Ipv4Addr>)>,
    ipv6: Option<(Path, Vec<SocketAddrV6>)>,
    identity: Identity,
    pub membership: Arc<Membership>,
    pub cancel: CancellationToken,
    pub slots: Semaphore,
    connections: Mutex<HashMap<String, ConnectionSlot>>,
}

fn transport() -> Arc<quinn::TransportConfig> {
    static CONFIG: OnceLock<Arc<quinn::TransportConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            #[cfg(target_os = "android")]
            const STREAM_WINDOW: u32 = 2 * 1024 * 1024;
            #[cfg(not(target_os = "android"))]
            const STREAM_WINDOW: u32 = 4 * 1024 * 1024;

            #[cfg(target_os = "android")]
            const CONN_WINDOW: u32 = 8 * 1024 * 1024;
            #[cfg(not(target_os = "android"))]
            const CONN_WINDOW: u32 = 16 * 1024 * 1024;

            let mut config = quinn::TransportConfig::default();
            config.max_concurrent_bidi_streams(128u32.into());
            config.max_concurrent_uni_streams(0u32.into());
            config.stream_receive_window(STREAM_WINDOW.into());
            config.receive_window(CONN_WINDOW.into());
            config.send_window(u64::from(CONN_WINDOW));
            config.initial_rtt(Duration::from_millis(100));
            config.datagram_receive_buffer_size(None);
            config.max_idle_timeout(Some(
                quinn::IdleTimeout::try_from(Duration::from_secs(35)).unwrap(),
            ));
            config.keep_alive_interval(Some(Duration::from_secs(12)));
            Arc::new(config)
        })
        .clone()
}

impl Direct {
    pub fn new(
        identity: Identity,
        platform: &dyn Platform,
        cancel: CancellationToken,
        observe: bool,
    ) -> anyhow::Result<Arc<Self>> {
        let membership = Arc::new(Membership::default());
        let mut tls = rustls::ServerConfig::builder_with_provider(crypto_provider())
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_client_cert_verifier(membership.clone())
            .with_single_cert(identity.chain(), identity.key())?;
        tls.alpn_protocols = vec![ALPN.to_vec()];
        tls.send_tls13_tickets = 0;
        let mut config =
            quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls)?));
        config.transport = transport();
        let primary = Path::new(platform::udp(platform, None)?, config.clone(), observe)?;
        let alternate = platform.alternate_udp().and_then(|(raw, addresses)| {
            Path::new(raw, config.clone(), observe)
                .ok()
                .map(|path| (path, addresses))
        });
        let ipv6 = platform.ipv6_udp().and_then(|(raw, addresses)| {
            let port = raw.local_addr().ok()?.port();
            let mut addresses: Vec<_> = addresses
                .into_iter()
                .map(|ip| SocketAddrV6::new(ip, port, 0, 0))
                .filter(|&addr| candidate6(addr))
                .collect();
            addresses.sort_unstable();
            addresses.dedup();
            addresses.truncate(MAX_IPV6_CANDIDATES);
            if addresses.is_empty() {
                return None;
            }
            Some((Path::new(raw, config, false).ok()?, addresses))
        });
        Ok(Arc::new(Self {
            primary,
            alternate,
            ipv6,
            identity,
            membership,
            cancel,
            slots: Semaphore::new(256),
            connections: Mutex::new(HashMap::new()),
        }))
    }

    #[cfg(test)]
    pub fn local_port(&self) -> u16 {
        self.primary.port()
    }

    pub fn candidates(&self, primary: Vec<Ipv4Addr>) -> Vec<SocketAddrV4> {
        let alternate = self
            .alternate
            .iter()
            .flat_map(|(path, ips)| ips.iter().map(|&ip| SocketAddrV4::new(ip, path.port())));
        let port = self.primary.port();
        primary
            .into_iter()
            .map(|ip| SocketAddrV4::new(ip, port))
            .chain(alternate)
            .filter(|&addr| candidate(addr))
            .take(MAX_CANDIDATES)
            .collect()
    }

    pub fn candidates6(&self) -> &[SocketAddrV6] {
        self.ipv6.as_ref().map_or(&[], |(_, addresses)| addresses)
    }

    fn paths(&self) -> impl Iterator<Item = &Path> {
        std::iter::once(&self.primary)
            .chain(self.alternate.iter().map(|(path, _)| path))
            .chain(self.ipv6.iter().map(|(path, _)| path))
    }

    pub async fn discover(&self, endpoint: &str) -> Vec<SocketAddrV4> {
        let Some((host, port)) = stun_endpoint(endpoint) else {
            return vec![];
        };
        let Ok(Ok(mut hosts)) = timeout(
            Duration::from_secs(1),
            tokio::net::lookup_host((host, port)),
        )
        .await
        else {
            return vec![];
        };
        let Some(server) = hosts.find_map(|a| match a {
            SocketAddr::V4(a)
                if !a.ip().is_unspecified() && !a.ip().is_multicast() && !a.ip().is_broadcast() =>
            {
                Some(a)
            }
            _ => None,
        }) else {
            return vec![];
        };
        let alternate = async { self.alternate.as_ref()?.0.observe(server).await };
        let (primary, alternate) = tokio::join!(self.primary.observe(server), alternate);
        [primary, alternate].into_iter().flatten().collect()
    }

    pub fn probe(&self, data: &[u8], to: SocketAddrV4) {
        let _ = self.primary.raw.send_to(data, to);
    }

    pub fn punch(self: &Arc<Self>, candidates: &[SocketAddr]) {
        if candidates.is_empty() {
            return;
        }
        let candidates = candidates.to_vec();
        let this = self.clone();
        tokio::spawn(async move {
            for _ in 0..5 {
                if this.cancel.is_cancelled() {
                    break;
                }
                for path in this.paths() {
                    for &a in candidates.iter().filter(|&&a| path.matches(a)) {
                        let _ = path.raw.send_to(b"fesen-hole/1", a);
                    }
                }
                tokio::time::sleep(Duration::from_millis(130)).await;
            }
        });
    }

    pub fn close(&self) {
        self.cancel.cancel();
        self.membership.invalidate();
        for path in self.paths() {
            path.endpoint.close(0u32.into(), b"closed");
        }
        self.connections.lock().unwrap().clear();
    }

    pub fn renew(&self, peers: Vec<Peer>) {
        self.membership.renew(peers);
        self.connections.lock().unwrap().retain(|id, slot| {
            if self.membership.peer(id).is_some() {
                return true;
            }
            if let Ok(mut slot) = slot.try_lock()
                && let Some(conn) = slot.take()
            {
                conn.close(0u32.into(), b"revoked");
            }
            false
        });
    }

    pub async fn accept(&self) -> Option<quinn::Incoming> {
        async fn optional(path: Option<&Path>) -> Option<quinn::Incoming> {
            match path {
                Some(path) => path.endpoint.accept().await,
                None => std::future::pending().await,
            }
        }
        tokio::select! {
            _ = self.cancel.cancelled() => None,
            incoming = self.primary.endpoint.accept() => incoming,
            incoming = optional(self.alternate.as_ref().map(|(path, _)| path)) => incoming,
            incoming = optional(self.ipv6.as_ref().map(|(path, _)| path)) => incoming,
        }
    }

    pub fn identify(&self, conn: &Connection) -> Option<Allowed> {
        let chain = conn
            .peer_identity()?
            .downcast::<Vec<CertificateDer<'static>>>()
            .ok()?;
        self.membership.certificate(chain.first()?.as_ref())
    }

    pub async fn connect<F, Fut>(
        self: &Arc<Self>,
        peer: &str,
        mut candidates: F,
    ) -> Result<(Connection, Allowed), Code>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<Vec<SocketAddr>, Code>>,
    {
        let slot = {
            let mut connections = self.connections.lock().unwrap();
            if let Some(slot) = connections.get(peer) {
                slot.clone()
            } else {
                let slot = ConnectionSlot::default();
                connections.insert(peer.to_owned(), slot.clone());
                slot
            }
        };
        let mut slot = slot.lock().await;
        let allowed = self.membership.peer(peer).ok_or(Code::Offline)?;
        if let Some(existing) = slot
            .as_ref()
            .filter(|c| c.close_reason().is_none() && self.identify(c).is_some())
        {
            return Ok((existing.clone(), allowed));
        }
        let conn = tokio::select! {
            _ = self.cancel.cancelled() => return Err(Code::NotConnected),
            _ = allowed.cancel.cancelled() => return Err(Code::Unauthorized),
            result = async {
                let addresses = candidates().await?;
                let result = self.connect_candidates(&allowed.certificate, &addresses).await;
                if self.primary.stun.is_some() && matches!(result, Err(Code::DirectFailed)) {
                    let updated = candidates().await?;
                    if updated != addresses {
                        return self.connect_candidates(&allowed.certificate, &updated).await;
                    }
                }
                result
            } => result?,
        };
        *slot = Some(conn.clone());
        Ok((conn, allowed))
    }

    async fn connect_candidates(
        &self,
        cert: &[u8],
        candidates: &[SocketAddr],
    ) -> Result<Connection, Code> {
        let mut roots = RootCertStore::empty();
        roots
            .add(CertificateDer::from(cert.to_vec()))
            .map_err(|_| Code::Unauthorized)?;
        let mut tls = rustls::ClientConfig::builder_with_provider(crypto_provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|_| Code::Internal)?
            .with_root_certificates(roots)
            .with_client_auth_cert(self.identity.chain(), self.identity.key())
            .map_err(|_| Code::Internal)?;
        tls.alpn_protocols = vec![ALPN.to_vec()];
        let mut config = quinn::ClientConfig::new(Arc::new(
            QuicClientConfig::try_from(tls).map_err(|_| Code::Internal)?,
        ));
        config.transport_config(transport());
        let mut racers = FuturesUnordered::new();
        for path in self.paths() {
            for &candidate in candidates.iter().filter(|&&a| path.matches(a)) {
                if let Ok(race) = path
                    .endpoint
                    .connect_with(config.clone(), candidate, CERT_NAME)
                {
                    racers.push(race);
                }
            }
        }
        timeout(Duration::from_secs(7), async {
            while let Some(result) = racers.next().await {
                if let Ok(conn) = result {
                    let verified = conn
                        .peer_identity()
                        .and_then(|key| key.downcast::<Vec<CertificateDer<'static>>>().ok())
                        .is_some_and(|chain| {
                            chain.first().is_some_and(|leaf| leaf.as_ref() == cert)
                        });
                    if verified {
                        return Ok(conn);
                    }
                    conn.close(0u32.into(), b"invalid identity");
                }
            }
            Err(Code::DirectFailed)
        })
        .await
        .map_err(|_| Code::DirectFailed)?
    }
}

pub async fn frame_in<T: DeserializeOwned>(stream: &mut (impl AsyncRead + Unpin)) -> io::Result<T> {
    let count = stream.read_u32().await? as usize;
    if count == 0 || count > 4096 {
        return Err(io::Error::other("invalid frame"));
    }
    if count <= 256 {
        let mut stack = [0u8; 256];
        stream.read_exact(&mut stack[..count]).await?;
        serde_json::from_slice(&stack[..count]).map_err(io::Error::other)
    } else {
        let mut data = vec![0; count];
        stream.read_exact(&mut data).await?;
        serde_json::from_slice(&data).map_err(io::Error::other)
    }
}

pub async fn frame_out<T: Serialize>(
    stream: &mut (impl AsyncWrite + Unpin),
    body: &T,
) -> io::Result<()> {
    let mut buf = Vec::with_capacity(64);
    buf.extend_from_slice(&[0u8; 4]);
    serde_json::to_writer(&mut buf, body).map_err(io::Error::other)?;
    let len = buf.len() - 4;
    if len > 4096 {
        return Err(io::Error::other("invalid frame"));
    }
    buf[..4].copy_from_slice(&(len as u32).to_be_bytes());
    stream.write_all(&buf).await?;
    stream.flush().await
}

pub async fn open(conn: &Connection, req: Open) -> Result<(SendStream, RecvStream), Code> {
    timeout(Duration::from_secs(9), async {
        let (mut tx, mut rx) = conn.open_bi().await.map_err(|_| Code::DirectFailed)?;
        frame_out(&mut tx, &req)
            .await
            .map_err(|_| Code::DirectFailed)?;
        if let Some(error) = frame_in::<Option<Code>>(&mut rx)
            .await
            .map_err(|_| Code::DirectFailed)?
        {
            return Err(error);
        }
        Ok((tx, rx))
    })
    .await
    .map_err(|_| Code::DirectFailed)?
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DualPath;

    impl Platform for DualPath {
        fn network(&self) -> io::Result<platform::Network> {
            Ok(platform::Network::default())
        }
        fn protect(&self, _: &socket2::Socket, _: Option<Ipv4Addr>) -> io::Result<()> {
            Ok(())
        }
        fn alternate_udp(&self) -> Option<(UdpSocket, Vec<Ipv4Addr>)> {
            Some((
                platform::datagram(|_| Ok(())).ok()?,
                vec![Ipv4Addr::new(192, 168, 3, 9)],
            ))
        }
        fn ipv6_udp(&self) -> Option<(UdpSocket, Vec<std::net::Ipv6Addr>)> {
            Some((
                platform::datagram6(|_| Ok(())).ok()?,
                vec!["2001:db8::1".parse().unwrap()],
            ))
        }
        fn apply(&self, _: &platform::Tun) -> io::Result<()> {
            Ok(())
        }
        fn write_packet(&self, _: &[u8]) {}
        fn set_input(&self, _: tokio::sync::mpsc::Sender<Vec<u8>>) {}
    }

    #[tokio::test]
    async fn alternate_candidates_advertise_and_accept_on_their_own_socket() {
        let origin = Identity::new().unwrap();
        let target = Identity::new().unwrap();
        let sender =
            Direct::new(origin.clone(), &DualPath, CancellationToken::new(), false).unwrap();
        let receiver =
            Direct::new(target.clone(), &DualPath, CancellationToken::new(), false).unwrap();
        receiver.renew(vec![Peer {
            id: "sender".into(),
            certificate: origin.cert,
        }]);
        sender.renew(vec![Peer {
            id: "receiver".into(),
            certificate: target.cert,
        }]);
        let candidates = receiver.candidates(vec![Ipv4Addr::new(10, 1, 0, 1)]);
        assert_eq!(candidates.len(), 2);
        assert_ne!(candidates[0].port(), candidates[1].port());
        assert_eq!(*candidates[1].ip(), Ipv4Addr::new(192, 168, 3, 9));
        let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, candidates[1].port());
        let accepting = async {
            let connection = receiver.accept().await.unwrap().await.unwrap();
            assert!(receiver.identify(&connection).is_some());
            connection
        };
        let dialing = sender.connect("receiver", || async { Ok(vec![address.into()]) });
        let (outgoing, _) = timeout(Duration::from_secs(5), async {
            tokio::join!(dialing, accepting)
        })
        .await
        .unwrap();
        assert_eq!(
            outgoing.unwrap().0.remote_address(),
            SocketAddr::V4(address)
        );
        sender.close();
        receiver.close();
    }

    #[tokio::test]
    async fn ipv6_transport_authenticates_and_carries_an_ipv4_target() {
        let caller = Identity::new().unwrap();
        let publisher = Identity::new().unwrap();
        let sender =
            Direct::new(caller.clone(), &DualPath, CancellationToken::new(), false).unwrap();
        let receiver = Direct::new(
            publisher.clone(),
            &DualPath,
            CancellationToken::new(),
            false,
        )
        .unwrap();
        sender.renew(vec![Peer {
            id: "publisher".into(),
            certificate: publisher.cert,
        }]);
        receiver.renew(vec![Peer {
            id: "caller".into(),
            certificate: caller.cert,
        }]);
        let advertised = receiver.candidates6();
        assert_eq!(advertised.len(), 1);
        assert_eq!(advertised[0].ip().to_string(), "2001:db8::1");
        let loopback = SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, advertised[0].port()));
        let target = "198.51.100.10:8080".parse().unwrap();
        let publisher = async {
            let conn = receiver.accept().await.unwrap().await.unwrap();
            assert!(receiver.identify(&conn).is_some());
            let (mut tx, mut rx) = conn.accept_bi().await.unwrap();
            assert_eq!(frame_in::<Open>(&mut rx).await.unwrap().target, target);
            frame_out(&mut tx, &Option::<Code>::None).await.unwrap();
            tx.finish().unwrap();
            tx.stopped().await.unwrap();
        };
        let caller = async {
            let (conn, _) = sender
                .connect("publisher", || async { Ok(vec![loopback]) })
                .await
                .unwrap();
            assert_eq!(conn.remote_address(), loopback);
            let (_, mut rx) = open(
                &conn,
                Open {
                    service: "test".into(),
                    target,
                },
            )
            .await
            .unwrap();
            assert_eq!(rx.read(&mut [0]).await.unwrap(), None);
        };
        timeout(Duration::from_secs(5), async {
            tokio::join!(publisher, caller)
        })
        .await
        .unwrap();
        sender.close();
        receiver.close();
    }

    #[test]
    fn expired_roster_cannot_authorize_new_traffic() {
        let membership = Membership::default();
        let cert = vec![1, 2, 3];
        membership.renew(vec![Peer {
            id: "member".into(),
            certificate: cert.clone(),
        }]);
        let allowed = membership.certificate(&cert).unwrap();
        membership.0.write().unwrap().expires = Some(Instant::now());
        assert!(membership.certificate(&cert).is_none());
        assert!(membership.peer("member").is_none());
        membership.invalidate();
        assert!(allowed.cancel.is_cancelled());
    }

    #[tokio::test]
    async fn quic_bulk_stream_throughput_and_o1_membership_lookup() {
        let sender_id = Identity::new().unwrap();
        let receiver_id = Identity::new().unwrap();
        let sender = Direct::new(sender_id.clone(), &DualPath, CancellationToken::new(), false).unwrap();
        let receiver = Direct::new(receiver_id.clone(), &DualPath, CancellationToken::new(), false).unwrap();

        let mut peers = Vec::with_capacity(128);
        peers.push(Peer {
            id: "sender".into(),
            certificate: sender_id.cert.clone(),
        });
        for i in 1..128u16 {
            let mut fake = vec![0u8; 128];
            fake[0..2].copy_from_slice(&i.to_be_bytes());
            peers.push(Peer {
                id: format!("peer-{i}"),
                certificate: fake,
            });
        }
        receiver.renew(peers);
        sender.renew(vec![Peer {
            id: "receiver".into(),
            certificate: receiver_id.cert,
        }]);
        assert!(receiver.membership.certificate(&sender_id.cert).is_some());

        let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, receiver.local_port());
        const TOTAL: usize = 8 * 1024 * 1024;
        const CHUNK: usize = 64 * 1024;

        let server_task = tokio::spawn({
            let receiver = receiver.clone();
            async move {
                let conn = receiver.accept().await.unwrap().await.unwrap();
                let (mut tx, mut rx) = conn.accept_bi().await.unwrap();
                let _open: Open = frame_in(&mut rx).await.unwrap();
                frame_out(&mut tx, &Option::<Code>::None).await.unwrap();
                let payload = vec![0x5au8; CHUNK];
                let mut sent = 0;
                while sent < TOTAL {
                    tx.write_all(&payload).await.unwrap();
                    sent += CHUNK;
                }
                tx.finish().unwrap();
                let _ = tx.stopped().await;
            }
        });

        let (conn, _) = sender
            .connect("receiver", || async { Ok(vec![address.into()]) })
            .await
            .unwrap();
        let started = Instant::now();
        let (_, mut rx) = open(
            &conn,
            Open {
                service: "bench".into(),
                target: "198.51.100.10:80".parse().unwrap(),
            },
        )
        .await
        .unwrap();
        let mut buf = vec![0u8; CHUNK];
        let mut received = 0;
        while let Some(n) = rx.read(&mut buf).await.unwrap() {
            received += n;
        }
        let elapsed = started.elapsed();
        server_task.await.unwrap();
        assert_eq!(received, TOTAL);
        let mbps = (TOTAL as f64 / (1024.0 * 1024.0)) / elapsed.as_secs_f64();
        eprintln!("QUIC loopback 8 MiB transfer: {elapsed:?} ({mbps:.1} MiB/s)");
        sender.close();
        receiver.close();
    }
}
