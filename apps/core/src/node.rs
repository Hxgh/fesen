mod commands;
mod forwards;
mod shares;
#[cfg(test)]
mod tests;
mod tunnel;
mod types;

pub use types::{Command, HostServiceInput, Status};

use crate::{
    control::{Link, Session},
    local::{Identity, Local, Log, Login, private_dir},
    packets,
    platform::{DeviceInfo, Network, Platform, Tun},
    routing::Routes,
    traffic::TrafficTracker,
};
use fesen_proto::{Code, Credential, LocalForwardStatus, origin};
use std::{
    collections::{HashMap, HashSet},
    io,
    net::Ipv4Addr,
    path::PathBuf,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::watch,
    time::{Instant, interval},
};
use tokio_util::sync::CancellationToken;
use types::{ForwardHandle, ForwardKey, State};

pub(super) const UI_LEASE: Duration = Duration::from_secs(60);

pub struct Core {
    dir: PathBuf,
    device_info: DeviceInfo,
    state: Mutex<State>,
    pub(crate) platform: Arc<dyn Platform>,
    log: Arc<Log>,
    operations: tokio::sync::Mutex<()>,
    forwards_lock: tokio::sync::Mutex<HashMap<ForwardKey, ForwardHandle>>,
    host_probe_lock: tokio::sync::Mutex<()>,
    probe_cancel: Mutex<Option<CancellationToken>>,
    network_probe: tokio::sync::Mutex<Instant>,
    pub(crate) routes: RwLock<Routes>,
    changes: watch::Sender<u64>,
    status_seq: AtomicU64,
    pub(crate) quit: CancellationToken,
    applied_shutdown: CancellationToken,
    forwards_shutdown: CancellationToken,
    pub(crate) traffic: Arc<TrafficTracker>,
    keep_access: bool,
}

impl Core {
    pub fn new(
        dir: PathBuf,
        platform: Arc<dyn Platform>,
        keep_access: bool,
    ) -> io::Result<Arc<Self>> {
        private_dir(&dir)?;
        let local = Local::load(&dir)?;
        let device_info = platform.device_info();
        let network = platform.network().unwrap_or_default();
        let log = Arc::new(Log::new(&dir));
        log.prune();
        let traffic = Arc::new(TrafficTracker::new(log.clone()));
        let core = Arc::new(Self {
            dir,
            device_info,
            platform,
            log,
            state: Mutex::new(State {
                local,
                network,
                link: None,
                view: vec![],
                latest: HashSet::new(),
                sharing: HashMap::new(),
                forward_states: HashMap::new(),
                cloud_profiles: vec![],
                profile_synced: false,
                dismissed_restore: false,
                wanted: false,
                restore_pending: false,
                auto_share_blocked: false,
                ui_until: Some(Instant::now() + UI_LEASE),
                error: None,
                is_admin: false,
                admin_token: None,
            }),
            operations: tokio::sync::Mutex::new(()),
            forwards_lock: tokio::sync::Mutex::new(HashMap::new()),
            host_probe_lock: tokio::sync::Mutex::new(()),
            probe_cancel: Mutex::new(None),
            network_probe: tokio::sync::Mutex::new(Instant::now() - Duration::from_secs(1)),
            routes: RwLock::new(Routes::default()),
            changes: watch::channel(0).0,
            status_seq: AtomicU64::new(1),
            quit: CancellationToken::new(),
            applied_shutdown: CancellationToken::new(),
            forwards_shutdown: CancellationToken::new(),
            traffic,
            keep_access,
        });
        if core.platform.system_routing() {
            packets::spawn(core.clone())?;
            tokio::spawn(core.clone().route_worker());
        } else {
            core.applied_shutdown.cancel();
        }
        tokio::spawn(core.clone().forward_worker());
        tokio::spawn(core.clone().monitor());
        Ok(core)
    }

    pub fn network(&self) -> Network {
        self.state.lock().unwrap().network.clone()
    }

    pub(crate) fn dns_servers(&self) -> Vec<Ipv4Addr> {
        self.state.lock().unwrap().network.dns.clone()
    }

    #[cfg(target_os = "android")]
    pub(crate) fn running(&self) -> bool {
        self.state.lock().unwrap().wanted
    }

