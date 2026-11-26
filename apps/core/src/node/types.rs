use crate::{
    control::Link,
    local::Local,
    platform::Network,
    probe,
    routing::{Label, Mapping},
};
use fesen_proto::{
    Code, DeviceOs, DeviceProfile, HostOs, LocalForwardStatus, Offering, Publication, ServiceKind,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    net::Ipv4Addr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

#[cfg(target_os = "android")]
pub(super) const COPY_BUF: usize = 32 * 1024;
#[cfg(not(target_os = "android"))]
pub(super) const COPY_BUF: usize = 64 * 1024;

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
        #[serde(default)]
        republish: bool,
    },
    SaveHost {
        ip: Ipv4Addr,
        name: String,
        #[serde(default)]
        host_os: Option<HostOs>,
        services: Vec<HostServiceInput>,
        #[serde(default)]
        publish: bool,
        #[serde(default)]
        replace: bool,
        #[serde(default)]
        old_host_name: Option<String>,
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
    RemoveMany {
        ids: Vec<String>,
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
    #[serde(default)]
    pub version: u64,
    pub server: String,
    pub member: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
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

pub(super) type ForwardKey = (String, Ipv4Addr, u16);

pub(super) struct ForwardHandle {
    pub(super) owner: String,
    pub(super) local_port: u16,
    pub(super) cancel: CancellationToken,
    pub(super) closed: tokio::sync::oneshot::Receiver<()>,
}

pub(super) struct Sharing {
    pub(super) publication: Publication,
    pub(super) cancel: CancellationToken,
}

pub(super) struct State {
    pub(super) local: Local,
    pub(super) link: Option<Arc<Link>>,
    pub(super) network: Network,
    pub(super) view: Vec<Offering>,
    pub(super) latest: HashSet<String>,
    pub(super) sharing: HashMap<String, Sharing>,
    pub(super) forward_states: HashMap<ForwardKey, LocalForwardStatus>,
    pub(super) cloud_profiles: Vec<DeviceProfile>,
    pub(super) profile_synced: bool,
    pub(super) dismissed_restore: bool,
    pub(super) wanted: bool,
    pub(super) restore_pending: bool,
    pub(super) auto_share_blocked: bool,
    pub(super) ui_until: Option<Instant>,
    pub(super) error: Option<Code>,
    pub(super) is_admin: bool,
    pub(super) admin_token: Option<String>,
}

pub(super) struct Flow<'a>(&'a AtomicUsize);

impl<'a> Flow<'a> {
    pub(super) fn new(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        Self(counter)
    }
}

impl Drop for Flow<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(super) struct HostUpdate {
    pub(super) ip: Ipv4Addr,
    pub(super) name: String,
    pub(super) host_os: Option<HostOs>,
    pub(super) services: Vec<HostServiceInput>,
    pub(super) publish: bool,
    pub(super) replace: bool,
    pub(super) old_host_name: Option<String>,
}
