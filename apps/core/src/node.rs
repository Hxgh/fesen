use crate::{
    control::{Link, Session},
    direct::{self, Allowed},
    local::{Identity, Local, Log, Login, private_dir},
    packets,
    platform::{self, Network, Platform, Tun},
    probe,
    routing::{Destination, Label, Mapping, Routes, alias_name},
};
use fesen_acl::Rules;
use fesen_proto::{
    Call, Code, Credential, HostOs, Offering, Open, Publication, ServiceKind, View, origin,
    valid_id, valid_label, valid_username,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    io,
    net::{Ipv4Addr, SocketAddrV4},
    path::PathBuf,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, copy_bidirectional_with_sizes},
    sync::watch,
    time::{Instant, interval, timeout},
};
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
pub struct HostServiceInput {
    pub port: u16,
    pub kind: ServiceKind,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Command {
    Status {
        #[serde(default)]
        ui: bool,
    },
    Attach,
    Detach,
    Join {
        address: String,
    },
    Login {
        username: String,
        password: String,
        label: String,
    },
    Start,
    Stop,
    Logout,
    Save {
        id: Option<String>,
        name: String,
        ips: String,
        ports: String,
        #[serde(default)]
        http: bool,
        #[serde(default)]
        kind: Option<ServiceKind>,
        #[serde(default)]
        host_os: Option<HostOs>,
    },
    SaveHost {
        ip: Ipv4Addr,
        name: String,
        #[serde(default)]
        host_os: Option<HostOs>,
        services: Vec<HostServiceInput>,
        #[serde(default)]
        publish: bool,
    },
    Share {
        id: String,
        enabled: bool,
    },
    ShareMany {
        ids: Vec<String>,
        enabled: bool,
    },
    Remove {
        id: String,
    },
    Customize {
        id: String,
        remark: String,
        alias: String,
        ip: Option<Ipv4Addr>,
    },
    TestConnection {
        id: String,
        ip: Ipv4Addr,
        port: u16,
    },
    TestLocalSite {
        id: String,
        ip: Ipv4Addr,
        port: u16,
    },
    ProbeHost {
        ip: Ipv4Addr,
        #[serde(default)]
        extra_ports: Vec<u16>,
        #[serde(default)]
        ports: Vec<u16>,
    },
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Status {
    pub server: String,
    pub member: Option<String>,
    pub online: bool,
    pub running: bool,
    pub background: bool,
    pub system_routing: bool,
    #[serde(default)]
    pub routes_ready: bool,
    pub services: Vec<Offering>,
    pub shares: Vec<Publication>,
    pub active: Vec<String>,
    #[serde(default)]
    pub active_flows: usize,
    pub labels: Vec<Label>,
    pub virtual_ips: Vec<Mapping>,
    pub aliases: HashMap<String, Ipv4Addr>,
    pub error: Option<Code>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe: Option<probe::ProbeOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_probe: Option<probe::HostProbe>,
}

struct Sharing {
    publication: Publication,
    cancel: CancellationToken,
}

struct State {
    local: Local,
    link: Option<Arc<Link>>,
    network: Network,
    view: Vec<Offering>,
    latest: HashSet<String>,
    sharing: HashMap<String, Sharing>,
    wanted: bool,
    ui_until: Option<Instant>,
    error: Option<Code>,
}

pub struct Core {
    dir: PathBuf,
    state: Mutex<State>,
    pub(crate) platform: Arc<dyn Platform>,
    log: Log,
    operations: tokio::sync::Mutex<()>,
    host_probe_lock: tokio::sync::Mutex<()>,
    network_probe: tokio::sync::Mutex<Instant>,
    pub(crate) routes: RwLock<Routes>,
    changes: watch::Sender<u64>,
    pub(crate) quit: CancellationToken,
    applied_shutdown: CancellationToken,
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
        let network = platform.network().unwrap_or_default();
        let log = Log::new(&dir);
        log.prune();
        let core = Arc::new(Self {
            dir,
            platform,
            log,
            state: Mutex::new(State {
                local,
                network,
                link: None,
                view: vec![],
                latest: HashSet::new(),
                sharing: HashMap::new(),
                wanted: false,
                ui_until: Some(Instant::now() + Duration::from_secs(20)),
                error: None,
            }),
            operations: tokio::sync::Mutex::new(()),
            host_probe_lock: tokio::sync::Mutex::new(()),
            network_probe: tokio::sync::Mutex::new(Instant::now() - Duration::from_secs(1)),
            routes: RwLock::new(Routes::default()),
            changes: watch::channel(0).0,
            quit: CancellationToken::new(),
            applied_shutdown: CancellationToken::new(),
            flows: AtomicUsize::new(0),
            keep_access,
        });
        if core.platform.system_routing() {
            packets::spawn(core.clone())?;
            tokio::spawn(core.clone().route_worker());
        } else {
            core.applied_shutdown.cancel();
        }
        tokio::spawn(core.clone().monitor());
        Ok(core)
    }

    pub fn network(&self) -> Network {
        self.state.lock().unwrap().network.clone()
    }

    #[cfg(target_os = "android")]
    pub(crate) fn running(&self) -> bool {
        self.state.lock().unwrap().wanted
    }

    fn has_work(&self, state: &State) -> bool {
        !state.sharing.is_empty()
            || self.flows.load(Ordering::Acquire) != 0
            || (self.keep_access && state.wanted)
    }

    pub fn status(&self) -> Status {
        let state = self.state.lock().unwrap();
        let routes = self.routes.read().unwrap();
        let mut active: Vec<_> = state.sharing.keys().cloned().collect();
        active.sort_unstable();
        Status {
            server: state.local.server.clone(),
            member: state.local.session.as_ref().map(|s| s.member.clone()),
            online: self.platform.access_ready()
                && state
                    .link
                    .as_ref()
                    .is_some_and(|l| l.direct.membership.is_fresh()),
            running: state.wanted,
            background: self.has_work(&state),
            system_routing: self.platform.system_routing(),
            routes_ready: routes.ready_for(&state.view),
            services: state.view.clone(),
            shares: state.local.shares.clone(),
            active,
            active_flows: self.flows.load(Ordering::Acquire),
            labels: state.local.labels.clone(),
            virtual_ips: routes.mappings.clone(),
            aliases: routes.aliases.clone(),
            error: state.error,
            http_status: None,
            probe: None,
            host_probe: None,
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
            self.log.event(error);
        }
    }

    fn changed(&self) {
        self.changes.send_modify(|n| *n = n.wrapping_add(1));
    }

    fn edit(&self, change: impl FnOnce(&mut Local) -> Result<(), Code>) -> Result<(), Code> {
        let mut state = self.state.lock().unwrap();
        let mut next = state.local.clone();
        change(&mut next)?;
        next.save(&self.dir)?;
        state.local = next;
        Ok(())
    }

    pub async fn execute(self: &Arc<Self>, command: Command) -> Result<Status, Code> {
        if self.quit.is_cancelled() {
            return Err(Code::NotConnected);
        }
        match command {
            Command::Status { ui } => {
                if ui {
                    let mut state = self.state.lock().unwrap();
                    // An in-flight WebView poll cannot reattach a closed window.
                    if state.ui_until.is_some() {
                        state.ui_until = Some(Instant::now() + Duration::from_secs(16));
                    }
                }
                return Ok(self.status());
            }
            Command::Attach => {
                self.state.lock().unwrap().ui_until =
                    Some(Instant::now() + Duration::from_secs(16));
                return Ok(self.status());
            }
            Command::Detach => {
                self.state.lock().unwrap().ui_until = None;
                return Ok(self.status());
            }
            Command::TestConnection { id, ip, port } => {
                let outcome = self
                    .test_connection(&id, SocketAddrV4::new(ip, port))
                    .await?;
                return Ok(Status {
                    http_status: outcome.http_status,
                    probe: Some(outcome),
                    ..self.status()
                });
            }
            Command::TestLocalSite { id, ip, port } => {
                let code = self
                    .test_local_site(&id, SocketAddrV4::new(ip, port))
                    .await?;
                return Ok(Status {
                    http_status: Some(code),
                    ..self.status()
                });
            }
            Command::ProbeHost {
                ip,
                extra_ports,
                ports,
            } => {
                let host_probe = self.probe_host(ip, &extra_ports, &ports).await?;
                return Ok(Status {
                    host_probe: Some(host_probe),
                    ..self.status()
                });
            }
            _ => {}
        }
        let _lock = self.operations.lock().await;
        if self.quit.is_cancelled() {
            return Err(Code::NotConnected);
        }
        let result = self.change(command).await;
        if let Err(code) = result {
            self.report(code);
            return Err(code);
        }
        Ok(self.status())
    }

    async fn change(self: &Arc<Self>, command: Command) -> Result<(), Code> {
        match command {
            Command::Status { .. }
            | Command::Attach
            | Command::Detach
            | Command::TestConnection { .. }
            | Command::TestLocalSite { .. }
            | Command::ProbeHost { .. } => unreachable!(),
            Command::Join { address } => {
                let address = origin(&address)?.to_string();
                self.edit(|local| {
                    if local.server == address {
                        return Ok(());
                    }
                    if local.session.is_some() {
                        return Err(Code::Busy);
                    }
                    local.server = address;
                    local.identities.clear();
                    local.labels.clear();
                    Ok(())
                })
            }
            Command::Login {
                username,
                password,
                label,
            } => {
                if !valid_username(&username) || !valid_label(&label) {
                    return Err(Code::Invalid);
                }
                if self.state.lock().unwrap().local.session.is_some() {
                    return Err(Code::Busy);
                }
                self.connect(
                    Credential::Password {
                        username: username.clone(),
                        password,
                        label,
                    },
                    &username,
                )
                .await
            }
            Command::Start => self.resume().await,
            Command::Stop => {
                self.disconnect(false);
                self.state.lock().unwrap().error = None;
                Ok(())
            }
            Command::Logout => {
                let live = self.state.lock().unwrap().link.clone();
                // Close functioning streams before asking the server to invalidate the token.
                self.restrict_local();
                if let Some(link) = live {
                    let _ = link.call(Call::Logout).await;
                }
                self.disconnect(false);
                self.edit(|l| {
                    l.session = None;
                    Ok(())
                })?;
                self.state.lock().unwrap().error = None;
                Ok(())
            }
            Command::Save {
                id,
                name,
                ips,
                ports,
                http,
                kind,
                host_os,
            } => {
                let id = id.unwrap_or_else(|| hex::encode(rand::random::<[u8; 16]>()));
                let effective_http = match kind {
                    Some(k) => k == ServiceKind::Http,
                    None => http,
                };
                let rule = Publication {
                    id: id.clone(),
                    name: name.trim().to_owned(),
                    rules: Rules::parse(&ips, &ports).map_err(|_| Code::Invalid)?,
                    http: effective_http,
                    kind,
                    host_os,
                }
                .checked()?;
                if self.state.lock().unwrap().sharing.contains_key(&id) {
                    return Err(Code::Busy);
                }
                self.edit(|local| {
                    if let Some(old) = local.shares.iter_mut().find(|s| s.id == id) {
                        *old = rule;
                    } else if local.shares.len() < 32 {
                        local.shares.push(rule);
                    } else {
                        return Err(Code::Limit);
                    }
                    Ok(())
                })
            }
            Command::SaveHost {
                ip,
                name,
                host_os,
                services,
                publish,
            } => self.save_host(ip, name, host_os, services, publish).await,
            Command::Share { id, enabled } => self.share(id, enabled).await,
            Command::ShareMany { ids, enabled } => self.share_many(ids, enabled).await,
            Command::Remove { id } => {
                self.share(id.clone(), false).await?;
                self.edit(|l| {
                    let removed_ip = l
                        .shares
                        .iter()
                        .find(|s| s.id == id)
                        .and_then(Publication::single_ip);
                    l.shares.retain(|s| s.id != id);
                    if let Some(ip) = removed_ip {
                        let keeps_os = l.shares.iter().any(|s| {
                            s.single_ip() == Some(ip)
                                && matches!(
                                    s.effective_kind(),
                                    ServiceKind::Ssh | ServiceKind::Rdp
                                )
                        });
                        if !keeps_os {
                            for s in &mut l.shares {
                                if s.single_ip() == Some(ip) {
                                    s.host_os = None;
                                }
                            }
                        }
                    }
                    Ok(())
                })
            }
            Command::Customize {
                id,
                remark,
                alias,
                ip,
            } => {
                if !valid_id(&id) || !(remark.is_empty() || valid_label(&remark)) {
                    return Err(Code::Invalid);
                }
                let alias = (!alias.trim().is_empty())
                    .then(|| alias_name(&alias))
                    .transpose()?;
                if alias.is_some()
                    && !self.state.lock().unwrap().view.iter().any(|s| {
                        s.service.id == id
                            && ip.is_some_and(|ip| {
                                s.service.rules.ips.iter().any(|r| r.contains(ip))
                            })
                    })
                {
                    return Err(Code::Invalid);
                }
                self.edit(|local| {
                    if let Some(alias) = &alias
                        && local
                            .labels
                            .iter()
                            .any(|l| l.service != id && l.alias.as_ref() == Some(alias))
                    {
                        return Err(Code::Conflict);
                    }
                    local.labels.retain(|l| l.service != id);
                    if !remark.is_empty() || alias.is_some() {
                        if local.labels.len() >= 256 {
                            return Err(Code::Limit);
                        }
                        local.labels.push(Label {
                            service: id,
                            remark,
                            alias,
                            ip,
                        });
                    }
                    Ok(())
                })?;
                self.changed();
                Ok(())
            }
        }
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
                next.session = Some(Login {
                    member: session.member.clone(),
                    token: token.clone(),
                });
                next.save(&self.dir)?;
                state.local = next;
            }
            state.wanted = true;
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

    async fn share(&self, id: String, enabled: bool) -> Result<(), Code> {
        if !enabled {
            let link = {
                let mut state = self.state.lock().unwrap();
                let Some(share) = state.sharing.remove(&id) else {
                    return Ok(());
                };
                share.cancel.cancel();
                state.link.clone()
            };
            if let Some(link) = link
                && link.call(Call::Unpublish { id }).await.is_err()
            {
                self.disconnect(true);
                return Err(Code::NotConnected);
            }
            return Ok(());
        }
        if self.refresh_network().await {
            return Err(Code::NetworkChanged);
        }
        let (link, net, service) = {
            let state = self.state.lock().unwrap();
            if state.sharing.contains_key(&id) {
                return Ok(());
            }
            if !state.network.known() {
                return Err(Code::NetworkChanged);
            }
            let link = state.link.clone().ok_or(Code::NotConnected)?;
            if !link.direct.membership.is_fresh() {
                return Err(Code::NotConnected);
            }
            let service = state
                .local
                .shares
                .iter()
                .find(|s| s.id == id)
                .cloned()
                .ok_or(Code::Invalid)?;
            (link, state.network.clone(), service)
        };
        if let Err(e) = link
            .call(Call::Publish {
                service: service.clone(),
            })
            .await
        {
            if e == Code::NotConnected {
                self.disconnect(true);
            }
            return Err(e);
        }
        let mut state = self.state.lock().unwrap();
        if state.network != net
            || !Self::is_link(&state, &link)
            || link.direct.cancel.is_cancelled()
        {
            drop(state);
            link.direct.close();
            return Err(Code::NetworkChanged);
        }
        state.sharing.insert(
            id,
            Sharing {
                publication: service,
                cancel: link.direct.cancel.child_token(),
            },
        );
        state.error = None;
        Ok(())
    }

    async fn share_many(&self, ids: Vec<String>, enabled: bool) -> Result<(), Code> {
        let mut unique = Vec::new();
        let mut seen = HashSet::new();
        for id in ids {
            if !valid_id(&id) {
                return Err(Code::Invalid);
            }
            if seen.insert(id.clone()) {
                unique.push(id);
            }
        }
        if unique.is_empty() || unique.len() > 32 {
            return Err(Code::Invalid);
        }
        {
            let state = self.state.lock().unwrap();
            if unique
                .iter()
                .any(|id| !state.local.shares.iter().any(|s| &s.id == id))
            {
                return Err(Code::Invalid);
            }
        }
        let mut first_error = None;
        for id in unique {
            if let Err(err) = self.share(id, enabled).await
                && first_error.is_none()
            {
                first_error = Some(err);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn save_host(
        &self,
        ip: Ipv4Addr,
        name: String,
        host_os: Option<HostOs>,
        services: Vec<HostServiceInput>,
        publish: bool,
    ) -> Result<(), Code> {
        if services.is_empty() || services.len() > probe::MAX_HOST_PROBE_PORTS {
            return Err(Code::Invalid);
        }
        let ip_text = ip.to_string();
        Rules::parse(&ip_text, "80").map_err(|_| Code::Invalid)?;
        let label = {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                ip_text.clone()
            } else {
                trimmed.to_owned()
            }
        };
        if !valid_label(&label) {
            return Err(Code::Invalid);
        }
        let mut seen_ports = HashSet::new();
        for svc in &services {
            if svc.port == 0 || !seen_ports.insert(svc.port) {
                return Err(Code::Invalid);
            }
        }

        let has_os_port = services
            .iter()
            .any(|s| matches!(s.kind, ServiceKind::Ssh | ServiceKind::Rdp));
        let host_os = if has_os_port { host_os } else { None };

        let target_ids = {
            let mut state = self.state.lock().unwrap();
            let mut next = state.local.clone();
            let mut target_ids = Vec::with_capacity(services.len());
            let mut new_entries = Vec::new();

            for svc in &services {
                let existing = next.shares.iter_mut().find(|s| {
                    s.single_ip() == Some(ip)
                        && matches!(
                            s.rules.ports.as_slice(),
                            [p] if p.start == svc.port && p.end == svc.port
                        )
                });
                if let Some(share) = existing {
                    target_ids.push(share.id.clone());
                    if !state.sharing.contains_key(&share.id) {
                        share.name = label.clone();
                        share.kind = Some(svc.kind);
                        share.http = svc.kind == ServiceKind::Http;
                        if host_os.is_some() {
                            share.host_os = host_os;
                        }
                    }
                } else {
                    let id = hex::encode(rand::random::<[u8; 16]>());
                    let rule = Publication {
                        id: id.clone(),
                        name: label.clone(),
                        rules: Rules::parse(&ip_text, &svc.port.to_string())
                            .map_err(|_| Code::Invalid)?,
                        http: svc.kind == ServiceKind::Http,
                        kind: Some(svc.kind),
                        host_os,
                    }
                    .checked()?;
                    target_ids.push(id);
                    new_entries.push(rule);
                }
            }
            if next.shares.len() + new_entries.len() > 32 {
                return Err(Code::Limit);
            }
            if let Some(os) = host_os {
                for share in &mut next.shares {
                    if share.single_ip() == Some(ip) && !state.sharing.contains_key(&share.id) {
                        share.host_os = Some(os);
                    }
                }
            }
            next.shares.extend(new_entries);
            next.save(&self.dir)?;
            state.local = next;
            target_ids
        };

        if publish {
            self.share_many(target_ids, true).await?;
        }
        Ok(())
    }

    fn restrict_local(&self) {
        let mut state = self.state.lock().unwrap();
        for (_, r) in state.sharing.drain() {
            r.cancel.cancel();
        }
    }

    fn disconnect(&self, keep_wanted: bool) {
        let link = {
            let mut state = self.state.lock().unwrap();
            state.wanted &= keep_wanted;
            state.view.clear();
            state.latest.clear();
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

    pub(crate) fn update_view(&self, link: &Arc<Link>, view: View) {
        let mut state = self.state.lock().unwrap();
        if !Self::is_link(&state, link) {
            return;
        }
        link.direct.renew(view.peers);
        let services = view
            .services
            .into_iter()
            .filter(|o| o.owner != link.id)
            .collect::<Vec<_>>();
        state.latest = services.iter().map(|o| o.service.id.clone()).collect();
        if services != state.view {
            state.view = services;
            drop(state);
            self.changed();
        }
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
                    s.local.labels.clone(),
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
            if tun != applied {
                let p = self.platform.clone();
                let update = tun.clone();
                let success = tokio::task::spawn_blocking(move || p.apply(&update))
                    .await
                    .is_ok_and(|r| r.is_ok());
                if success {
                    applied = tun;
                    self.routes.write().unwrap().installed = true;
                } else {
                    self.report(Code::Conflict);
                    let p = self.platform.clone();
                    let _ = tokio::task::spawn_blocking(move || p.apply(&Tun::default())).await;
                    applied = Tun::default();
                    let cleared = self.routes.read().unwrap().cleared();
                    *self.routes.write().unwrap() = cleared;
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
                self.log.prune();
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
    }

    async fn incoming(self: Arc<Self>, link: Arc<Link>) {
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

    fn grant(&self, link: &Arc<Link>, request: &Open) -> Result<CancellationToken, Code> {
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
        Ok(share.cancel.clone())
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
                let share = match self.grant(link, &request) {
                    Ok(c) => c,
                    Err(e) => {
                        self.log.event(e);
                        direct::frame_out(&mut tx, &Some(e)).await.ok()?;
                        return Some(());
                    }
                };
                // Recheck the underlay before connecting, not just on the periodic heartbeat.
                if self.refresh_network().await { return None; }
                let _flow = Flow::new(&self.flows);
                tokio::select! {
                    biased;
                    _ = share.cancelled() => None,
                    result = async {
                        match platform::tcp(&*self.platform, request.target).await {
                            Ok(mut tcp) => {
                                direct::frame_out(&mut tx, &Option::<Code>::None).await.ok()?;
                                let mut peer = tokio::io::join(&mut rx, &mut tx);
                                let _ = copy_bidirectional_with_sizes(&mut peer, &mut tcp, 16384, 16384).await;
                            }
                            Err(e) => { direct::frame_out(&mut tx, &Some(e)).await.ok()?; }
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
        mut app: T,
        address: SocketAddrV4,
    ) -> Result<(), Code> {
        let _flow = Flow::new(&self.flows);
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
                        let _ = copy_bidirectional_with_sizes(&mut app, &mut remote, 16384, 16384).await;
                        Ok(())
                    } => result,
                }
            }
            Destination::Peer {
                owner,
                service,
                target,
            } => {
                self.with_peer(&owner, Open { service, target }, async |tx, rx| {
                    let mut remote = tokio::io::join(rx, tx);
                    let _ =
                        copy_bidirectional_with_sizes(&mut app, &mut remote, 16384, 16384).await;
                    Ok(())
                })
                .await
            }
        }
    }

    // Local IPC only: use the publisher process and its actual underlay socket, never a GUI socket.
    async fn test_local_site(&self, id: &str, target: SocketAddrV4) -> Result<u16, Code> {
        {
            let state = self.state.lock().unwrap();
            if state.local.session.is_none() {
                return Err(Code::NotConnected);
            }
            let site = state
                .local
                .shares
                .iter()
                .find(|s| s.id == id)
                .ok_or(Code::Offline)?;
            if !site.http || !site.rules.allows(*target.ip(), target.port()) {
                return Err(Code::Unauthorized);
            }
        }
        let _flow = Flow::new(&self.flows);
        probe::http(platform::tcp(&*self.platform, target).await?, target).await
    }

    async fn probe_host(
        &self,
        ip: Ipv4Addr,
        extra_ports: &[u16],
        ports: &[u16],
    ) -> Result<probe::HostProbe, Code> {
        if self.state.lock().unwrap().local.session.is_none() {
            return Err(Code::NotConnected);
        }
        let Ok(_guard) = self.host_probe_lock.try_lock() else {
            return Err(Code::Busy);
        };
        let _flow = Flow::new(&self.flows);
        if ports.is_empty() {
            probe::host(&*self.platform, ip, extra_ports).await
        } else {
            probe::ports(&*self.platform, ip, ports).await
        }
    }

    async fn test_connection(
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
                let access = SocketAddrV4::new(routes.access_ip(*address.ip()), address.port());
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
        let _flow = Flow::new(&self.flows);
        let started = Instant::now();
        timeout(
            Duration::from_secs(24),
            self.with_peer(&owner, request, async |mut tx, mut rx| {
                let mut outcome = match kind {
                    ServiceKind::Http => {
                        let code = probe::http(tokio::io::join(rx, tx), access).await?;
                        probe::ProbeOutcome {
                            kind,
                            rtt_ms: 0,
                            http_status: Some(code),
                            host_os: None,
                            detail: None,
                        }
                    }
                    ServiceKind::Https => {
                        probe::tls(tokio::io::join(rx, tx), Duration::from_secs(5)).await?;
                        probe::ProbeOutcome {
                            kind,
                            rtt_ms: 0,
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
                            rtt_ms: 0,
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
                            rtt_ms: 0,
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
                            rtt_ms: 0,
                            http_status: None,
                            host_os: None,
                            detail: None,
                        }
                    }
                };
                outcome.rtt_ms = started.elapsed().as_millis().clamp(1, u32::MAX as u128) as u32;
                Ok(outcome)
            }),
        )
        .await
        .map_err(|_| Code::DirectFailed)?
    }

    async fn with_peer<R>(
        &self,
        owner: &str,
        request: Open,
        on_open: impl AsyncFnOnce(quinn::SendStream, quinn::RecvStream) -> Result<R, Code>,
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
                let (tx, rx) = direct::open(&conn, request).await?;
                on_open(tx, rx).await
            } => result,
        }
    }
}

struct Flow<'a>(&'a AtomicUsize);
impl<'a> Flow<'a> {
    fn new(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        Self(counter)
    }
}
impl Drop for Flow<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fesen_proto::Peer;

    struct LocalPlatform;
    impl Platform for LocalPlatform {
        fn network(&self) -> io::Result<Network> {
            Ok(Network {
                marker: "test".into(),
                addresses: vec![Ipv4Addr::new(10, 0, 0, 1)],
                ..Network::default()
            })
        }
        fn protect(&self, _: &socket2::Socket, _: Option<Ipv4Addr>) -> io::Result<()> {
            Ok(())
        }
        fn apply(&self, _: &Tun) -> io::Result<()> {
            Ok(())
        }
        fn write_packet(&self, _: &[u8]) {}
        fn set_input(&self, _: tokio::sync::mpsc::Sender<Vec<u8>>) {}
    }

    #[tokio::test]
    async fn closing_old_flows_does_not_hide_the_sharing_pause() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::new(dir.path().to_path_buf(), Arc::new(LocalPlatform), false).unwrap();
        core.report(Code::NetworkChanged);
        core.report(Code::Offline);
        core.report(Code::NotConnected);
        assert_eq!(core.status().error, Some(Code::NetworkChanged));
        core.execute(Command::Stop).await.unwrap();
        core.report(Code::Offline);
        assert_eq!(core.status().error, Some(Code::Offline));
        core.stop();
        core.wait_stopped().await;
    }

    #[tokio::test]
    async fn peer_leaving_the_directory_is_not_a_global_connection_failure() {
        let dir = tempfile::tempdir().unwrap();
        let platform = Arc::new(LocalPlatform);
        let core = Core::new(dir.path().to_path_buf(), platform.clone(), false).unwrap();
        let direct = direct::Direct::new(
            Identity::new().unwrap(),
            &*platform,
            core.quit.child_token(),
            false,
        )
        .unwrap();
        let link = Link::isolated(direct);
        core.state.lock().unwrap().link = Some(link.clone());
        let view = View {
            peers: vec![Peer {
                id: "peer".into(),
                certificate: Identity::new().unwrap().cert,
            }],
            services: vec![Offering {
                owner: "peer".into(),
                member: "member".into(),
                service: Publication {
                    id: "fixture".into(),
                    name: "Site".into(),
                    http: true,
                    kind: None,
                    host_os: None,
                    rules: Rules::parse("10.0.0.2", "80").unwrap(),
                },
            }],
        };
        core.update_view(&link, view.clone());
        assert_eq!(core.status().services.len(), 1);
        core.update_view(&link, View::default());
        assert!(core.status().services.is_empty());
        assert_eq!(core.status().error, None);
        core.update_view(&link, view);
        assert_eq!(core.status().services.len(), 1);
        assert_eq!(core.status().error, None);
        core.stop();
        core.wait_stopped().await;
    }

    #[tokio::test]
    async fn publisher_rechecks_each_open_on_the_same_authenticated_connection() {
        let dir = tempfile::tempdir().unwrap();
        let platform = Arc::new(LocalPlatform);
        let core = Core::new(dir.path().to_path_buf(), platform.clone(), false).unwrap();
        let owner = Identity::new().unwrap();
        let member = Identity::new().unwrap();
        let server =
            direct::Direct::new(owner.clone(), &*platform, core.quit.child_token(), false).unwrap();
        let destination = SocketAddrV4::new(Ipv4Addr::LOCALHOST, server.local_port());
        let verified =
            direct::Direct::new(member.clone(), &*platform, core.quit.child_token(), false)
                .unwrap();
        let rogue = direct::Direct::new(
            Identity::new().unwrap(),
            &*platform,
            core.quit.child_token(),
            false,
        )
        .unwrap();
        let link = Link::isolated(server.clone());
        server.renew(vec![Peer {
            id: "reader".into(),
            certificate: member.cert,
        }]);
        for node in [&verified, &rogue] {
            node.renew(vec![Peer {
                id: link.id.clone(),
                certificate: owner.cert.clone(),
            }]);
        }
        let service = Publication {
            id: "0123456789abcdef0123456789abcdef".into(),
            name: "Test".into(),
            rules: Rules::parse("10.0.0.2", "443").unwrap(),
            http: false,
            kind: None,
            host_os: None,
        };
        {
            let mut state = core.state.lock().unwrap();
            state.link = Some(link.clone());
            state.sharing.insert(
                service.id.clone(),
                Sharing {
                    publication: service.clone(),
                    cancel: server.cancel.child_token(),
                },
            );
        }
        tokio::spawn(core.clone().incoming(link.clone()));
        let (conn, _) = verified
            .connect(&link.id, || async { Ok(vec![destination.into()]) })
            .await
            .unwrap();
        for target in ["10.0.0.2:444", "10.0.0.3:443"] {
            assert!(matches!(
                direct::open(
                    &conn,
                    Open {
                        service: service.id.clone(),
                        target: target.parse().unwrap()
                    }
                )
                .await,
                Err(Code::Unauthorized)
            ));
        }
        core.restrict_local();
        assert!(matches!(
            direct::open(
                &conn,
                Open {
                    service: service.id.clone(),
                    target: "10.0.0.2:443".parse().unwrap(),
                }
            )
            .await,
            Err(Code::Unauthorized)
        ));
        if let Ok((untrusted, _)) = rogue
            .connect(&link.id, || async { Ok(vec![destination.into()]) })
            .await
        {
            let response = direct::open(
                &untrusted,
                Open {
                    service: service.id,
                    target: "10.0.0.2:443".parse().unwrap(),
                },
            )
            .await;
            assert!(matches!(response, Err(Code::DirectFailed)));
        }
        core.stop();
        core.wait_stopped().await;
    }

    #[tokio::test]
    async fn detached_idle_core_shuts_down_and_releases_loopback_ipc() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::new(dir.path().to_path_buf(), Arc::new(LocalPlatform), false).unwrap();
        #[cfg(not(windows))]
        {
            let ipc = crate::ipc::Endpoint::bind(dir.path()).await.unwrap();
            let task = tokio::spawn(ipc.serve(core.clone()));
            let result =
                crate::ipc::request(dir.path(), &serde_json::json!({"op":"status", "ui":true}))
                    .await
                    .unwrap();
            assert_eq!(result["data"]["background"], false);
            core.execute(Command::Detach).await.unwrap();
            let _ = core.execute(Command::Status { ui: true }).await;
            timeout(Duration::from_secs(4), task)
                .await
                .unwrap()
                .unwrap();
            assert!(!dir.path().join("ipc.json").exists());
        }
        #[cfg(windows)]
        {
            core.execute(Command::Detach).await.unwrap();
            let _ = core.execute(Command::Status { ui: true }).await;
            timeout(Duration::from_secs(4), core.wait_stopped())
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn save_host_merges_ports_propagates_os_and_enforces_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::new(dir.path().to_path_buf(), Arc::new(LocalPlatform), false).unwrap();

        assert_eq!(
            core.execute(Command::ProbeHost {
                ip: Ipv4Addr::new(198, 51, 100, 35),
                extra_ports: vec![],
                ports: vec![],
            })
            .await
            .err(),
            Some(Code::NotConnected)
        );

        assert_eq!(
            core.execute(Command::SaveHost {
                ip: Ipv4Addr::LOCALHOST,
                name: "bad".into(),
                host_os: Some(HostOs::Windows),
                services: vec![HostServiceInput {
                    port: 22,
                    kind: ServiceKind::Ssh,
                }],
                publish: false,
            })
            .await
            .err(),
            Some(Code::Invalid)
        );
        assert_eq!(
            core.execute(Command::SaveHost {
                ip: Ipv4Addr::new(198, 51, 100, 35),
                name: "dup".into(),
                host_os: None,
                services: vec![
                    HostServiceInput {
                        port: 22,
                        kind: ServiceKind::Ssh,
                    },
                    HostServiceInput {
                        port: 22,
                        kind: ServiceKind::Tcp,
                    },
                ],
                publish: false,
            })
            .await
            .err(),
            Some(Code::Invalid)
        );

        let s1 = core
            .execute(Command::Save {
                id: None,
                name: "old-name".into(),
                ips: "198.51.100.35".into(),
                ports: "80".into(),
                http: true,
                kind: None,
                host_os: None,
            })
            .await
            .unwrap();
        assert_eq!(s1.shares.len(), 1);
        let original_id = s1.shares[0].id.clone();

        let s2 = core
            .execute(Command::SaveHost {
                ip: Ipv4Addr::new(198, 51, 100, 35),
                name: "WinHost".into(),
                host_os: Some(HostOs::Windows),
                services: vec![
                    HostServiceInput {
                        port: 22,
                        kind: ServiceKind::Ssh,
                    },
                    HostServiceInput {
                        port: 3389,
                        kind: ServiceKind::Rdp,
                    },
                    HostServiceInput {
                        port: 80,
                        kind: ServiceKind::Http,
                    },
                ],
                publish: false,
            })
            .await
            .unwrap();
        assert_eq!(s2.shares.len(), 3);
        let http_entry = s2.shares.iter().find(|s| s.id == original_id).unwrap();
        assert_eq!(http_entry.name, "WinHost");
        assert_eq!(http_entry.kind, Some(ServiceKind::Http));
        assert!(http_entry.http);
        assert!(s2.shares.iter().all(|s| s.host_os == Some(HostOs::Windows)));

        // Removing both SSH and RDP clears the host_os badge from the remaining HTTP share.
        for s in s2.shares.iter().filter(|s| s.id != original_id) {
            core.execute(Command::Remove { id: s.id.clone() })
                .await
                .unwrap();
        }
        let s3 = core.status();
        assert_eq!(s3.shares.len(), 1);
        assert_eq!(s3.shares[0].id, original_id);
        assert_eq!(s3.shares[0].host_os, None);

        core.stop();
        core.wait_stopped().await;
    }

    #[tokio::test]
    async fn stopping_access_closes_a_local_fallback_connection() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let dir = tempfile::tempdir().unwrap();
        let platform = Arc::new(LocalPlatform);
        let core = Core::new(dir.path().into(), platform.clone(), false).unwrap();
        let direct = direct::Direct::new(
            Identity::new().unwrap(),
            &*platform,
            core.quit.child_token(),
            false,
        )
        .unwrap();
        core.state.lock().unwrap().link = Some(Link::isolated(direct));
        let local = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let std::net::SocketAddr::V4(target) = local.local_addr().unwrap() else {
            unreachable!();
        };
        let (mut app, stream) = tokio::io::duplex(1024);
        let forward = tokio::spawn({
            let core = core.clone();
            async move { core.forward(stream, target).await }
        });
        let (mut target, _) = timeout(Duration::from_secs(2), local.accept())
            .await
            .unwrap()
            .unwrap();
        app.write_all(b"ok").await.unwrap();
        let mut data = [0; 2];
        timeout(Duration::from_secs(2), target.read_exact(&mut data))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&data, b"ok");
        core.execute(Command::Stop).await.unwrap();
        timeout(Duration::from_secs(2), forward)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(app.read(&mut data).await.unwrap(), 0);
        assert!(!core.status().background);
        core.stop();
        core.wait_stopped().await;
    }
}