    fn has_work(&self, state: &State) -> bool {
        !state.sharing.is_empty()
            || self.traffic.active_count() != 0
            || (self.keep_access && state.wanted)
            || (state.wanted && !state.local.forwards().is_empty())
    }

    #[cfg(windows)]
    pub(crate) fn visual_state(&self) -> u8 {
        let state = self.state.lock().unwrap();
        let online = self.platform.access_ready()
            && state
                .link
                .as_ref()
                .is_some_and(|l| l.direct.membership.is_fresh());
        if online {
            2
        } else if state.wanted {
            1
        } else {
            0
        }
    }

    #[cfg(windows)]
    pub(crate) fn subscribe_changes(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    fn bump_status(&self) {
        self.status_seq.fetch_add(1, Ordering::Release);
    }

    fn status_tag(&self, state: &State) -> (u64, bool, bool) {
        let seq = self.status_seq.load(Ordering::Acquire);
        let online = self.platform.access_ready()
            && state
                .link
                .as_ref()
                .is_some_and(|l| l.direct.membership.is_fresh());
        let background = self.has_work(state);
        let tag = (seq << 2) | (u64::from(online) << 1) | u64::from(background);
        (tag, online, background)
    }

    pub(super) fn touch_ui(&self) {
        let mut state = self.state.lock().unwrap();
        if state.ui_until.is_some() {
            state.ui_until = Some(Instant::now() + UI_LEASE);
        }
    }

    pub(crate) fn poll_status(&self, ui: bool, since: u64) -> Result<Option<Status>, Code> {
        if self.quit.is_cancelled() {
            return Err(Code::NotConnected);
        }
        let mut state = self.state.lock().unwrap();
        if ui && state.ui_until.is_some() {
            state.ui_until = Some(Instant::now() + UI_LEASE);
        }
        let (tag, online, background) = self.status_tag(&state);
        if since != 0 && since == tag {
            return Ok(None);
        }
        Ok(Some(self.build_status(&state, tag, online, background)))
    }

    fn device_name(&self, local: &Local) -> String {
        local
            .device_name
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| self.device_info.name.clone())
    }

    pub fn status(&self) -> Status {
        let state = self.state.lock().unwrap();
        let (tag, online, background) = self.status_tag(&state);
        self.build_status(&state, tag, online, background)
    }

    fn build_status(&self, state: &State, version: u64, online: bool, background: bool) -> Status {
        let routes = self.routes.read().unwrap();
        let mut active: Vec<_> = state.sharing.keys().cloned().collect();
        active.sort_unstable();
        let mut forwards: Vec<_> = state
            .local
            .forwards()
            .iter()
            .map(|m| {
                state
                    .forward_states
                    .get(&(m.service.clone(), m.target_ip, m.target_port))
                    .cloned()
                    .unwrap_or(LocalForwardStatus {
                        service: m.service.clone(),
                        target_ip: m.target_ip,
                        target_port: m.target_port,
                        local_port: m.local_port,
                        active: false,
                        error: None,
                        suggested_port: None,
                    })
            })
            .collect();
        forwards.sort_unstable_by(|a, b| {
            (&a.service, a.target_ip, a.target_port, a.local_port).cmp(&(
                &b.service,
                b.target_ip,
                b.target_port,
                b.local_port,
            ))
        });
        let restore_prompt = if state.local.session.is_some()
            && state.local.shares().is_empty()
            && state.local.forwards().is_empty()
            && !state.dismissed_restore
        {
            state
                .cloud_profiles
                .iter()
                .find(|p| {
                    p.machine_id == self.device_info.machine_id
                        && (!p.shares.is_empty() || !p.forwards.is_empty())
                })
                .cloned()
        } else {
            None
        };
        Status {
            version,
            server: state.local.server.clone(),
            member: state.local.session.as_ref().map(|s| s.member.clone()),
            device_id: state.link.as_ref().map(|l| l.id.clone()),
            machine_id: self.device_info.machine_id.clone(),
            device_os: self.device_info.os,
            device_name: self.device_name(&state.local),
            online,
            running: state.wanted,
            background,
            system_routing: self.platform.system_routing(),
            routes_ready: routes.ready_for(&state.view),
            is_admin: state.is_admin,
            admin_ready: state.admin_token.is_some(),
            services: state.view.clone(),
            shares: state.local.shares().to_vec(),
            active,
            active_flows: self.traffic.active_count(),
            labels: state.local.labels().to_vec(),
            virtual_ips: routes.mappings.clone(),
            service_virtual_ips: routes.service_mappings.clone(),
            aliases: routes.aliases.clone(),
            forwards,
            cloud_profiles: state.cloud_profiles.clone(),
            restore_prompt,
            error: state.error,
            auto_connect: state.local.auto_connect,
            auto_share: state.local.auto_share,
            cloud_backup: state.local.cloud_backup(),
            auto_share_blocked: state.auto_share_blocked,
            suggested_port: None,
            http_status: None,
            probe: None,
            host_probe: None,
            admin_data: None,
            logs: None,
            verbose_logs: self.log.is_verbose(),
            traffic: None,
        }
    }

