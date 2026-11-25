use fesen_proto::candidate;
use quinn::{
    AsyncUdpSocket, UdpPoller,
    udp::{RecvMeta, Transmit},
};
use std::{
    io::{self, IoSliceMut},
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};
use tokio::{sync::oneshot, time::timeout};

const COOKIE: [u8; 4] = [0x21, 0x12, 0xa4, 0x42];

fn request(id: [u8; 12]) -> [u8; 20] {
    let mut packet = [0; 20];
    packet[1] = 1;
    packet[4..8].copy_from_slice(&COOKIE);
    packet[8..].copy_from_slice(&id);
    packet
}

fn mapped(packet: &[u8], id: [u8; 12]) -> Option<SocketAddrV4> {
    if packet.len() < 20
        || packet[..2] != [1, 1]
        || packet[4..8] != COOKIE
        || packet[8..20] != id
        || usize::from(u16::from_be_bytes([packet[2], packet[3]])) != packet.len() - 20
    {
        return None;
    }
    let mut attrs = &packet[20..];
    let mut plain = None;
    let mut xor = None;
    while !attrs.is_empty() {
        let header = attrs.get(..4)?;
        let kind = u16::from_be_bytes([header[0], header[1]]);
        let len = usize::from(u16::from_be_bytes([header[2], header[3]]));
        let value = attrs.get(4..4 + len)?;
        attrs = attrs.get(4 + ((len + 3) & !3)..)?;
        if value.len() == 8 && value[..2] == [0, 1] && matches!(kind, 1 | 0x20) {
            let mut port = u16::from_be_bytes([value[2], value[3]]);
            let mut ip = [value[4], value[5], value[6], value[7]];
            if kind == 0x20 {
                port ^= u16::from_be_bytes([COOKIE[0], COOKIE[1]]);
                for (octet, mask) in ip.iter_mut().zip(COOKIE) {
                    *octet ^= mask;
                }
            }
            let address = SocketAddrV4::new(Ipv4Addr::from(ip), port);
            if candidate(address) {
                if kind == 0x20 {
                    xor = Some(address);
                } else {
                    plain = Some(address);
                }
            }
        }
    }
    xor.or(plain)
}

#[derive(Debug)]
struct Pending {
    server: SocketAddrV4,
    id: [u8; 12],
    reply: oneshot::Sender<SocketAddrV4>,
}

#[derive(Debug)]
pub struct Socket {
    inner: Arc<dyn AsyncUdpSocket>,
    pending: Mutex<Option<Pending>>,
}

impl Socket {
    pub fn new(inner: Arc<dyn AsyncUdpSocket>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            pending: Mutex::new(None),
        })
    }

    pub async fn observe(&self, raw: &UdpSocket, server: SocketAddrV4) -> Option<SocketAddrV4> {
        let id = rand::random();
        let (reply, mut receive) = oneshot::channel();
        *self.pending.lock().unwrap() = Some(Pending { server, id, reply });
        for wait in [500, 800] {
            if raw.send_to(&request(id), server).is_err() {
                break;
            }
            if let Ok(reply) = timeout(Duration::from_millis(wait), &mut receive).await {
                return reply.ok();
            }
        }
        self.pending.lock().unwrap().take();
        None
    }

    fn receive(&self, addr: SocketAddr, packet: &[u8]) -> bool {
        if packet.len() < 20 || packet[4..8] != COOKIE {
            return false;
        }
        let mut pending = self.pending.lock().unwrap();
        if let Some(probe) = pending.as_ref()
            && addr == SocketAddr::V4(probe.server)
            && let Some(address) = mapped(packet, probe.id)
        {
            let _ = pending.take().unwrap().reply.send(address);
            true
        } else {
            false
        }
    }
}

impl AsyncUdpSocket for Socket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        self.inner.clone().create_io_poller()
    }
    fn try_send(&self, transmit: &Transmit<'_>) -> io::Result<()> {
        self.inner.try_send(transmit)
    }
    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        let result = self.inner.poll_recv(cx, bufs, meta);
        if let Poll::Ready(Ok(n)) = result {
            // Quinn stays the sole reader of the QUIC socket, including on GRO paths.
            for (buf, meta) in bufs.iter().zip(&mut meta[..n]) {
                if let Some(data) = buf.get(..meta.len)
                    && meta.stride > 0
                {
                    if data.len() <= meta.stride {
                        if self.receive(meta.addr, data) {
                            meta.len = 0;
                        }
                    } else {
                        for packet in data.chunks(meta.stride) {
                            self.receive(meta.addr, packet);
                        }
                    }
                }
            }
        }
        result
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }
    fn max_transmit_segments(&self) -> usize {
        self.inner.max_transmit_segments()
    }
    fn max_receive_segments(&self) -> usize {
        self.inner.max_receive_segments()
    }
    fn may_fragment(&self) -> bool {
        self.inner.may_fragment()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_accepts_only_matching_ipv4_response() {
        let id = [42; 12];
        let mut packet = request(id).to_vec();
        packet[0] = 1;
        packet[3] = 12;
        packet.extend_from_slice(&[0, 0x20, 0, 8, 0, 1, 0x30, 0x96, 0xe7, 0x21, 0xc0, 0x6a]);
        assert_eq!(
            mapped(&packet, id),
            Some("198.51.100.40:4484".parse().unwrap())
        );
        assert_eq!(mapped(&packet, [0; 12]), None);
        assert_eq!(mapped(&packet[..31], id), None);
        packet[25] = 2;
        assert_eq!(mapped(&packet, id), None);
    }
}
