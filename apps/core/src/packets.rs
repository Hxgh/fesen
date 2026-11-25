use crate::{
    node::Core,
    platform::{self, MTU},
};
use futures_util::{SinkExt, StreamExt};
use netstack_smoltcp::StackBuilder;
use fesen_proto::Code;
use std::{
    io,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UdpSocket,
    sync::{Semaphore, mpsc},
    time::timeout,
};

pub fn spawn(core: Arc<Core>) -> io::Result<()> {
    let (stack, driver, udp, tcp) = StackBuilder::default()
        .mtu(MTU)
        .enable_tcp(true)
        .enable_udp(true)
        .build()?;
    let (mut input, mut output) = stack.split();
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(512);
    core.platform.set_input(tx);
    if let Some(driver) = driver {
        let quit = core.quit.clone();
        tokio::spawn(async move {
            tokio::select! { _ = quit.cancelled() => {}, _ = driver => {} }
        });
    }
    {
        let quit = core.quit.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = quit.cancelled() => break,
                    v = rx.recv() => {
                        let Some(packet) = v else { break; };
                        if packet.len() >= 20 && packet.len() <= MTU && packet[0] >> 4 == 4
                            && input.send(packet).await.is_err() { break; }
                    }
                }
            }
        });
    }
    {
        let core = core.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = core.quit.cancelled() => break,
                    v = output.next() => match v {
                        Some(Ok(packet)) => core.platform.write_packet(&packet),
                        _ => break,
                    }
                }
            }
        });
    }
    let mut tcp = tcp.expect("TCP enabled");
    {
        let core = core.clone();
        let slots = Arc::new(Semaphore::new(256));
        tokio::spawn(async move {
            loop {
                let Some((stream, _, remote)) = (tokio::select! { _ = core.quit.cancelled() => None, stream = tcp.next() => stream })
                else {
                    break;
                };
                let SocketAddr::V4(remote) = remote else {
                    continue;
                };
                let Ok(slot) = slots.clone().try_acquire_owned() else {
                    core.report(Code::Busy);
                    continue;
                };
                let core = core.clone();
                let is_dns = remote.port() == 53
                    && core.routes.read().unwrap().tun.dns == Some(*remote.ip());
                tokio::spawn(async move {
                    let _slot = slot;
                    if is_dns {
                        let _ = timeout(Duration::from_secs(8), dns_tcp(&core, stream)).await;
                    } else if let Err(e) = core.forward(stream, remote).await {
                        core.report(e);
                    }
                });
            }
        });
    }
    let (mut queries, mut replies) = udp.expect("UDP enabled").split();
    let (answers, mut outgoing) = mpsc::channel(64);
    {
        let quit = core.quit.clone();
        tokio::spawn(async move {
            loop {
                let reply = tokio::select! { _ = quit.cancelled() => None, reply = outgoing.recv() => reply };
                let Some(reply) = reply else { break };
                if replies.send(reply).await.is_err() {
                    break;
                }
            }
        });
    }
    tokio::spawn(async move {
        let slots = Arc::new(Semaphore::new(32));
        loop {
            let Some((query, from, to)) = (tokio::select! { _ = core.quit.cancelled() => None, query = queries.next() => query })
            else {
                break;
            };
            let SocketAddr::V4(to_v4) = to else { continue };
            if to.port() != 53 || core.routes.read().unwrap().tun.dns != Some(*to_v4.ip()) {
                continue;
            }
            let Ok(slot) = slots.clone().try_acquire_owned() else {
                continue;
            };
            let core = core.clone();
            let answers = answers.clone();
            tokio::spawn(async move {
                let _slot = slot;
                if let Some(answer) = dns(&core, &query, false).await {
                    let _ = answers.try_send((answer, to, from));
                }
            });
        }
    });
    Ok(())
}

async fn dns_tcp(core: &Core, mut stream: netstack_smoltcp::TcpStream) -> io::Result<()> {
    let len = stream.read_u16().await? as usize;
    if !(12..=4096).contains(&len) {
        return Ok(());
    }
    let mut query = vec![0; len];
    stream.read_exact(&mut query).await?;
    if let Some(reply) = dns(core, &query, true).await {
        stream.write_u16(reply.len() as u16).await?;
        stream.write_all(&reply).await?;
    }
    stream.shutdown().await
}

