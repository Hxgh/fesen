use crate::local::Log;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    io,
    net::SocketAddrV4,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_util::sync::CancellationToken;

const HISTORY_LEN: usize = 60;
const LEASE_MS: u64 = 15_000;
pub(crate) const ANOMALY_BPS: u64 = 1024 * 1024; // 1 MiB/s sustained threshold

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowKind {
    OutboundTun,
    OutboundForward,
    InboundLocal,
    InboundLan,
}

impl FlowKind {
    pub(crate) fn tag(self) -> &'static str {
        match self {
            Self::OutboundTun => "out_tun",
            Self::OutboundForward => "out_fwd",
            Self::InboundLocal => "in_local",
            Self::InboundLan => "in_lan",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficSample {
    pub ts_ms: u64,
    pub tx_bps: u64,
    pub rx_bps: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficFlow {
    pub id: u64,
    pub kind: FlowKind,
    pub service: String,
    pub service_name: String,
    pub peer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub target: SocketAddrV4,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_port: Option<u16>,
    pub tx_bytes: u64,
    pub rx_bytes: u64,
    pub tx_bps: u64,
    pub rx_bps: u64,
    pub duration_ms: u64,
    pub anomaly: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficSnapshot {
    pub enabled: bool,
    pub ts_ms: u64,
    pub active_flows: usize,
    pub total_tx_bytes: u64,
    pub total_rx_bytes: u64,
    pub total_tx_bps: u64,
    pub total_rx_bps: u64,
    pub control_tx_bytes: u64,
    pub control_rx_bytes: u64,
    pub history: Vec<TrafficSample>,
    pub flows: Vec<TrafficFlow>,
}

#[derive(Clone, Default)]
pub struct ResolvedFlowInfo {
    pub service_name: String,
    pub peer: String,
    pub source: Option<String>,
}

pub struct FlowSpec {
    pub kind: FlowKind,
    pub service: String,
    pub peer_id: String,
    pub src_port: Option<u16>,
    pub target: SocketAddrV4,
    pub local_port: Option<u16>,
    pub cancel: CancellationToken,
}

struct ActiveFlow {
    id: u64,
    kind: FlowKind,
    service: String,
    peer_id: String,
    src_port: Option<u16>,
    target: SocketAddrV4,
    local_port: Option<u16>,
    started: Instant,
    tx: Arc<AtomicU64>,
    rx: Arc<AtomicU64>,
    cancel: CancellationToken,
    resolved: Option<ResolvedFlowInfo>,
}

#[derive(Default)]
struct Sampler {
    last_sample_ms: u64,
    last_total_tx: u64,
    last_total_rx: u64,
    last_flows: HashMap<u64, (u64, u64)>,
    history: VecDeque<TrafficSample>,
}

pub struct TrafficTracker {
    enabled: AtomicBool,
    enabled_until_ms: AtomicU64,
    active_count: AtomicUsize,
    total_tx: AtomicU64,
    total_rx: AtomicU64,
    control_tx: AtomicU64,
    control_rx: AtomicU64,
    next_id: AtomicU64,
    flows: Mutex<HashMap<u64, ActiveFlow>>,
    sampler: Mutex<Sampler>,
    log: Arc<Log>,
}

impl TrafficTracker {
    pub fn new(log: Arc<Log>) -> Self {
        Self {
            enabled: AtomicBool::new(false),
            enabled_until_ms: AtomicU64::new(0),
            active_count: AtomicUsize::new(0),
            total_tx: AtomicU64::new(0),
            total_rx: AtomicU64::new(0),
            control_tx: AtomicU64::new(0),
            control_rx: AtomicU64::new(0),
            next_id: AtomicU64::new(1),
            flows: Mutex::new(HashMap::new()),
            sampler: Mutex::new(Sampler::default()),
            log,
        }
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64
    }

    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn active_count(&self) -> usize {
        self.active_count.load(Ordering::Acquire)
    }

    #[inline]
    pub fn add_control_tx(&self, bytes: u64) {
        if self.is_enabled() {
            self.control_tx.fetch_add(bytes, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn add_control_rx(&self, bytes: u64) {
        if self.is_enabled() {
            self.control_rx.fetch_add(bytes, Ordering::Relaxed);
        }
    }

    fn reset_monitoring(&self) {
        self.enabled.store(false, Ordering::Release);
        self.enabled_until_ms.store(0, Ordering::Release);
        self.total_tx.store(0, Ordering::Relaxed);
        self.total_rx.store(0, Ordering::Relaxed);
        self.control_tx.store(0, Ordering::Relaxed);
        self.control_rx.store(0, Ordering::Relaxed);
        {
            let mut flows = self.flows.lock().unwrap();
            for f in flows.values_mut() {
                f.tx.store(0, Ordering::Relaxed);
                f.rx.store(0, Ordering::Relaxed);
            }
        }
        let mut sampler = self.sampler.lock().unwrap();
        sampler.last_sample_ms = 0;
        sampler.last_total_tx = 0;
        sampler.last_total_rx = 0;
        sampler.last_flows.clear();
        sampler.history.clear();
    }

    pub fn expire_if_idle(&self) {
        if !self.is_enabled() {
            return;
        }
        let until = self.enabled_until_ms.load(Ordering::Acquire);
        if until == 0 || Self::now_ms() > until {
            self.reset_monitoring();
        }
    }

    pub fn register(self: &Arc<Self>, spec: FlowSpec) -> FlowGuard {
        self.active_count.fetch_add(1, Ordering::AcqRel);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let tx = Arc::new(AtomicU64::new(0));
        let rx = Arc::new(AtomicU64::new(0));
        let entry = ActiveFlow {
            id,
            kind: spec.kind,
            service: spec.service,
            peer_id: spec.peer_id,
            src_port: spec.src_port,
            target: spec.target,
            local_port: spec.local_port,
            started: Instant::now(),
            tx: tx.clone(),
            rx: rx.clone(),
            cancel: spec.cancel,
            resolved: None,
        };
        self.flows.lock().unwrap().insert(id, entry);
        FlowGuard {
            tracker: self.clone(),
            id,
            tx,
            rx,
            reported: false,
        }
    }

    pub fn drop_flow(&self, id: u64) -> bool {
        let flows = self.flows.lock().unwrap();
        if let Some(entry) = flows.get(&id) {
            entry.cancel.cancel();
            true
        } else {
            false
        }
    }

    pub fn snapshot(
        &self,
        enable: bool,
        resolve: impl Fn(&str, &str, Option<u16>) -> ResolvedFlowInfo,
    ) -> TrafficSnapshot {
        let now_ms = Self::now_ms();
        let active_flows = self.active_count();
        if !enable {
            self.reset_monitoring();
            return TrafficSnapshot {
                enabled: false,
                ts_ms: now_ms,
                active_flows,
                total_tx_bytes: 0,
                total_rx_bytes: 0,
                total_tx_bps: 0,
                total_rx_bps: 0,
                control_tx_bytes: 0,
                control_rx_bytes: 0,
                history: Vec::new(),
                flows: Vec::new(),
            };
        }

        let was_enabled = self.enabled.swap(true, Ordering::AcqRel);
        self.enabled_until_ms
            .store(now_ms.saturating_add(LEASE_MS), Ordering::Release);

        // Lazily resolve service names, peers, and originating local processes only while open.
        let pending: Vec<(u64, String, String, Option<u16>)> = {
            let flows = self.flows.lock().unwrap();
            flows
                .values()
                .filter(|f| f.resolved.is_none())
                .map(|f| (f.id, f.service.clone(), f.peer_id.clone(), f.src_port))
                .collect()
        };
        if !pending.is_empty() {
            let updates: Vec<(u64, ResolvedFlowInfo)> = pending
                .into_iter()
                .map(|(id, svc, peer_id, src_port)| (id, resolve(&svc, &peer_id, src_port)))
                .collect();
            let mut flows = self.flows.lock().unwrap();
            for (id, info) in updates {
                if let Some(entry) = flows.get_mut(&id) {
                    entry.resolved = Some(info);
                }
            }
        }

        let total_tx_bytes = self.total_tx.load(Ordering::Relaxed);
        let total_rx_bytes = self.total_rx.load(Ordering::Relaxed);
        let control_tx_bytes = self.control_tx.load(Ordering::Relaxed);
        let control_rx_bytes = self.control_rx.load(Ordering::Relaxed);

        let mut sampler = self.sampler.lock().unwrap();
        if !was_enabled {
            sampler.history.clear();
            sampler.last_flows.clear();
            sampler.last_sample_ms = 0;
        }

        let elapsed_ms = if sampler.last_sample_ms > 0 {
            now_ms.saturating_sub(sampler.last_sample_ms)
        } else {
            0
        };
        let (total_tx_bps, total_rx_bps) = if elapsed_ms >= 200 {
            let dtx = total_tx_bytes.saturating_sub(sampler.last_total_tx);
            let drx = total_rx_bytes.saturating_sub(sampler.last_total_rx);
            (
                dtx.saturating_mul(1000) / elapsed_ms,
                drx.saturating_mul(1000) / elapsed_ms,
            )
        } else {
            (0, 0)
        };
        if sampler.last_sample_ms == 0 || elapsed_ms >= 450 {
            sampler.last_sample_ms = now_ms;
            sampler.last_total_tx = total_tx_bytes;
            sampler.last_total_rx = total_rx_bytes;
            sampler.history.push_back(TrafficSample {
                ts_ms: now_ms,
                tx_bps: total_tx_bps,
                rx_bps: total_rx_bps,
            });
            while sampler.history.len() > HISTORY_LEN {
                sampler.history.pop_front();
            }
        }

        let snapshot_flows = {
            let flows = self.flows.lock().unwrap();
            let mut next_prev = HashMap::with_capacity(flows.len());
            let mut list: Vec<TrafficFlow> = flows
                .values()
                .map(|f| {
                    let tx_b = f.tx.load(Ordering::Relaxed);
                    let rx_b = f.rx.load(Ordering::Relaxed);
                    next_prev.insert(f.id, (tx_b, rx_b));
                    let (tx_bps, rx_bps) = if elapsed_ms >= 200 {
                        let (ptx, prx) = sampler.last_flows.get(&f.id).copied().unwrap_or((0, 0));
                        (
                            tx_b.saturating_sub(ptx).saturating_mul(1000) / elapsed_ms,
                            rx_b.saturating_sub(prx).saturating_mul(1000) / elapsed_ms,
                        )
                    } else {
                        (0, 0)
                    };
                    let duration_ms = f.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                    let anomaly = tx_bps.saturating_add(rx_bps) >= ANOMALY_BPS;
                    let info = f.resolved.as_ref();
                    TrafficFlow {
                        id: f.id,
                        kind: f.kind,
                        service: f.service.clone(),
                        service_name: info.map_or_else(String::new, |i| i.service_name.clone()),
                        peer: info.map_or_else(String::new, |i| i.peer.clone()),
                        source: info.and_then(|i| i.source.clone()),
                        target: f.target,
                        local_port: f.local_port,
                        tx_bytes: tx_b,
                        rx_bytes: rx_b,
                        tx_bps,
                        rx_bps,
                        duration_ms,
                        anomaly,
                    }
                })
                .collect();
            if elapsed_ms == 0 || elapsed_ms >= 450 {
                sampler.last_flows = next_prev;
            }
            list.sort_unstable_by(|a, b| {
                (b.tx_bps + b.rx_bps, b.tx_bytes + b.rx_bytes, a.id)
                    .cmp(&(a.tx_bps + a.rx_bps, a.tx_bytes + a.rx_bytes, b.id))
            });
            list
        };

        TrafficSnapshot {
            enabled: true,
            ts_ms: now_ms,
            active_flows,
            total_tx_bytes,
            total_rx_bytes,
            total_tx_bps,
            total_rx_bps,
            control_tx_bytes,
            control_rx_bytes,
            history: sampler.history.iter().cloned().collect(),
            flows: snapshot_flows,
        }
    }
}

pub struct FlowGuard {
    tracker: Arc<TrafficTracker>,
    id: u64,
    tx: Arc<AtomicU64>,
    rx: Arc<AtomicU64>,
    reported: bool,
}

impl FlowGuard {
    pub fn wrap<T: AsyncRead + AsyncWrite + Unpin>(&self, inner: T) -> TrafficStream<T> {
        TrafficStream {
            inner,
            tx: self.tx.clone(),
            rx: self.rx.clone(),
            tracker: self.tracker.clone(),
        }
    }

    pub fn close_stopped(&mut self, peer: impl FnOnce(&str) -> String) {
        self.reported = true;
        let removed = self.tracker.flows.lock().unwrap().remove(&self.id);
        if let Some(entry) = removed {
            let tx = entry.tx.load(Ordering::Relaxed);
            let rx = entry.rx.load(Ordering::Relaxed);
            let dur_ms = entry.started.elapsed().as_millis();
            self.tracker.log.record_info("flow", || {
                let resolved_peer = entry
                    .resolved
                    .as_ref()
                    .map(|r| r.peer.clone())
                    .unwrap_or_else(|| peer(&entry.peer_id));
                let p = if resolved_peer.is_empty() {
                    "-"
                } else {
                    resolved_peer.as_str()
                };
                format!(
                    "dir={} peer={} target={} tx={} rx={} ms={} res=stopped",
                    entry.kind.tag(),
                    p,
                    entry.target,
                    tx,
                    rx,
                    dur_ms,
                )
            });
        }
    }

    pub fn close_error(&mut self, reason: &str, peer: impl FnOnce(&str) -> String) {
        self.reported = true;
        let removed = self.tracker.flows.lock().unwrap().remove(&self.id);
        let Some(entry) = removed else { return };
        let tx = entry.tx.load(Ordering::Relaxed);
        let rx = entry.rx.load(Ordering::Relaxed);
        let dur_ms = entry.started.elapsed().as_millis();
        let resolved_peer = entry
            .resolved
            .as_ref()
            .map(|r| r.peer.clone())
            .unwrap_or_else(|| peer(&entry.peer_id));
        let p = if resolved_peer.is_empty() {
            "-"
        } else {
            resolved_peer.as_str()
        };
        self.tracker.log.record(
            "WARN",
            "flow",
            &format!(
                "dir={} peer={} target={} tx={} rx={} ms={} res={}",
                entry.kind.tag(),
                p,
                entry.target,
                tx,
                rx,
                dur_ms,
                reason
            ),
        );
    }

    fn finish_ok(&self) {
        let removed = self.tracker.flows.lock().unwrap().remove(&self.id);
        let Some(entry) = removed else { return };
        if self.tracker.log.is_verbose() {
            let tx = entry.tx.load(Ordering::Relaxed);
            let rx = entry.rx.load(Ordering::Relaxed);
            let dur_ms = entry.started.elapsed().as_millis();
            if tx > 0 || rx > 0 || dur_ms >= 500 {
                let resolved_peer = entry
                    .resolved
                    .as_ref()
                    .map(|r| r.peer.clone())
                    .unwrap_or_else(|| entry.peer_id.clone());
                let p = if resolved_peer.is_empty() {
                    "-"
                } else {
                    resolved_peer.as_str()
                };
                self.tracker.log.record(
                    "INFO",
                    "flow",
                    &format!(
                        "dir={} peer={} target={} tx={} rx={} ms={} res=ok",
                        entry.kind.tag(),
                        p,
                        entry.target,
                        tx,
                        rx,
                        dur_ms,
                    ),
                );
            }
        }
    }
}

impl Drop for FlowGuard {
    fn drop(&mut self) {
        self.tracker.active_count.fetch_sub(1, Ordering::AcqRel);
        if !self.reported {
            self.finish_ok();
        } else {
            self.tracker.flows.lock().unwrap().remove(&self.id);
        }
    }
}

pub struct TrafficStream<T> {
    inner: T,
    tx: Arc<AtomicU64>,
    rx: Arc<AtomicU64>,
    tracker: Arc<TrafficTracker>,
}

impl<T: AsyncRead + Unpin> AsyncRead for TrafficStream<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if !this.tracker.is_enabled() {
            return Pin::new(&mut this.inner).poll_read(cx, buf);
        }
        let before = buf.filled().len();
        let res = Pin::new(&mut this.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &res {
            let delta = (buf.filled().len() - before) as u64;
            if delta > 0 {
                this.rx.fetch_add(delta, Ordering::Relaxed);
                this.tracker.total_rx.fetch_add(delta, Ordering::Relaxed);
            }
        }
        res
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for TrafficStream<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if !this.tracker.is_enabled() {
            return Pin::new(&mut this.inner).poll_write(cx, buf);
        }
        let res = Pin::new(&mut this.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &res
            && *n > 0
        {
            let delta = *n as u64;
            this.tx.fetch_add(delta, Ordering::Relaxed);
            this.tracker.total_tx.fetch_add(delta, Ordering::Relaxed);
        }
        res
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn tracker_counts_bytes_samples_on_demand_and_clears_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(Log::new(dir.path()));
        let tracker = Arc::new(TrafficTracker::new(log.clone()));

        let resolve_calls = Arc::new(AtomicUsize::new(0));
        let rc = resolve_calls.clone();
        let resolve = move |svc: &str, peer: &str, port: Option<u16>| {
            rc.fetch_add(1, Ordering::Relaxed);
            ResolvedFlowInfo {
                service_name: format!("name:{svc}"),
                peer: peer.to_owned(),
                source: port.map(|p| format!("app :{p}")),
            }
        };

        let idle = tracker.snapshot(false, &resolve);
        assert!(!idle.enabled);
        assert!(idle.flows.is_empty());

        // Connection opened BEFORE enabling traffic monitoring does not resolve metadata or count bytes while closed.
        let cancel = CancellationToken::new();
        let guard = tracker.register(FlowSpec {
            kind: FlowKind::InboundLocal,
            service: "svc1".into(),
            peer_id: "Mac".into(),
            src_port: Some(50123),
            target: "192.0.2.10:22".parse().unwrap(),
            local_port: None,
            cancel: cancel.clone(),
        });
        assert_eq!(resolve_calls.load(Ordering::Relaxed), 0);
        assert_eq!(tracker.active_count(), 1);

        let (mut client, server) = tokio::io::duplex(2048);
        let mut stream = guard.wrap(server);
        client.write_all(b"pre").await.unwrap();
        let mut pbuf = [0u8; 3];
        stream.read_exact(&mut pbuf).await.unwrap();
        assert_eq!(tracker.total_rx.load(Ordering::Relaxed), 0);

        // Opening the traffic panel immediately exposes the pre-existing active flow and starts metering.
        log.set_verbose(true);
        let snap0 = tracker.snapshot(true, &resolve);
        assert!(snap0.enabled);
        assert_eq!(snap0.flows.len(), 1);
        assert_eq!(snap0.flows[0].source.as_deref(), Some("app :50123"));
        assert_eq!(resolve_calls.load(Ordering::Relaxed), 1);

        client.write_all(b"hello-rx").await.unwrap();
        let mut buf = [0u8; 8];
        stream.read_exact(&mut buf).await.unwrap();
        stream.write_all(b"reply").await.unwrap();
        let mut rbuf = [0u8; 5];
        client.read_exact(&mut rbuf).await.unwrap();

        let snap = tracker.snapshot(true, &resolve);
        assert!(snap.enabled);
        assert_eq!(snap.total_rx_bytes, 8);
        assert_eq!(snap.total_tx_bytes, 5);
        assert_eq!(snap.flows.len(), 1);
        assert_eq!(resolve_calls.load(Ordering::Relaxed), 1);

        assert!(tracker.drop_flow(snap.flows[0].id));
        assert!(cancel.is_cancelled());

        drop(stream);
        drop(guard);
        assert_eq!(tracker.active_count(), 0);
        let entries = log.tail(10);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].scope, "flow");
        assert!(entries[0].message.contains("dir=in_local"));
        assert!(entries[0].message.contains("tx=5 rx=8"));

        let off = tracker.snapshot(false, &resolve);
        assert!(!off.enabled);
        assert!(off.history.is_empty());
        assert_eq!(off.total_tx_bytes, 0);
    }
}