    pub fn report(&self, error: Code) {
        let new = {
            let mut state = self.state.lock().unwrap();
            // A flow cancelled by the network change must not hide why sharing stopped.
            if state.error == Some(Code::NetworkChanged)
                && matches!(error, Code::Offline | Code::NotConnected)
            {
                return;
            }
            let new = state.error != Some(error);
            state.error = Some(error);
            new
        };
        if new {
            self.bump_status();
            self.log.event(error);
        }
    }

    fn changed(&self) {
        self.bump_status();
        self.changes.send_modify(|n| *n = n.wrapping_add(1));
    }

    fn edit(&self, change: impl FnOnce(&mut Local) -> Result<(), Code>) -> Result<(), Code> {
        let mut state = self.state.lock().unwrap();
        let mut next = state.local.clone();
        change(&mut next)?;
        next.save(&self.dir)?;
        state.local = next;
        self.bump_status();
        Ok(())
    }

    async fn resume(self: &Arc<Self>) -> Result<(), Code> {
        let session = {
            let state = self.state.lock().unwrap();
            if state.link.is_some() {
                return Ok(());
            }
            state.local.session.clone().ok_or(Code::Unauthorized)?
        };
        let res = self
            .connect(
                Credential::Resume {
                    token: session.token,
                },
                &session.member,
            )
            .await;
        if matches!(res, Err(Code::Unauthorized | Code::DeviceRevoked)) {
            let member = session.member.clone();
            let drop_identity = res == Err(Code::DeviceRevoked);
            self.edit(|l| {
                l.session = None;
                if drop_identity {
                    l.identities.remove(&member);
                }
                Ok(())
            })?;
            self.disconnect(false);
        }
        res
    }

    async fn connect(self: &Arc<Self>, credential: Credential, member: &str) -> Result<(), Code> {
        let (address, existing_identity, net) = {
            let s = self.state.lock().unwrap();
            (
                origin(&s.local.server)?,
                s.local.identities.get(member).cloned(),
                s.network.clone(),
            )
        };
        let had_existing = existing_identity.is_some();
        let mut identity = existing_identity.map_or_else(Identity::new, Ok)?;
        let session = match tokio::select! {
            _ = self.quit.cancelled() => return Err(Code::NotConnected),
            res = Session::connect(&*self.platform, &address, identity.clone(), credential.clone(), self.quit.child_token()) => res,
        } {
            Ok(session) => session,
            Err(Code::DeviceRevoked)
                if had_existing && matches!(credential, Credential::Password { .. }) =>
            {
                // Password verified on server, only the old stored device cert was revoked:
                // rotate to a fresh device certificate automatically instead of stranding the user.
                identity = Identity::new()?;
                tokio::select! {
                    _ = self.quit.cancelled() => return Err(Code::NotConnected),
                    res = Session::connect(&*self.platform, &address, identity.clone(), credential, self.quit.child_token()) => res?,
                }
            }
            Err(err) => return Err(err),
        };
        {
            let mut state = self.state.lock().unwrap();
            if !state.network.same_underlay(&net) || self.quit.is_cancelled() {
                session.link.direct.close();
                return Err(Code::NetworkChanged);
            }
            if let Some(token) = &session.token {
                let mut next = state.local.clone();
                if next.identities.len() >= 128 && !next.identities.contains_key(member) {
                    return Err(Code::Limit);
                }
                next.identities.insert(member.to_owned(), identity);
                next.migrate_to_member(&session.member);
                next.session = Some(Login {
                    member: session.member.clone(),
                    token: token.clone(),
                });
                next.save(&self.dir)?;
                state.local = next;
            }
            state.wanted = true;
            state.is_admin = session.is_admin;
            state.admin_token = session.admin_token.clone();
            if state.error != Some(Code::NetworkChanged) {
                state.error = None;
            }
            state.link = Some(session.link.clone());
        }
        self.log.record_info("ctl", || {
            format!("connected member={} admin={}", session.member, session.is_admin)
        });
        tokio::spawn(self.clone().incoming(session.link.clone()));
        session.spawn(self.clone());
        self.changed();
        Ok(())
    }

