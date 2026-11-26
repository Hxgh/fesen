use crate::{
    admin_client,
    control::{Link, Session},
    direct::{self, Allowed},
    local::{Identity, Local, Log, Login, private_dir},
    packets,
    platform::{self, DeviceInfo, Network, Platform, Tun},
    probe,
    routing::{Destination, Label, Mapping, Routes, alias_name},
};
use fesen_acl::Rules;
use fesen_proto::{
    Call, Code, Credential, DeviceOs, DeviceProfile, HostOs, LocalForwardStatus, LocalMapping,
    MemberPref, Offering, Open, Publication, ServiceKind, View, origin, valid_id, valid_label,
    valid_machine_id, valid_username,
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
    net::TcpListener,
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
        #[serde(default)]
        label: String,
    },
    RenameDevice {
        label: String,
    },
    Start,
    Stop,
    Quit,
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
        #[serde(default)]
        deep: bool,
    },
    CancelProbeHost,
    AdminSession,
    AdminAction {
        action: serde_json::Value,
    },
    SetPreferences {
        #[serde(default)]
        auto_connect: Option<bool>,
        #[serde(default)]
        auto_share: Option<bool>,
        #[serde(default)]
        cloud_backup: Option<bool>,
    },
    SetLocalForward {
        service: String,
        target_ip: Ipv4Addr,
        target_port: u16,
        local_port: u16,
    },
    SuggestLocalPort {
        #[serde(default)]
        preferred: Option<u16>,
    },
    ImportProfile {
        machine_id: String,
        #[serde(default = "default_true")]
        import_shares: bool,
        #[serde(default = "default_true")]
        import_forwards: bool,
    },
    DeleteProfile {
        machine_id: String,
    },
    DismissRestore,
}

fn default_true() -> bool {
    true
}

