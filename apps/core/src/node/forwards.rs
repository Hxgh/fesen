use super::{
    Core,
    types::{COPY_BUF, Flow, ForwardHandle, ForwardKey},
};
use fesen_proto::{Code, LocalForwardStatus, Open};
use std::{
    collections::{HashMap, HashSet},
    net::{Ipv4Addr, SocketAddrV4},
    sync::Arc,
    time::Duration,
};
use tokio::{io::copy_bidirectional_with_sizes, net::TcpListener, time::timeout};
use tokio_util::sync::CancellationToken;

impl Core {
    pub(super) async fn forward_worker(self: Arc<Self>) {
        let mut rx = self.changes.subscribe();
        loop {
            if self.quit.is_cancelled() {
                break;
            }
            self.reconcile_forwards().await;
            tokio::select! {
                _ = self.quit.cancelled() => break,
                _ = rx.changed() => {},
            }
        }
        {
            let mut listeners = self.forwards_lock.lock().await;
            let drained: Vec<_> = listeners.drain().map(|(_, h)| h).collect();
            for h in &drained {
                h.cancel.cancel();
            }
            futures_util::future::join_all(
                drained
                    .into_iter()
                    .map(|h| timeout(Duration::from_millis(200), h.closed)),
            )
            .await;
        }
        self.state.lock().unwrap().forward_states.clear();
        self.bump_status();
        self.forwards_shutdown.cancel();
    }