    fn disconnect(&self, keep_wanted: bool) {
        let link = {
            let mut state = self.state.lock().unwrap();
            state.wanted &= keep_wanted;
            state.view.clear();
            state.latest.clear();
            state.profile_synced = false;
            state.forward_states.clear();
            for (_, r) in state.sharing.drain() {
                r.cancel.cancel();
            }
            state.link.take()
        };
        if let Some(link) = link {
            link.direct.close();
        }
        self.changed();
    }

    fn is_link(state: &State, link: &Arc<Link>) -> bool {
        state.link.as_ref().is_some_and(|l| Arc::ptr_eq(l, link))
    }

    pub(super) fn peer_label(&self, peer_id: &str) -> String {
        let state = self.state.lock().unwrap();
        if let Some(o) = state.view.iter().find(|o| o.owner == peer_id) {
            return o
                .device
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(o.member.as_str())
                .to_owned();
        }
        peer_id.chars().take(8).collect()
    }

    pub(super) fn service_meta(&self, service_id: &str) -> (String, String) {
        let state = self.state.lock().unwrap();
        if let Some(o) = state.view.iter().find(|o| o.service.id == service_id) {
            let peer = o
                .device
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(o.member.as_str())
                .to_owned();
            return (o.service.name.clone(), peer);
        }
        if let Some(s) = state.local.shares().iter().find(|s| s.id == service_id) {
            return (s.name.clone(), self.device_name(&state.local));
        }
        (service_id.chars().take(8).collect(), String::new())
    }

    pub(crate) fn resolve_flow_info(
        &self,
        service_id: &str,
        peer_id: &str,
        src_port: Option<u16>,
    ) -> crate::traffic::ResolvedFlowInfo {
        let (service_name, svc_peer) = self.service_meta(service_id);
        let peer = if svc_peer.is_empty() && !peer_id.is_empty() {
            self.peer_label(peer_id)
        } else {
            svc_peer
        };
        let source = src_port.map(|port| match self.platform.tcp_client_process(port) {
            Some(proc) => format!("{proc} :{port}"),
            None => format!(":{port}"),
        });
        crate::traffic::ResolvedFlowInfo {
            service_name,
            peer,
            source,
        }
    }

    pub(crate) fn link_ended(&self, link: &Arc<Link>, error: Option<Code>) {
        let mut state = self.state.lock().unwrap();
        if !Self::is_link(&state, link) {
            return;
        }
        state.link = None;
        state.view.clear();
        state.latest.clear();
        state.profile_synced = false;
        state.forward_states.clear();
        for (_, r) in state.sharing.drain() {
            r.cancel.cancel();
        }
        if matches!(error, Some(Code::Unauthorized | Code::DeviceRevoked)) {
            state.wanted = false;
            if let Some(session) = state.local.session.take()
                && error == Some(Code::DeviceRevoked)
            {
                state.local.identities.remove(&session.member);
            }
            let _ = state.local.save(&self.dir);
        }
        drop(state);
        if let Some(e) = error {
            self.report(e);
        }
        self.changed();
    }

