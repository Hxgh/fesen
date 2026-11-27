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
};
use fesen_proto::{Code, Credential, LocalForwardStatus, origin};
use std::{
    collections::{HashMap, HashSet},
    io,
    net::Ipv4Addr,
    path::PathBuf,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, AtomicUsize, Ordering},
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
    flows: AtomicUsize,
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
            flows: AtomicUsize::new(0),
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
            || self.flows.load(Ordering::Acquire) != 0
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
            active_flows: self.flows.load(Ordering::Acquire),
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
        if res == Err(Code::Unauthorized) {
            self.edit(|l| {
                l.session = None;
                Ok(())
            })?;
            self.disconnect(false);
        }
        res
    }

    async fn connect(self: &Arc<Self>, credential: Credential, member: &str) -> Result<(), Code> {
        let (address, identity, net) = {
            let s = self.state.lock().unwrap();
            (
                origin(&s.local.server)?,
                s.local.identities.get(member).cloned(),
                s.network.clone(),
            )
        };
        let identity = identity.map_or_else(Identity::new, Ok)?;
        let session = tokio::select! {
            _ = self.quit.cancelled() => return Err(Code::NotConnected),
            res = Session::connect(&*self.platform, &address, identity.clone(), credential, self.quit.child_token()) => res?,
        };
        {
            let mut state = self.state.lock().unwrap();
            if state.network != net || self.quit.is_cancelled() {
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
        if error == Some(Code::Unauthorized) {
            state.wanted = false;
            state.local.session = None;
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
        let (link, was_sharing) = {
            let mut state = self.state.lock().unwrap();
            if state.network == network {
                return false;
            }
            state.network = network;
            let was_sharing = !state.sharing.is_empty();
            if was_sharing {
                state.restore_pending = false;
            }
            for (_, s) in state.sharing.drain() {
                s.cancel.cancel();
            }
            state.view.clear();
            state.latest.clear();
            (state.link.take(), was_sharing)
        };
        if let Some(link) = link {
            link.direct.close();
        }
        if was_sharing {
            self.report(Code::NetworkChanged);
        }
        self.changed();
        true
    }

    async fn route_worker(self: Arc<Self>) {
        let mut rx = self.changes.subscribe();
        let mut applied = Tun::default();
        loop {
            let (network, view, labels, control, wanted) = {
                let s = self.state.lock().unwrap();
                (
                    s.network.clone(),
                    s.view.clone(),
                    s.local.labels().to_vec(),
                    s.link.as_ref().map(|l| l.server_ip),
                    s.wanted,
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
                )
            };
            let mut next = match result {
                Ok(next) => next,
                Err(e) => {
                    self.report(e);
                    self.routes.read().unwrap().cleared()
                }
            };
            let tun = next.tun.clone();
            next.installed = tun == applied;
            *self.routes.write().unwrap() = next;
            self.bump_status();
            if tun != applied {
                let p = self.platform.clone();
                let update = tun.clone();
                let success = tokio::task::spawn_blocking(move || p.apply(&update))
                    .await
                    .is_ok_and(|r| r.is_ok());
                if success {
                    applied = tun;
                    self.routes.write().unwrap().installed = true;
                    self.bump_status();
                } else {
                    self.report(Code::Conflict);
                    let p = self.platform.clone();
                    let _ = tokio::task::spawn_blocking(move || p.apply(&Tun::default())).await;
                    applied = Tun::default();
                    let cleared = self.routes.read().unwrap().cleared();
                    *self.routes.write().unwrap() = cleared;
                    self.bump_status();
                }
            }
            tokio::select! {
                _ = self.quit.cancelled() => break,
                _ = rx.changed() => {},
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
                        let wanted = this.state.lock().unwrap().wanted;
                        if wanted && let Err(e) = this.resume().await {
                            let paused =
                                this.state.lock().unwrap().error == Some(Code::NetworkChanged);
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
