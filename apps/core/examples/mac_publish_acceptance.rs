use http_body_util::{BodyExt, Empty, Limited};
use hyper::{Request, body::Bytes, client::conn::http1};
use hyper_util::rt::TokioIo;
use fesen_core::{
    Command, Core,
    ipc::Endpoint,
    platform::{Network, Platform, Tun},
};
use fesen_proto::Code;
use std::{
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, UdpSocket},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Peer(Ipv4Addr, Option<Ipv6Addr>);

impl Platform for Peer {
    fn network(&self) -> io::Result<Network> {
        Ok(Network {
            marker: "local-acceptance".into(),
            addresses: vec![self.0],
            ..Network::default()
        })
    }
    fn protect(&self, _: &socket2::Socket, target: Option<Ipv4Addr>) -> io::Result<()> {
        if target.is_some_and(|ip| !ip.is_loopback()) {
            return Err(io::Error::other("test peer cannot open local TCP"));
        }
        Ok(())
    }
    fn ipv6_udp(&self) -> Option<(UdpSocket, Vec<Ipv6Addr>)> {
        let ip = self.1?;
        let socket = UdpSocket::bind((ip, 0)).ok()?;
        socket.set_nonblocking(true).ok()?;
        Some((socket, vec![ip]))
    }
    fn apply(&self, _: &Tun) -> io::Result<()> {
        Ok(())
    }
    fn write_packet(&self, _: &[u8]) {}
    fn set_input(&self, _: tokio::sync::mpsc::Sender<Vec<u8>>) {}
}

async fn wait_for(path: &Path) {
    while !path.exists() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn probe(core: &Arc<Core>, target: SocketAddrV4) -> anyhow::Result<Option<u16>> {
    let id = loop {
        if let Some(share) = core
            .status()
            .services
            .into_iter()
            .find(|s| s.service.rules.allows(*target.ip(), target.port()))
        {
            break share.service.id;
        }
        tokio::time::sleep(Duration::from_millis(75)).await;
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let op = |port| Command::TestConnection {
        id: id.clone(),
        ip: *target.ip(),
        port,
    };
    anyhow::ensure!(matches!(core.execute(op(0)).await, Err(Code::Unauthorized)));
    Ok(core.execute(op(target.port())).await?.http_status)
}

async fn http_check(
    core: Arc<Core>,
    target: SocketAddrV4,
    reference: Option<Vec<u8>>,
) -> anyhow::Result<()> {
    let status = probe(&core, target).await?;
    if reference.is_some() {
        anyhow::ensure!(status == Some(200), "HTTP probe: {status:?}");
    }
    let (request, remote) = tokio::io::duplex(8192);
    let job = tokio::spawn(async move { core.forward(remote, target).await });
    let (mut client, connection) = http1::handshake(TokioIo::new(request)).await?;
    let driver = tokio::spawn(connection);
    let response = client
        .send_request(
            Request::get("/")
                .header("host", target.to_string())
                .header("accept-encoding", "identity")
                .header("connection", "close")
                .body(Empty::<Bytes>::new())?,
        )
        .await?;
    anyhow::ensure!(
        response.status() == 200,
        "HTTP request: {}",
        response.status()
    );
    let body = Limited::new(response.into_body(), 65536)
        .collect()
        .await
        .map_err(anyhow::Error::from_boxed)?
        .to_bytes();
    if let Some(reference) = reference {
        anyhow::ensure!(
            !reference.is_empty() && body.as_ref() == reference,
            "HTTP body mismatch"
        );
    }
    drop(client);
    driver.abort();
    job.abort();
    println!("REAL_HTTP_200");
    println!("HTTP_BODY_BYTES {}", body.len());
    Ok(())
}

async fn persistent(core: Arc<Core>, target: SocketAddrV4, work: PathBuf) -> anyhow::Result<()> {
    probe(&core, target).await?;
    let (mut app, rpc) = tokio::io::duplex(4096);
    let task = tokio::spawn(async move { core.forward(rpc, target).await });
    let mut reply = [0; 4];
    app.write_all(b"hold").await?;
    app.read_exact(&mut reply).await?;
    anyhow::ensure!(&reply == b"hold");
    println!("FLOW_OPEN");
    wait_for(&work.join("closed")).await;
    app.write_all(b"kept").await?;
    app.read_exact(&mut reply).await?;
    anyhow::ensure!(&reply == b"kept");
    println!("FLOW_ALIVE_AFTER_GUI_CLOSE");
    anyhow::ensure!(app.read(&mut reply).await? == 0);
    task.await??;
    println!("FLOW_CLOSED_AFTER_UNPUBLISH");
    Ok(())
}

fn lab_target() -> anyhow::Result<SocketAddrV4> {
    if let Ok(addr) = std::env::var("FESEN_LAB_TARGET") {
        return Ok(addr.parse()?);
    }
    let ip: Ipv4Addr = std::env::var("FESEN_LAB_TARGET_IP")
        .map_err(|_| anyhow::anyhow!("需要设置 FESEN_LAB_TARGET 或 FESEN_LAB_TARGET_IP/PORT"))?
        .parse()?;
    let port: u16 = std::env::var("FESEN_LAB_TARGET_PORT")
        .map_err(|_| anyhow::anyhow!("需要设置 FESEN_LAB_TARGET_PORT"))?
        .parse()?;
    Ok(SocketAddrV4::new(ip, port))
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let work = PathBuf::from(args.next().unwrap());
    let peer: Ipv4Addr = args.next().unwrap().parse()?;
    let mode = args.next().unwrap();
    let ipv6 = args.next().map(|ip| ip.parse::<Ipv6Addr>()).transpose()?;
    let target = lab_target()?;
    let dir = work.join("reader");
    let endpoint = Endpoint::bind(&dir).await?;
    let core = Core::new(dir, Arc::new(Peer(peer, ipv6)), true)?;
    let server = tokio::spawn(endpoint.serve(core.clone()));
    let lab = async {
        if mode == "--http-only" {
            let mut reference = vec![];
            io::Read::read_to_end(&mut io::Read::take(io::stdin(), 65537), &mut reference)?;
            anyhow::ensure!(reference.len() <= 65536, "reference too large");
            http_check(core.clone(), target, Some(reference)).await
        } else {
            http_check(core.clone(), target, None).await?;
            persistent(core.clone(), mode.parse()?, work).await
        }
    };
    let result = tokio::time::timeout(Duration::from_secs(100), lab).await;
    core.stop();
    server.await?;
    result??;
    Ok(())
}