    async fn refresh_network(&self) -> bool {
        let mut sampled = self.network_probe.lock().await;
        if sampled.elapsed() < Duration::from_millis(100) {
            return false;
        }
        let p = self.platform.clone();
        let network = tokio::task::spawn_blocking(move || p.network())
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
        *sampled = Instant::now();
        let (link, was_sharing, prev_marker, next_marker, prev_addrs, next_addrs) = {
            let mut state = self.state.lock().unwrap();
            if state.network == network {
                return false;
            }
            if state.network.same_underlay(&network) {
                state.network = network;
                return false;
            }
            let prev_marker = state.network.marker.clone();
            let next_marker = network.marker.clone();
            let prev_addrs = state.network.addresses.clone();
            let next_addrs = network.addresses.clone();
            state.network = network;
            let was_sharing = !state.sharing.is_empty();
            state.restore_pending = false;
            for (_, s) in state.sharing.drain() {
                s.cancel.cancel();
            }
            state.view.clear();
            state.latest.clear();
            (
                state.link.take(),
                was_sharing,
                prev_marker,
                next_marker,
                prev_addrs,
                next_addrs,
            )
        };
        if let Some(link) = link {
            link.direct.close();
        }
        self.log.record(
            if was_sharing { "WARN" } else { "INFO" },
            "net",
            &format!(
                "changed sharing={} addrs={:?}->{:?} gw={}->{}",
                was_sharing, prev_addrs, next_addrs, prev_marker, next_marker
            ),
        );
        if was_sharing {
            self.report(Code::NetworkChanged);
        }
        self.changed();
        true
    }