struct Question {
    name: String,
    kind: u16,
    end: usize,
}

fn question(bytes: &[u8]) -> Option<Question> {
    if !(12..=4096).contains(&bytes.len()) || bytes[2] & 0xf8 != 0 || bytes[4..6] != [0, 1] {
        return None;
    }
    let mut name = String::new();
    let mut end = 12;
    loop {
        let length = *bytes.get(end)? as usize;
        end += 1;
        if length == 0 {
            break;
        }
        if length > 63 || end + length > 267 {
            return None;
        }
        let label = std::str::from_utf8(bytes.get(end..end + length)?).ok()?;
        if label.contains('.') {
            return None;
        }
        if !name.is_empty() {
            name.push('.');
        }
        name.push_str(label);
        end += length;
    }
    let data = bytes.get(end..end + 4)?;
    if data[2..] != [0, 1] {
        return None;
    }
    Some(Question {
        name: name.to_ascii_lowercase(),
        kind: u16::from_be_bytes([data[0], data[1]]),
        end: end + 4,
    })
}

fn response(query: &[u8], q: &Question, rcode: u8, answer: Option<Ipv4Addr>) -> Vec<u8> {
    let mut bytes = query[..q.end].to_vec();
    bytes[2] = 0x80 | (query[2] & 1);
    bytes[3] = 0x80 | rcode;
    bytes[6..12].fill(0);
    if let Some(ip) = answer {
        bytes[7] = 1;
        bytes.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 5, 0, 4]);
        bytes.extend_from_slice(&ip.octets());
    }
    bytes
}

async fn dns(core: &Core, query: &[u8], tcp: bool) -> Option<Vec<u8>> {
    let q = question(query)?;
    if q.name == "fesen" || q.name.ends_with(".fesen") {
        let ip = core.routes.read().unwrap().aliases.get(&q.name).copied();
        return Some(response(
            query,
            &q,
            if ip.is_some() { 0 } else { 3 },
            ip.filter(|_| matches!(q.kind, 1 | 255)),
        ));
    }
    // Android may send non-fesen DNS to the VPN; preserve the selected network's resolver.
    for ip in core.network().dns.into_iter().take(3) {
        let result = timeout(Duration::from_millis(1800), async {
            let address = SocketAddrV4::new(ip, 53);
            if tcp {
                let mut stream = platform::tcp(&*core.platform, address)
                    .await
                    .map_err(io::Error::other)?;
                stream.write_u16(query.len() as u16).await?;
                stream.write_all(query).await?;
                let len = stream.read_u16().await? as usize;
                if !(12..=4096).contains(&len) {
                    return Err(io::Error::other("invalid DNS"));
                }
                let mut answer = vec![0; len];
                stream.read_exact(&mut answer).await?;
                return Ok::<_, io::Error>(answer);
            }
            let socket = UdpSocket::from_std(platform::udp(&*core.platform, Some(ip))?)?;
            socket.connect(address).await?;
            socket.send(query).await?;
            let mut answer = vec![0; 4096];
            let len = socket.recv(&mut answer).await?;
            answer.truncate(len);
            if len > MTU - 28 {
                let mut truncated = response(query, &q, 0, None);
                truncated[2] |= 2;
                return Ok(truncated);
            }
            Ok(answer)
        })
        .await;
        if let Ok(Ok(answer)) = result
            && answer.len() >= 12
            && answer[..2] == query[..2]
            && answer[2] & 0x80 != 0
        {
            return Some(answer);
        }
    }
    Some(response(query, &q, 2, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_real_dns_records() {
        let query = b"\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x03app\x05fesen\x00\x00\x01\x00\x01";
        let q = question(query).unwrap();
        assert_eq!(q.name, "app.fesen");
        let answer = response(query, &q, 0, Some("198.18.0.3".parse().unwrap()));
        assert_eq!(answer[7], 1);
        assert_eq!(&answer[answer.len() - 4..], &[198, 18, 0, 3]);
        assert!(question(&query[..20]).is_none());
    }
}
