use super::{Core, types::COPY_BUF};
use crate::{
    control::Link,
    direct::{self, Allowed},
    platform, probe,
    routing::Destination,
    traffic::{FlowKind, FlowSpec},
};
use fesen_proto::{Code, HostOs, Open, ServiceKind, valid_id};
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, copy_bidirectional_with_sizes},
    time::{Instant, timeout},
};
use tokio_util::sync::CancellationToken;

impl Core {
    pub(super) async fn incoming(self: Arc<Self>, link: Arc<Link>) {
        let pending = Arc::new(tokio::sync::Semaphore::new(64));
        while let Some(incoming) = link.direct.accept().await {
            let Ok(slot) = pending.clone().try_acquire_owned() else {
                incoming.refuse();
                continue;
            };
            let core = self.clone();
            let link = link.clone();
            tokio::spawn(async move {
                let conn = tokio::select! {
                    _ = link.direct.cancel.cancelled() => return,
                    conn = timeout(Duration::from_secs(8), incoming) => match conn { Ok(Ok(c)) => c, _ => return },
                };
                drop(slot);
                let Some(allowed) = link.direct.identify(&conn) else {
                    conn.close(0u32.into(), b"unauthorized");
                    return;
                };
                loop {
                    let (tx, rx) = tokio::select! {
                        biased;
                        _ = allowed.cancel.cancelled() => break,
                        _ = link.direct.cancel.cancelled() => break,
                        pair = conn.accept_bi() => match pair { Ok(p) => p, Err(_) => break },
                    };
                    let core = core.clone();
                    let allowed = allowed.clone();
                    let link = link.clone();
                    tokio::spawn(async move {
                        core.serve_stream(&link, &allowed, tx, rx).await;
                    });
                }
                conn.close(0u32.into(), b"closed");
            });
        }
    }

    fn grant(
        &self,
        link: &Arc<Link>,
        request: &Open,
    ) -> Result<(CancellationToken, String), Code> {
        let state = self.state.lock().unwrap();
        if !Self::is_link(&state, link)
            || !state.network.known()
            || !link.direct.membership.is_fresh()
        {
            return Err(Code::Unauthorized);
        }
        let share = state
            .sharing
            .get(&request.service)
            .ok_or(Code::Unauthorized)?;
        if !share
            .publication
            .rules
            .allows(*request.target.ip(), request.target.port())
        {
            return Err(Code::Unauthorized);
        }
        Ok((share.cancel.clone(), share.publication.name.clone()))
    }

    async fn serve_stream(
        &self,
        link: &Arc<Link>,
        peer: &Allowed,
        mut tx: quinn::SendStream,
        mut rx: quinn::RecvStream,
    ) {
        let Ok(_slot) = link.direct.slots.try_acquire() else {
            let _ = direct::frame_out(&mut tx, &Some(Code::Busy)).await;
            return;
        };
        let _ = tokio::select! {
            biased;
            _ = peer.cancel.cancelled() => None,
            _ = link.direct.cancel.cancelled() => None,
            result = async {
                let request = timeout(Duration::from_secs(4), direct::frame_in::<Open>(&mut rx)).await.ok()?.ok()?;
                let (share, share_name) = match self.grant(link, &request) {
                    Ok(v) => v,
                    Err(e) => {
                        self.log.record("WARN", "share", &format!("denied target={} err={e:?}", request.target));
                        direct::frame_out(&mut tx, &Some(e)).await.ok()?;
                        return Some(());
                    }
                };
                // Recheck the underlay before connecting, not just on the periodic heartbeat.
                if self.refresh_network().await { return None; }
                let flow_cancel = share.child_token();
                let is_local_target = request.target.ip().is_loopback()
                    || self.network().addresses.contains(request.target.ip());
                let peer_id = link
                    .direct
                    .membership
                    .id_for(&peer.certificate)
                    .unwrap_or_default();
                let _ = share_name;
                let mut flow = self.traffic.register(FlowSpec {
                    kind: if is_local_target {
                        FlowKind::InboundLocal
                    } else {
                        FlowKind::InboundLan
                    },
                    service: request.service.clone(),
                    peer_id,
                    src_port: None,
                    target: request.target,
                    local_port: None,
                    cancel: flow_cancel.clone(),
                });
                tokio::select! {
                    biased;
                    _ = flow_cancel.cancelled() => {
                        flow.close_stopped(|id| self.peer_label(id));
                        None
                    }
                    result = async {
                        match platform::tcp(&*self.platform, request.target).await {
                            Ok(tcp) => {
                                direct::frame_out(&mut tx, &Option::<Code>::None).await.ok()?;
                                let mut peer_io = tokio::io::join(&mut rx, &mut tx);
                                let mut counted_tcp = flow.wrap(tcp);
                                let _ = copy_bidirectional_with_sizes(&mut peer_io, &mut counted_tcp, COPY_BUF, COPY_BUF).await;
                            }
                            Err(e) => {
                                flow.close_error(&format!("{e:?}"), |id| self.peer_label(id));
                                direct::frame_out(&mut tx, &Some(e)).await.ok()?;
                            }
                        }
                        Some(())
                    } => result,
                }
            } => result,
        };
        let _ = tx.finish();
    }