    async fn route_worker(self: Arc<Self>) {
        let mut rx = self.changes.subscribe();
        let mut applied = Tun::default();
        let mut failures: u8 = 0;
        let mut last_build_err: Option<Code> = None;
        loop {
            let (network, view, labels, control, wanted, local_shares) = {
                let s = self.state.lock().unwrap();
                (
                    s.network.clone(),
                    s.view.clone(),
                    s.local.labels().to_vec(),
                    s.link.as_ref().map(|l| l.server_ip),
                    s.wanted,
                    s.sharing
                        .values()
                        .flat_map(|sh| sh.publication.rules.ips.iter().copied())
                        .collect::<Vec<_>>(),
                )
            };
            if self.quit.is_cancelled() {
                break;
            }
            let result = {
                let previous = self.routes.read().unwrap();
                Routes::build(
                    &previous,
                    &network,
                    view,
                    &labels,
                    control,
                    wanted && self.platform.keep_empty_tun(),
                    &local_shares,
                )
            };
            let (mut next, build_ok) = match result {
                Ok(next) => {
                    last_build_err = None;
                    (next, true)
                }
                Err(e) => {
                    if last_build_err != Some(e) {
                        last_build_err = Some(e);
                        self.log.record(
                            "ERR",
                            "route",
                            &format!(
                                "build failed err={e:?} routes={} addrs={:?}",
                                network.routes.len(),
                                network.addresses
                            ),
                        );
                    }
                    self.report(e);
                    (self.routes.read().unwrap().cleared(), false)
                }
            };
            let tun = next.tun.clone();
            next.installed = tun == applied;
            *self.routes.write().unwrap() = next;
            self.bump_status();
            let retry_at = if tun != applied {
                let p = self.platform.clone();
                let update = tun.clone();
                let apply_res = tokio::task::spawn_blocking(move || p.apply(&update)).await;
                match apply_res {
                    Ok(Ok(())) => {
                        if failures > 0 || tun.address.is_some() {
                            self.log.record_info("tun", || {
                                format!(
                                    "applied addr={:?} routes={} dns={:?}",
                                    tun.address,
                                    tun.routes.len(),
                                    tun.dns
                                )
                            });
                        }
                        applied = tun;
                        failures = 0;
                        self.routes.write().unwrap().installed = true;
                        {
                            let mut state = self.state.lock().unwrap();
                            if state.error == Some(Code::Conflict) {
                                state.error = None;
                            }
                        }
                        self.bump_status();
                        None
                    }
                    Ok(Err(err)) => {
                        failures = failures.saturating_add(1);
                        self.log.record(
                            "ERR",
                            "tun",
                            &format!("apply failed attempt={failures}: {err}"),
                        );
                        if failures >= 2 {
                            self.report(Code::Conflict);
                        }
                        let delay = Duration::from_secs(match failures {
                            0 | 1 => 2,
                            2 => 4,
                            3 => 8,
                            _ => 15,
                        });
                        let p = self.platform.clone();
                        let _ = tokio::task::spawn_blocking(move || p.apply(&Tun::default())).await;
                        applied = Tun::default();
                        let cleared = self.routes.read().unwrap().cleared();
                        *self.routes.write().unwrap() = cleared;
                        self.bump_status();
                        Some(Instant::now() + delay)
                    }
                    Err(err) => {
                        failures = failures.saturating_add(1);
                        self.log.record("ERR", "tun", &format!("apply panicked: {err}"));
                        if failures >= 2 {
                            self.report(Code::Conflict);
                        }
                        let p = self.platform.clone();
                        let _ = tokio::task::spawn_blocking(move || p.apply(&Tun::default())).await;
                        applied = Tun::default();
                        let cleared = self.routes.read().unwrap().cleared();
                        *self.routes.write().unwrap() = cleared;
                        self.bump_status();
                        Some(Instant::now() + Duration::from_secs(2))
                    }
                }
            } else {
                failures = 0;
                if build_ok {
                    let mut cleared = false;
                    {
                        let mut state = self.state.lock().unwrap();
                        if state.error == Some(Code::Conflict) {
                            state.error = None;
                            cleared = true;
                        }
                    }
                    if cleared {
                        self.bump_status();
                    }
                }
                None
            };
            tokio::select! {
                _ = self.quit.cancelled() => break,
                _ = rx.changed() => {}
                _ = async {
                    if let Some(deadline) = retry_at {
                        tokio::time::sleep_until(deadline).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {}
            }
        }
        *self.routes.write().unwrap() = Routes::default();
        let p = self.platform.clone();
        let _ = tokio::task::spawn_blocking(move || p.apply(&Tun::default())).await;
        self.applied_shutdown.cancel();
    }

    async fn monitor(self: Arc<Self>) {
        let mut tick = interval(Duration::from_secs(2));
        let mut cleanup = Instant::now();
        let mut retry = None;
        loop {
            tokio::select! {
                _ = self.quit.cancelled() => return,
                _ = tick.tick() => {}
            }
            {
                let state = self.state.lock().unwrap();
                if state
                    .ui_until
                    .is_none_or(|deadline| Instant::now() >= deadline)
                    && !self.has_work(&state)
                {
                    drop(state);
                    self.stop();
                    return;
                }
            }
            self.traffic.expire_if_idle();
            if self.refresh_network().await {
                retry = None;
            }
            if cleanup.elapsed() >= Duration::from_secs(30) {
                let log = self.log.clone();
                let _ = tokio::task::spawn_blocking(move || log.prune()).await;
                cleanup = Instant::now();
            }
            let wants = {
                let s = self.state.lock().unwrap();
                s.wanted && s.link.is_none() && s.local.session.is_some()
            };
            if wants && retry.is_none_or(|i: Instant| i <= Instant::now()) {
                retry = Some(Instant::now() + Duration::from_secs(5));
                let this = self.clone();
                tokio::spawn(async move {
                    if let Ok(_op) = this.operations.try_lock() {
                        let wanted = {
                            let mut s = this.state.lock().unwrap();
                            if s.wanted
                                && s.link.is_none()
                                && s.error != Some(Code::NetworkChanged)
                                && s.local.auto_share
                                && s.network.known()
                            {
                                let (active, share_net) = s.local.active_shares();
                                if !active.is_empty()
                                    && !share_net.is_empty()
                                    && s.network.marker == share_net
                                {
                                    s.restore_pending = true;
                                }
                            }
                            s.wanted
                        };
                        if wanted && let Err(e) = this.resume().await {
                            let mut s = this.state.lock().unwrap();
                            s.restore_pending = false;
                            let paused = s.error == Some(Code::NetworkChanged);
                            drop(s);
                            if !paused {
                                this.report(e);
                            }
                        }
                    }
                });
            }
        }
    }

    pub fn stop(&self) {
        self.quit.cancel();
        self.disconnect(false);
    }

    pub async fn wait_stopped(&self) {
        self.quit.cancelled().await;
        self.applied_shutdown.cancelled().await;
        self.forwards_shutdown.cancelled().await;
    }
}