    pub(super) async fn reconcile_forwards(self: &Arc<Self>) {
        let mut listeners = self.forwards_lock.lock().await;
        let (online, configured, view) = {
            let state = self.state.lock().unwrap();
            let online = !self.quit.is_cancelled()
                && state.wanted
                && state
                    .link
                    .as_ref()
                    .is_some_and(|l| l.direct.membership.is_fresh());
            (online, state.local.forwards().to_vec(), state.view.clone())
        };
        if !online {
            let drained: Vec<_> = listeners.drain().map(|(_, h)| h).collect();
            for h in &drained {
                h.cancel.cancel();
            }
            futures_util::future::join_all(
                drained
                    .into_iter()
                    .map(|h| timeout(Duration::from_millis(200), h.closed)),
            )
            .await;
            let mut state = self.state.lock().unwrap();
            if !state.forward_states.is_empty() {
                state.forward_states.clear();
                drop(state);
                self.bump_status();
            }
            return;
        }
        let mut desired: HashMap<ForwardKey, (String, u16)> = HashMap::new();
        let mut used_ports: HashSet<u16> = HashSet::new();
        for m in &configured {
            used_ports.insert(m.local_port);
            if let Some(o) = view.iter().find(|o| {
                o.service.id == m.service && o.service.rules.allows(m.target_ip, m.target_port)
            }) {
                desired.insert(
                    (m.service.clone(), m.target_ip, m.target_port),
                    (o.owner.clone(), m.local_port),
                );
            }
        }
        let stale: Vec<ForwardKey> = listeners
            .iter()
            .filter(|(k, h)| {
                desired
                    .get(*k)
                    .is_none_or(|(owner, port)| &h.owner != owner || h.local_port != *port)
            })
            .map(|(k, _)| k.clone())
            .collect();
        let mut removed = Vec::with_capacity(stale.len());
        for k in stale {
            if let Some(handle) = listeners.remove(&k) {
                handle.cancel.cancel();
                removed.push(handle);
            }
        }
        futures_util::future::join_all(
            removed
                .into_iter()
                .map(|h| timeout(Duration::from_millis(200), h.closed)),
        )
        .await;
        let mut states = HashMap::new();
        for m in configured {
            let key = (m.service.clone(), m.target_ip, m.target_port);
            let Some((owner, local_port)) = desired.get(&key).cloned() else {
                states.insert(
                    key,
                    LocalForwardStatus {
                        service: m.service,
                        target_ip: m.target_ip,
                        target_port: m.target_port,
                        local_port: m.local_port,
                        active: false,
                        error: Some(Code::Offline),
                        suggested_port: None,
                    },
                );
                continue;
            };
            if listeners.contains_key(&key) {
                states.insert(
                    key,
                    LocalForwardStatus {
                        service: m.service,
                        target_ip: m.target_ip,
                        target_port: m.target_port,
                        local_port,
                        active: true,
                        error: None,
                        suggested_port: None,
                    },
                );
                continue;
            }
            match std::net::TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, local_port)) {
                Ok(std_listener) if std_listener.set_nonblocking(true).is_ok() => {
                    if let Ok(listener) = TcpListener::from_std(std_listener) {
                        let cancel = self.quit.child_token();
                        let (closed_tx, closed) = tokio::sync::oneshot::channel();
                        let core = self.clone();
                        let svc = m.service.clone();
                        let target = SocketAddrV4::new(m.target_ip, m.target_port);
                        let own = owner.clone();
                        let tok = cancel.clone();
                        tokio::spawn(async move {
                            core.accept_forward_loop(listener, own, svc, target, tok, closed_tx)
                                .await;
                        });
                        listeners.insert(
                            key.clone(),
                            ForwardHandle {
                                owner,
                                local_port,
                                cancel,
                                closed,
                            },
                        );
                        states.insert(
                            key,
                            LocalForwardStatus {
                                service: m.service,
                                target_ip: m.target_ip,
                                target_port: m.target_port,
                                local_port,
                                active: true,
                                error: None,
                                suggested_port: None,
                            },
                        );
                    } else {
                        let suggested = suggest_free_port(Some(local_port), &used_ports);
                        states.insert(
                            key,
                            LocalForwardStatus {
                                service: m.service,
                                target_ip: m.target_ip,
                                target_port: m.target_port,
                                local_port,
                                active: false,
                                error: Some(Code::Conflict),
                                suggested_port: suggested,
                            },
                        );
                    }
                }
                _ => {
                    let suggested = suggest_free_port(Some(local_port), &used_ports);
                    states.insert(
                        key,
                        LocalForwardStatus {
                            service: m.service,
                            target_ip: m.target_ip,
                            target_port: m.target_port,
                            local_port,
                            active: false,
                            error: Some(Code::Conflict),
                            suggested_port: suggested,
                        },
                    );
                }
            }
        }
        let mut state = self.state.lock().unwrap();
        if state.forward_states != states {
            state.forward_states = states;
            drop(state);
            self.bump_status();
        }
    }

    async fn accept_forward_loop(
        self: Arc<Self>,
        listener: TcpListener,
        owner: String,
        service: String,
        target: SocketAddrV4,
        cancel: CancellationToken,
        closed_tx: tokio::sync::oneshot::Sender<()>,
    ) {
        let slots = Arc::new(tokio::sync::Semaphore::new(64));
        loop {
            let conn = tokio::select! {
                _ = cancel.cancelled() => break,
                res = listener.accept() => res,
            };
            let Ok((mut client, _)) = conn else { break };
            let Ok(permit) = slots.clone().try_acquire_owned() else {
                continue;
            };
            let _ = client.set_nodelay(true);
            let core = self.clone();
            let owner = owner.clone();
            let service = service.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let _flow = Flow::new(&core.flows);
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {},
                    _ = core.with_peer(&owner, Open { service, target }, async |tx, rx, _| {
                        let mut remote = tokio::io::join(rx, tx);
                        let _ = copy_bidirectional_with_sizes(&mut client, &mut remote, COPY_BUF, COPY_BUF).await;
                        Ok(())
                    }) => {},
                }
            });
        }
        drop(listener);
        let _ = closed_tx.send(());
    }
}

pub(super) fn suggest_free_port(preferred: Option<u16>, used: &HashSet<u16>) -> Option<u16> {
    let is_free = |port: u16| {
        port != 0
            && !used.contains(&port)
            && std::net::TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).is_ok()
    };
    if let Some(p) = preferred {
        let base = if p < 1024 {
            10000u16.saturating_add(p).max(1024)
        } else {
            p
        };
        for offset in 0..=40u16 {
            if let Some(candidate) = base.checked_add(offset)
                && is_free(candidate)
            {
                return Some(candidate);
            }
        }
    }
    for candidate in 18000..=18060u16 {
        if is_free(candidate) {
            return Some(candidate);
        }
    }
    for _ in 0..16 {
        if let Ok(listener) =
            std::net::TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            && let Ok(addr) = listener.local_addr()
            && !used.contains(&addr.port())
        {
            return Some(addr.port());
        }
    }
    None
}