fn default_device_os() -> DeviceOs {
    DeviceOs::Linux
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Status {
    pub server: String,
    pub member: Option<String>,
    #[serde(default)]
    pub machine_id: String,
    #[serde(default = "default_device_os")]
    pub device_os: DeviceOs,
    #[serde(default)]
    pub device_name: String,
    pub online: bool,
    pub running: bool,
    pub background: bool,
    pub system_routing: bool,
    #[serde(default)]
    pub routes_ready: bool,
    #[serde(default)]
    pub is_admin: bool,
    #[serde(default)]
    pub admin_ready: bool,
    pub services: Vec<Offering>,
    pub shares: Vec<Publication>,
    pub active: Vec<String>,
    #[serde(default)]
    pub active_flows: usize,
    pub labels: Vec<Label>,
    pub virtual_ips: Vec<Mapping>,
    #[serde(default)]
    pub service_virtual_ips: HashMap<String, Vec<Mapping>>,
    pub aliases: HashMap<String, Ipv4Addr>,
    #[serde(default)]
    pub forwards: Vec<LocalForwardStatus>,
    #[serde(default)]
    pub cloud_profiles: Vec<DeviceProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore_prompt: Option<DeviceProfile>,
    pub error: Option<Code>,
    #[serde(default = "default_true")]
    pub auto_connect: bool,
    #[serde(default = "default_true")]
    pub auto_share: bool,
    #[serde(default = "default_true")]
    pub cloud_backup: bool,
    #[serde(default)]
    pub auto_share_blocked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe: Option<probe::ProbeOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_probe: Option<probe::HostProbe>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admin_data: Option<serde_json::Value>,
}

type ForwardKey = (String, Ipv4Addr, u16);

struct ForwardHandle {
    owner: String,
    local_port: u16,
    cancel: CancellationToken,
    closed: tokio::sync::oneshot::Receiver<()>,
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
    forward_states: HashMap<ForwardKey, LocalForwardStatus>,
    cloud_profiles: Vec<DeviceProfile>,
    profile_synced: bool,
    dismissed_restore: bool,
    wanted: bool,
    restore_pending: bool,
    auto_share_blocked: bool,
    ui_until: Option<Instant>,
    error: Option<Code>,
    is_admin: bool,
    admin_token: Option<String>,
}

pub struct Core {
    dir: PathBuf,
    device_info: DeviceInfo,
    state: Mutex<State>,
    pub(crate) platform: Arc<dyn Platform>,
    log: Log,
    operations: tokio::sync::Mutex<()>,
    forwards_lock: tokio::sync::Mutex<HashMap<ForwardKey, ForwardHandle>>,
    host_probe_lock: tokio::sync::Mutex<()>,
    probe_cancel: Mutex<Option<CancellationToken>>,
    network_probe: tokio::sync::Mutex<Instant>,
    pub(crate) routes: RwLock<Routes>,
    changes: watch::Sender<u64>,
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
        let log = Log::new(&dir);
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
                ui_until: Some(Instant::now() + Duration::from_secs(20)),
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

    fn device_name(&self, local: &Local) -> String {
        local
            .device_name
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| self.device_info.name.clone())
    }

    pub fn status(&self) -> Status {
        let state = self.state.lock().unwrap();
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
            server: state.local.server.clone(),
            member: state.local.session.as_ref().map(|s| s.member.clone()),
            machine_id: self.device_info.machine_id.clone(),
            device_os: self.device_info.os,
            device_name: self.device_name(&state.local),
            online: self.platform.access_ready()
                && state
                    .link
                    .as_ref()
                    .is_some_and(|l| l.direct.membership.is_fresh()),
            running: state.wanted,
            background: self.has_work(&state),
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
            Command::Quit => {
                self.stop();
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
                deep,
            } => {
                let host_probe = self.probe_host(ip, &extra_ports, &ports, deep).await?;
                return Ok(Status {
                    host_probe: Some(host_probe),
                    ..self.status()
                });
            }
            Command::CancelProbeHost => {
                let token = self.probe_cancel.lock().unwrap().take();
                if let Some(tok) = token {
                    tok.cancel();
                    let _ = timeout(Duration::from_millis(200), self.host_probe_lock.lock()).await;
                }
                return Ok(self.status());
            }
            Command::AdminSession => {
                let (server, token) = {
                    let state = self.state.lock().unwrap();
                    (
                        state.local.server.clone(),
                        state.admin_token.clone().ok_or(Code::Unauthorized)?,
                    )
                };
                match admin_client::session(&*self.platform, &server, &token).await {
                    Ok(data) => {
                        return Ok(Status {
                            admin_data: Some(data),
                            ..self.status()
                        });
                    }
                    Err(e) => {
                        if e == Code::Unauthorized {
                            self.state.lock().unwrap().admin_token = None;
                        }
                        return Err(e);
                    }
                }
            }
            Command::AdminAction { action } => {
                let (server, token) = {
                    let state = self.state.lock().unwrap();
                    (
                        state.local.server.clone(),
                        state.admin_token.clone().ok_or(Code::Unauthorized)?,
                    )
                };
                let is_logout = action.get("op").and_then(|v| v.as_str()) == Some("logout");
                match admin_client::action(&*self.platform, &server, &token, &action).await {
                    Ok(()) => {
                        if is_logout {
                            self.state.lock().unwrap().admin_token = None;
                        }
                        return Ok(self.status());
                    }
                    Err(e) => {
                        if e == Code::Unauthorized {
                            self.state.lock().unwrap().admin_token = None;
                        }
                        return Err(e);
                    }
                }
            }
            Command::SetPreferences {
                auto_connect,
                auto_share,
                cloud_backup,
            } => {
                self.edit(|local| {
                    if let Some(v) = auto_connect {
                        local.auto_connect = v;
                    }
                    if let Some(v) = auto_share {
                        local.auto_share = v;
                    }
                    if let Some(v) = cloud_backup {
                        local.set_cloud_backup(v)?;
                    }
                    Ok(())
                })?;
                if auto_share == Some(false) {
                    self.state.lock().unwrap().auto_share_blocked = false;
                }
                if cloud_backup == Some(true) {
                    self.push_profile_bg();
                }
                return Ok(self.status());
            }
            Command::SuggestLocalPort { preferred } => {
                let used: HashSet<u16> = {
                    let state = self.state.lock().unwrap();
                    state.local.forwards().iter().map(|f| f.local_port).collect()
                };
                let port = suggest_free_port(preferred, &used).ok_or(Code::Conflict)?;
                return Ok(Status {
                    suggested_port: Some(port),
                    ..self.status()
                });
            }
            Command::DismissRestore => {
                self.state.lock().unwrap().dismissed_restore = true;
                return Ok(self.status());
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
            | Command::Quit
            | Command::TestConnection { .. }
            | Command::TestLocalSite { .. }
            | Command::ProbeHost { .. }
            | Command::CancelProbeHost
            | Command::AdminSession
            | Command::AdminAction { .. }
            | Command::SetPreferences { .. }
            | Command::SuggestLocalPort { .. }
            | Command::DismissRestore => unreachable!(),
            Command::Join { address } => {
                let address = origin(&address)?.to_string();
                let res = self.edit(|local| {
                    if local.server == address {
                        return Ok(());
                    }
                    if local.session.is_some() {
                        return Err(Code::Busy);
                    }
                    local.server = address;
                    local.clear_group();
                    Ok(())
                });
                if res.is_ok() {
                    let mut s = self.state.lock().unwrap();
                    s.is_admin = false;
                    s.admin_token = None;
                    s.cloud_profiles.clear();
                    s.profile_synced = false;
                    s.dismissed_restore = false;
                }
                res
            }
            Command::Login {
                username,
                password,
                label,
            } => {
                let auto_name = {
                    let s = self.state.lock().unwrap();
                    self.device_name(&s.local)
                };
                let label = if label.trim().is_empty() {
                    auto_name
                } else {
                    label.trim().to_owned()
                };
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
            Command::RenameDevice { label } => {
                let trimmed = label.trim().to_owned();
                if !valid_label(&trimmed) {
                    return Err(Code::Invalid);
                }
                let link = self.state.lock().unwrap().link.clone();
                if let Some(link) = link {
                    link.call(Call::RenameDevice {
                        label: trimmed.clone(),
                    })
                    .await?;
                }
                self.edit(|local| {
                    local.device_name = Some(trimmed);
                    Ok(())
                })?;
                self.push_profile_bg();
                Ok(())
            }
            Command::Start => {
                {
                    let mut state = self.state.lock().unwrap();
                    state.auto_share_blocked = false;
                    if state.link.is_none() && state.local.auto_share {
                        let (active, _) = state.local.active_shares();
                        state.restore_pending = !active.is_empty();
                    }
                }
                let res = self.resume().await;
                if res.is_err() {
                    self.state.lock().unwrap().restore_pending = false;
                }
                res
            }
            Command::Stop => {
                self.disconnect(false);
                self.reconcile_forwards().await;
                let mut state = self.state.lock().unwrap();
                state.restore_pending = false;
                state.auto_share_blocked = false;
                state.error = None;
                Ok(())
            }
            Command::Logout => {
                let (live, server, admin_token) = {
                    let mut s = self.state.lock().unwrap();
                    s.is_admin = false;
                    s.restore_pending = false;
                    s.auto_share_blocked = false;
                    s.cloud_profiles.clear();
                    s.profile_synced = false;
                    s.dismissed_restore = false;
                    (
                        s.link.clone(),
                        s.local.server.clone(),
                        s.admin_token.take(),
                    )
                };
                if let Some(tok) = admin_token {
                    let _ = admin_client::action(
                        &*self.platform,
                        &server,
                        &tok,
                        &serde_json::json!({"op": "logout"}),
                    )
                    .await;
                }
                // Close functioning streams before asking the server to invalidate the token.
                self.restrict_local();
                if let Some(link) = live {
                    let _ = link.call(Call::Logout).await;
                }
                self.disconnect(false);
                self.reconcile_forwards().await;
                self.edit(|l| {
                    l.clear_active_shares();
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
                    let shares = local.shares_mut()?;
                    if let Some(old) = shares.iter_mut().find(|s| s.id == id) {
                        *old = rule;
                    } else if shares.len() < 32 {
                        shares.push(rule);
                    } else {
                        return Err(Code::Limit);
                    }
                    Ok(())
                })?;
                self.push_profile_bg();
                Ok(())
            }
            Command::SaveHost {
                ip,
                name,
                host_os,
                services,
                publish,
            } => {
                self.save_host(ip, name, host_os, services, publish).await?;
                self.push_profile_bg();
                Ok(())
            }
            Command::Share { id, enabled } => self.share(id, enabled).await,
            Command::ShareMany { ids, enabled } => self.share_many(ids, enabled).await,
            Command::Remove { id } => {
                self.share(id.clone(), false).await?;
                self.edit(|l| {
                    l.record_share_active(&id, false, "");
                    let shares = l.shares_mut()?;
                    let removed_ip = shares
                        .iter()
                        .find(|s| s.id == id)
                        .and_then(Publication::single_ip);
                    shares.retain(|s| s.id != id);
                    if let Some(ip) = removed_ip {
                        let keeps_os = shares.iter().any(|s| {
                            s.single_ip() == Some(ip)
                                && matches!(
                                    s.effective_kind(),
                                    ServiceKind::Ssh | ServiceKind::Rdp
                                )
                        });
                        if !keeps_os {
                            for s in shares {
                                if s.single_ip() == Some(ip) {
                                    s.host_os = None;
                                }
                            }
                        }
                    }
                    Ok(())
                })?;
                self.push_profile_bg();
                Ok(())
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
                let remark = remark.trim().to_owned();
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
                    let labels = local.labels_mut()?;
                    if let Some(alias) = &alias
                        && labels
                            .iter()
                            .any(|l| l.service != id && l.alias.as_ref() == Some(alias))
                    {
                        return Err(Code::Conflict);
                    }
                    labels.retain(|l| l.service != id);
                    if !remark.is_empty() || alias.is_some() {
                        if labels.len() >= 256 {
                            return Err(Code::Limit);
                        }
                        labels.push(Label {
                            service: id.clone(),
                            remark: remark.clone(),
                            alias: alias.clone(),
                            ip,
                        });
                    }
                    local.mark_pref_dirty(&id);
                    Ok(())
                })?;
                let link = self.state.lock().unwrap().link.clone();
                if let Some(link) = link {
                    let core = self.clone();
                    let pref = MemberPref {
                        service: id.clone(),
                        remark,
                        alias,
                        ip,
                    };
                    tokio::spawn(async move {
                        if link.call(Call::SavePref { pref }).await.is_ok() {
                            let _ = core.edit(|local| {
                                if let Some(member) = local.active_member().map(str::to_owned)
                                    && let Some(acct) = local.accounts.get_mut(&member)
                                {
                                    acct.dirty_prefs.retain(|s| s != &id);
                                }
                                Ok(())
                            });
                        }
                    });
                }
                self.changed();
                Ok(())
            }
            Command::SetLocalForward {
                service,
                target_ip,
                target_port,
                local_port,
            } => {
                if !valid_id(&service) || target_port == 0 {
                    return Err(Code::Invalid);
                }
                if local_port == 0 {
                    self.edit(|local| {
                        local.forwards_mut()?.retain(|f| {
                            !(f.service == service
                                && f.target_ip == target_ip
                                && f.target_port == target_port)
                        });
                        Ok(())
                    })?;
                    self.reconcile_forwards().await;
                    self.push_profile_bg();
                    return Ok(());
                }
                {
                    let listeners = self.forwards_lock.lock().await;
                    let key = (service.clone(), target_ip, target_port);
                    let already_bound = listeners
                        .get(&key)
                        .is_some_and(|h| h.local_port == local_port);
                    {
                        let state = self.state.lock().unwrap();
                        if let Some(o) = state.view.iter().find(|o| o.service.id == service)
                            && !o.service.rules.allows(target_ip, target_port)
                        {
                            return Err(Code::Invalid);
                        }
                        if state.local.forwards().iter().any(|f| {
                            f.local_port == local_port
                                && !(f.service == service
                                    && f.target_ip == target_ip
                                    && f.target_port == target_port)
                        }) {
                            return Err(Code::Conflict);
                        }
                    }
                    if !already_bound
                        && std::net::TcpListener::bind(SocketAddrV4::new(
                            Ipv4Addr::LOCALHOST,
                            local_port,
                        ))
                        .is_err()
                    {
                        return Err(Code::Conflict);
                    }
                }
                self.edit(|local| {
                    let list = local.forwards_mut()?;
                    if let Some(existing) = list.iter_mut().find(|f| {
                        f.service == service
                            && f.target_ip == target_ip
                            && f.target_port == target_port
                    }) {
                        existing.local_port = local_port;
                    } else if list.len() < 64 {
                        list.push(LocalMapping {
                            service: service.clone(),
                            target_ip,
                            target_port,
                            local_port,
                        });
                    } else {
                        return Err(Code::Limit);
                    }
                    Ok(())
                })?;
                self.reconcile_forwards().await;
                self.push_profile_bg();
                self.changed();
                Ok(())
            }
            Command::ImportProfile {
                machine_id,
                import_shares,
                import_forwards,
            } => {
                if !valid_machine_id(&machine_id) {
                    return Err(Code::Invalid);
                }
                let profile = {
                    let state = self.state.lock().unwrap();
                    state
                        .cloud_profiles
                        .iter()
                        .find(|p| p.machine_id == machine_id)
                        .cloned()
                        .ok_or(Code::Invalid)?
                };
                let same_machine = profile.machine_id == self.device_info.machine_id;
                self.edit(|local| {
                    if import_shares {
                        let shares = local.shares_mut()?;
                        for s in &profile.shares {
                            if shares.len() >= 32 {
                                break;
                            }
                            if shares.iter().any(|cur| cur.rules == s.rules && cur.name == s.name) {
                                continue;
                            }
                            let mut imported = s.clone();
                            if !same_machine || shares.iter().any(|cur| cur.id == imported.id) {
                                imported.id = hex::encode(rand::random::<[u8; 16]>());
                            }
                            shares.push(imported);
                        }
                    }
                    if import_forwards {
                        let forwards = local.forwards_mut()?;
                        for f in &profile.forwards {
                            if forwards.len() >= 64 {
                                break;
                            }
                            if forwards.iter().any(|cur| {
                                cur.local_port == f.local_port
                                    || (cur.service == f.service
                                        && cur.target_ip == f.target_ip
                                        && cur.target_port == f.target_port)
                            }) {
                                continue;
                            }
                            forwards.push(f.clone());
                        }
                    }
                    Ok(())
                })?;
                self.state.lock().unwrap().dismissed_restore = true;
                self.reconcile_forwards().await;
                self.push_profile_bg();
                self.changed();
                Ok(())
            }
            Command::DeleteProfile { machine_id } => {
                if !valid_machine_id(&machine_id) {
                    return Err(Code::Invalid);
                }
                let link = self.state.lock().unwrap().link.clone().ok_or(Code::NotConnected)?;
                link.call(Call::DeleteProfile {
                    machine_id: machine_id.clone(),
                })
                .await?;
                self.state
                    .lock()
                    .unwrap()
                    .cloud_profiles
                    .retain(|p| p.machine_id != machine_id);
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
                && link.call(Call::Unpublish { id: id.clone() }).await.is_err()
            {
                self.disconnect(true);
                return Err(Code::NotConnected);
            }
            let _ = self.edit(|local| {
                local.record_share_active(&id, false, "");
                Ok(())
            });
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
                .shares()
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
        let mut next = state.local.clone();
        next.record_share_active(&id, true, &net.marker);
        let _ = next.save(&self.dir);
        state.local = next;
        state.auto_share_blocked = false;
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
                .any(|id| !state.local.shares().iter().any(|s| &s.id == id))
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
            let sharing_ids: HashSet<String> = state.sharing.keys().cloned().collect();
            let mut next = state.local.clone();
            let mut target_ids = Vec::with_capacity(services.len());
            let mut new_entries = Vec::new();
            {
                let shares = next.shares_mut()?;
                for svc in &services {
                    let existing = shares.iter_mut().find(|s| {
                        s.single_ip() == Some(ip)
                            && matches!(
                                s.rules.ports.as_slice(),
                                [p] if p.start == svc.port && p.end == svc.port
                            )
                    });
                    if let Some(share) = existing {
                        target_ids.push(share.id.clone());
                        if !sharing_ids.contains(&share.id) {
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
                if shares.len() + new_entries.len() > 32 {
                    return Err(Code::Limit);
                }
                if let Some(os) = host_os {
                    for share in shares.iter_mut() {
                        if share.single_ip() == Some(ip) && !sharing_ids.contains(&share.id) {
                            share.host_os = Some(os);
                        }
                    }
                }
                shares.extend(new_entries);
            }
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

    fn push_profile_bg(self: &Arc<Self>) {
        let (link, profile) = {
            let state = self.state.lock().unwrap();
            if !state.local.cloud_backup() {
                return;
            }
            let Some(link) = state.link.clone() else {
                return;
            };
            let profile = DeviceProfile {
                machine_id: self.device_info.machine_id.clone(),
                name: self.device_name(&state.local),
                os: self.device_info.os,
                shares: state.local.shares().to_vec(),
                forwards: state.local.forwards().to_vec(),
                updated_at: 0,
            };
            (link, profile)
        };
        tokio::spawn(async move {
            let _ = link.call(Call::SaveProfile { profile }).await;
        });
    }

    pub(crate) fn update_sync(
        self: &Arc<Self>,
        link: &Arc<Link>,
        prefs: Vec<MemberPref>,
        profiles: Vec<DeviceProfile>,
    ) {
        let (labels_changed, to_push, initial_profile) = {
            let mut state = self.state.lock().unwrap();
            if !Self::is_link(&state, link) {
                return;
            }
            state.cloud_profiles = profiles;
            let initial_profile = if !state.profile_synced {
                state.profile_synced = true;
                if state.local.cloud_backup()
                    && (!state.local.shares().is_empty() || !state.local.forwards().is_empty())
                {
                    Some(DeviceProfile {
                        machine_id: self.device_info.machine_id.clone(),
                        name: self.device_name(&state.local),
                        os: self.device_info.os,
                        shares: state.local.shares().to_vec(),
                        forwards: state.local.forwards().to_vec(),
                        updated_at: 0,
                    })
                } else {
                    None
                }
            } else {
                None
            };
            let dirty: HashSet<String> = state.local.dirty_prefs().iter().cloned().collect();
            let before = state.local.labels().to_vec();
            let mut merged: Vec<Label> = Vec::new();
            for p in &prefs {
                if !dirty.contains(&p.service) {
                    merged.push(Label {
                        service: p.service.clone(),
                        remark: p.remark.clone(),
                        alias: p.alias.clone(),
                        ip: p.ip,
                    });
                }
            }
            let mut to_push: Vec<MemberPref> = Vec::new();
            for l in &before {
                if dirty.contains(&l.service) {
                    merged.push(l.clone());
                    to_push.push(MemberPref {
                        service: l.service.clone(),
                        remark: l.remark.clone(),
                        alias: l.alias.clone(),
                        ip: l.ip,
                    });
                }
            }
            for svc in &dirty {
                if !before.iter().any(|l| &l.service == svc) {
                    to_push.push(MemberPref {
                        service: svc.clone(),
                        remark: String::new(),
                        alias: None,
                        ip: None,
                    });
                }
            }
            let same = merged.len() == before.len()
                && merged.iter().zip(&before).all(|(a, b)| {
                    a.service == b.service
                        && a.remark == b.remark
                        && a.alias == b.alias
                        && a.ip == b.ip
                });
            if !same {
                let mut next = state.local.clone();
                if let Ok(labels) = next.labels_mut() {
                    *labels = merged;
                    if next.save(&self.dir).is_ok() {
                        state.local = next;
                    }
                }
            }
            (!same, to_push, initial_profile)
        };
        if labels_changed {
            self.changed();
        }
        if !to_push.is_empty() || initial_profile.is_some() {
            let core = self.clone();
            let link = link.clone();
            tokio::spawn(async move {
                let mut pushed_all = true;
                for pref in to_push {
                    if link.call(Call::SavePref { pref }).await.is_err() {
                        pushed_all = false;
                    }
                }
                if pushed_all {
                    let _ = core.edit(|local| {
                        local.clear_dirty_prefs();
                        Ok(())
                    });
                }
                if let Some(profile) = initial_profile {
                    let _ = link.call(Call::SaveProfile { profile }).await;
                }
            });
        }
    }

    pub(crate) fn update_view(self: &Arc<Self>, link: &Arc<Link>, view: View) {
        let mut restore_ids: Option<Vec<String>> = None;
        let changed = {
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
            let changed = services != state.view;
            if changed {
                state.view = services;
            }
            if state.restore_pending {
                state.restore_pending = false;
                if state.local.auto_share {
                    let net_known = state.network.known();
                    let net_marker = state.network.marker.clone();
                    let (active, share_net) = state.local.active_shares();
                    if !active.is_empty() {
                        if net_known && !share_net.is_empty() && net_marker == share_net {
                            restore_ids = Some(active.to_vec());
                            state.auto_share_blocked = false;
                        } else {
                            state.auto_share_blocked = true;
                        }
                    }
                }
            }
            changed
        };
        if changed {
            self.changed();
        }
        if let Some(ids) = restore_ids {
            let core = self.clone();
            let link = link.clone();
            tokio::spawn(async move {
                let _lock = core.operations.lock().await;
                let valid_ids: Vec<String> = {
                    let state = core.state.lock().unwrap();
                    if !Self::is_link(&state, &link) || core.quit.is_cancelled() {
                        return;
                    }
                    state
                        .local
                        .shares()
                        .iter()
                        .filter(|s| ids.contains(&s.id))
                        .map(|s| s.id.clone())
                        .collect()
                };
                if !valid_ids.is_empty() {
                    let _ = core.share_many(valid_ids, true).await;
                    core.changed();
                }
            });
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

    async fn forward_worker(self: Arc<Self>) {
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
            for h in drained {
                let _ = timeout(Duration::from_millis(200), h.closed).await;
            }
        }
        self.state.lock().unwrap().forward_states.clear();
        self.forwards_shutdown.cancel();
    }

    async fn reconcile_forwards(self: &Arc<Self>) {
        let mut listeners = self.forwards_lock.lock().await;
        let (online, configured, view) = {
            let state = self.state.lock().unwrap();
            let online = !self.quit.is_cancelled()
                && state.wanted
                && state
                    .link
                    .as_ref()
                    .is_some_and(|l| l.direct.membership.is_fresh());
            (
                online,
                state.local.forwards().to_vec(),
                state.view.clone(),
            )
        };
        if !online {
            let drained: Vec<_> = listeners.drain().map(|(_, h)| h).collect();
            for h in &drained {
                h.cancel.cancel();
            }
            for h in drained {
                let _ = timeout(Duration::from_millis(200), h.closed).await;
            }
            self.state.lock().unwrap().forward_states.clear();
            return;
        }
        let mut desired: HashMap<ForwardKey, (String, u16)> = HashMap::new();
        let mut used_ports: HashSet<u16> = HashSet::new();
        for m in &configured {
            used_ports.insert(m.local_port);
            if let Some(o) = view
                .iter()
                .find(|o| o.service.id == m.service && o.service.rules.allows(m.target_ip, m.target_port))
            {
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
        for handle in removed {
            let _ = timeout(Duration::from_millis(200), handle.closed).await;
        }
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
                Ok(std_listener)
                    if std_listener.set_nonblocking(true).is_ok() =>
                {
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
        self.state.lock().unwrap().forward_states = states;
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
                        let _ = copy_bidirectional_with_sizes(&mut client, &mut remote, 16384, 16384).await;
                        Ok(())
                    }) => {},
                }
            });
        }
        drop(listener);
        let _ = closed_tx.send(());
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
                self.with_peer(&owner, Open { service, target }, async |tx, rx, _| {
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
                .shares()
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
        let _flow = Flow::new(&self.flows);
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
                let access = SocketAddrV4::new(routes.access_ip_for(id, *address.ip()), address.port());
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

    async fn with_peer<R>(
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

fn suggest_free_port(preferred: Option<u16>, used: &HashSet<u16>) -> Option<u16> {
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
                device: None,
                device_os: None,
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
                deep: false,
            })
            .await
            .err(),
            Some(Code::NotConnected)
        );

        core.state.lock().unwrap().local.session = Some(Login {
            member: "alice".into(),
            token: "t".repeat(64),
        });

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

        // Switching local session to another member isolates shares cleanly, and switching back restores them.
        core.state.lock().unwrap().local.session = Some(Login {
            member: "bob".into(),
            token: "b".repeat(64),
        });
        assert!(core.status().shares.is_empty());
        core.state.lock().unwrap().local.session = Some(Login {
            member: "alice".into(),
            token: "t".repeat(64),
        });
        assert_eq!(core.status().shares.len(), 1);
        assert_eq!(core.status().shares[0].id, original_id);

        // Verify legacy flat device.json migrates automatically into accounts[member].
        let legacy_dir = tempfile::tempdir().unwrap();
        let legacy_json = serde_json::json!({
            "server": "https://example.com/",
            "session": { "member": "alice", "token": "a".repeat(64) },
            "shares": [{
                "id": original_id,
                "name": "Legacy",
                "rules": {
                    "ips": [{ "start": "198.51.100.35", "end": "198.51.100.35" }],
                    "ports": [{ "start": 80, "end": 80 }]
                },
                "http": true
            }]
        });
        crate::local::save(
            &legacy_dir.path().join("device.json"),
            legacy_json.to_string().as_bytes(),
        )
        .unwrap();
        let migrated = Local::load(legacy_dir.path()).unwrap();
        assert!(migrated.shares.is_empty());
        assert_eq!(migrated.shares().len(), 1);
        assert_eq!(migrated.shares()[0].name, "Legacy");

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

    #[tokio::test]
    async fn probe_host_can_be_cancelled_and_immediately_releases_lock() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::new(dir.path().to_path_buf(), Arc::new(LocalPlatform), false).unwrap();
        core.state.lock().unwrap().local.session = Some(Login {
            member: "alice".into(),
            token: "t".repeat(64),
        });

        let task = tokio::spawn({
            let core = core.clone();
            async move {
                core.execute(Command::ProbeHost {
                    ip: Ipv4Addr::new(198, 51, 100, 99),
                    extra_ports: vec![],
                    ports: vec![],
                    deep: true,
                })
                .await
            }
        });
        while core.probe_cancel.lock().unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        core.execute(Command::CancelProbeHost).await.unwrap();
        let res = timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(res.host_probe.unwrap().cancelled);

        // Lock is already free right after CancelProbeHost completes.
        let quick = core
            .execute(Command::ProbeHost {
                ip: Ipv4Addr::new(198, 51, 100, 99),
                extra_ports: vec![],
                ports: [80].into(),
                deep: false,
            })
            .await;
        assert!(quick.is_ok());

        core.stop();
        core.wait_stopped().await;
    }
}