    pub async fn forward<T: AsyncRead + AsyncWrite + Unpin>(
        &self,
        app: T,
        address: SocketAddrV4,
    ) -> Result<(), Code> {
        self.forward_from(app, address, None).await
    }

    pub async fn forward_from<T: AsyncRead + AsyncWrite + Unpin>(
        &self,
        mut app: T,
        address: SocketAddrV4,
        src_port: Option<u16>,
    ) -> Result<(), Code> {
        let dest = self.routes.read().unwrap().destination(address);
        match dest {
            Destination::Reject => Err(Code::Unauthorized),
            Destination::Local => {
                let Some(link) = self.state.lock().unwrap().link.clone() else {
                    return Ok(());
                };
                tokio::select! {
                    biased;
                    _ = link.direct.cancel.cancelled() => Ok(()),
                    result = async {
                        let mut remote = platform::tcp(&*self.platform, address).await?;
                        let _ = copy_bidirectional_with_sizes(&mut app, &mut remote, COPY_BUF, COPY_BUF).await;
                        Ok(())
                    } => result,
                }
            }
            Destination::Peer {
                owner,
                service,
                target,
            } => {
                let flow_cancel = CancellationToken::new();
                let mut flow = self.traffic.register(FlowSpec {
                    kind: FlowKind::OutboundTun,
                    service: service.clone(),
                    peer_id: owner.clone(),
                    src_port,
                    target,
                    local_port: None,
                    cancel: flow_cancel.clone(),
                });
                let res = tokio::select! {
                    biased;
                    _ = flow_cancel.cancelled() => {
                        flow.close_stopped(|id| self.peer_label(id));
                        Ok(())
                    }
                    res = self.with_peer(&owner, Open { service: service.clone(), target }, async |tx, rx, _| {
                        let mut remote = flow.wrap(tokio::io::join(rx, tx));
                        let _ =
                            copy_bidirectional_with_sizes(&mut app, &mut remote, COPY_BUF, COPY_BUF)
                                .await;
                        Ok(())
                    }) => res,
                };
                if let Err(code) = &res {
                    flow.close_error(&format!("{code:?}"), |id| self.peer_label(id));
                }
                res
            }
        }
    }

    // Local IPC only: use the publisher process and its actual underlay socket, never a GUI socket.
    pub(super) async fn test_local_site(&self, id: &str, target: SocketAddrV4) -> Result<u16, Code> {
        {
            let state = self.state.lock().unwrap();
            if state.local.session.is_none() {
                return Err(Code::NotConnected);
            }
            let site = state
                .local
                .shares()
                .iter()
                .find(|s| s.id == id)
                .ok_or(Code::Offline)?;
            if !site.http || !site.rules.allows(*target.ip(), target.port()) {
                return Err(Code::Unauthorized);
            }
        }
        probe::http(platform::tcp(&*self.platform, target).await?, target).await
    }

    pub(super) async fn probe_host(
        &self,
        ip: Ipv4Addr,
        extra_ports: &[u16],
        ports: &[u16],
        deep: bool,
    ) -> Result<probe::HostProbe, Code> {
        let is_local_ip = {
            let state = self.state.lock().unwrap();
            if state.local.session.is_none() {
                return Err(Code::NotConnected);
            }
            state.network.addresses.contains(&ip)
        };
        let Ok(_guard) = self.host_probe_lock.try_lock() else {
            return Err(Code::Busy);
        };
        struct ProbeCancelGuard<'a>(&'a Mutex<Option<CancellationToken>>);
        impl Drop for ProbeCancelGuard<'_> {
            fn drop(&mut self) {
                *self.0.lock().unwrap() = None;
            }
        }
        let cancel = self.quit.child_token();
        *self.probe_cancel.lock().unwrap() = Some(cancel.clone());
        let _cancel_guard = ProbeCancelGuard(&self.probe_cancel);
        let started = Instant::now();
        let fut = async {
            if ports.is_empty() {
                probe::host_with_mode(&*self.platform, ip, extra_ports, deep, is_local_ip).await
            } else {
                probe::ports(&*self.platform, ip, ports).await
            }
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Ok(probe::HostProbe {
                ip,
                host_os: None,
                ports: Vec::new(),
                duration_ms: started.elapsed().as_millis().min(u32::MAX as u128) as u32,
                cancelled: true,
            }),
            res = fut => res,
        }
    }

    pub(super) async fn test_connection(
        &self,
        id: &str,
        address: SocketAddrV4,
    ) -> Result<probe::ProbeOutcome, Code> {
        if !valid_id(id) {
            return Err(Code::Invalid);
        }
        let (owner, request, access, kind) = {
            let state = self.state.lock().unwrap();
            let offering = state
                .view
                .iter()
                .find(|o| o.service.id == id)
                .ok_or(Code::Offline)?;
            if !offering.service.rules.allows(*address.ip(), address.port()) {
                return Err(Code::Unauthorized);
            }
            let access = if self.platform.system_routing() {
                let routes = self.routes.read().unwrap();
                let access =
                    SocketAddrV4::new(routes.access_ip_for(id, *address.ip()), address.port());
                if !matches!(
                    routes.destination(access),
                    Destination::Peer { owner, service, target }
                        if owner == offering.owner && service == id && target == address
                ) {
                    return Err(Code::Conflict);
                }
                access
            } else {
                address
            };
            (
                offering.owner.clone(),
                Open {
                    service: id.to_owned(),
                    target: address,
                },
                access,
                offering.service.effective_kind(),
            )
        };
        timeout(
            Duration::from_secs(24),
            self.with_peer(&owner, request, async |mut tx, mut rx, open_rtt_ms| {
                let outcome = match kind {
                    ServiceKind::Http => {
                        let code = probe::http(tokio::io::join(rx, tx), access).await?;
                        probe::ProbeOutcome {
                            kind,
                            rtt_ms: open_rtt_ms,
                            http_status: Some(code),
                            host_os: None,
                            detail: None,
                        }
                    }
                    ServiceKind::Https => {
                        probe::tls(tokio::io::join(rx, tx), Duration::from_secs(5)).await?;
                        probe::ProbeOutcome {
                            kind,
                            rtt_ms: open_rtt_ms,
                            http_status: None,
                            host_os: None,
                            detail: None,
                        }
                    }
                    ServiceKind::Ssh => {
                        let info = probe::ssh(rx, Duration::from_secs(4)).await?;
                        let _ = tx.finish();
                        probe::ProbeOutcome {
                            kind,
                            rtt_ms: open_rtt_ms,
                            http_status: None,
                            host_os: info.os,
                            detail: Some(info.banner),
                        }
                    }
                    ServiceKind::Rdp => {
                        let info =
                            probe::rdp(tokio::io::join(rx, tx), Duration::from_secs(4)).await?;
                        probe::ProbeOutcome {
                            kind,
                            rtt_ms: open_rtt_ms,
                            http_status: None,
                            host_os: Some(HostOs::Windows),
                            detail: Some(if info.nla { "NLA" } else { "RDP" }.into()),
                        }
                    }
                    ServiceKind::Smb | ServiceKind::Tcp => {
                        let _ = tx.finish();
                        let _ = rx.stop(0u32.into());
                        probe::ProbeOutcome {
                            kind,
                            rtt_ms: open_rtt_ms,
                            http_status: None,
                            host_os: None,
                            detail: None,
                        }
                    }
                };
                Ok(outcome)
            }),
        )
        .await
        .map_err(|_| Code::DirectFailed)?
    }

    pub(super) async fn with_peer<R>(
        &self,
        owner: &str,
        request: Open,
        on_open: impl AsyncFnOnce(quinn::SendStream, quinn::RecvStream, u32) -> Result<R, Code>,
    ) -> Result<R, Code> {
        let link = {
            let state = self.state.lock().unwrap();
            if !state.latest.contains(&request.service) {
                return Err(Code::Offline);
            }
            state.link.clone().ok_or(Code::Offline)?
        };
        let _slot = link.direct.slots.try_acquire().map_err(|_| Code::Busy)?;
        let (conn, peer) = link
            .direct
            .connect(owner, || link.candidates(owner))
            .await?;
        tokio::select! {
            biased;
            _ = peer.cancel.cancelled() => Err(Code::Offline),
            _ = link.direct.cancel.cancelled() => Err(Code::NotConnected),
            result = async {
                let open_started = Instant::now();
                let (tx, rx) = direct::open(&conn, request).await?;
                let open_rtt_ms = open_started.elapsed().as_millis().clamp(1, u32::MAX as u128) as u32;
                on_open(tx, rx, open_rtt_ms).await
            } => result,
        }
    }
}
