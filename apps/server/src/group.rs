use crate::store;
use axum::extract::ws::Utf8Bytes;
use fesen_acl::{MAX_ADDRESSES, count, union};
use fesen_proto::{
    Call, Code, DeviceOs, Event, MAX_CANDIDATES, MAX_DEVICES, MAX_IPV6_CANDIDATES,
    MAX_PUBLICATIONS, Offering, Peer, Publication, View, candidate, candidate6,
};
use rusqlite::Connection;
use std::{
    collections::HashMap,
    net::{SocketAddrV4, SocketAddrV6},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use url::Url;

#[derive(Clone)]
pub struct App {
    pub inner: Arc<Mutex<Group>>,
    pub view_frame: watch::Sender<(u64, Utf8Bytes)>,
    pub password_slots: Arc<tokio::sync::Semaphore>,
    pub dummy_hash: Arc<String>,
    pub origin: Url,
    pub origin_header: Arc<str>,
    pub expected_authority: Arc<str>,
    pub join_link: Arc<str>,
    pub udp_port: u16,
    pub stun: Option<String>,
}

impl App {
    pub fn new(
        db: Connection,
        origin: Url,
        udp_port: u16,
        stun: Option<String>,
    ) -> Result<Self, Code> {
        let initial_frame = Utf8Bytes::from(
            serde_json::to_string(&Event::View {
                view: View::default(),
            })
            .map_err(|_| Code::Internal)?,
        );
        let origin_header: Arc<str> = origin.origin().ascii_serialization().into();
        let expected_authority: Arc<str> = match (origin.host_str(), origin.port()) {
            (Some(host), Some(port)) => format!("{host}:{port}").into(),
            (Some(host), None) => host.into(),
            _ => String::new().into(),
        };
        let mut link = Url::parse("fesen://join").unwrap();
        link.query_pairs_mut()
            .append_pair("server", origin.as_str());
        let join_link: Arc<str> = link.as_str().into();
        Ok(Self {
            inner: Arc::new(Mutex::new(Group {
                db,
                online: HashMap::new(),
                probes: HashMap::new(),
                probe_udp: udp_port != 0,
                view_gen: 0,
            })),
            view_frame: watch::channel((0u64, initial_frame)).0,
            password_slots: Arc::new(tokio::sync::Semaphore::new(3)),
            dummy_hash: Arc::new(store::password_hash(&store::secret())?),
            origin,
            origin_header,
            expected_authority,
            join_link,
            udp_port,
            stun,
        })
    }

    pub fn changed(&self) {
        let (revision, view) = self.inner.lock().unwrap().next_view();
        if let Ok(json) = serde_json::to_string(&Event::View { view }) {
            let frame = Utf8Bytes::from(json);
            self.view_frame.send_if_modified(|slot| {
                if revision > slot.0 {
                    *slot = (revision, frame);
                    true
                } else {
                    false
                }
            });
        }
    }

    pub async fn verify(
        &self,
        member: &str,
        password: String,
        admin: bool,
    ) -> Result<String, Code> {
        let hash = store::login_hash(&self.inner.lock().unwrap().db, member, admin);
        let permit = self
            .password_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Code::Busy)?;
        let test = hash
            .clone()
            .unwrap_or_else(|| self.dummy_hash.as_ref().clone());
        let ok = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            store::verify(&password, &test)
        })
        .await
        .map_err(|_| Code::Internal)?;
        hash.filter(|_| ok).ok_or(Code::Unauthorized)
    }

    pub async fn hash(&self, password: String) -> Result<String, Code> {
        let permit = self
            .password_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Code::Busy)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            store::password_hash(&password)
        })
        .await
        .map_err(|_| Code::Internal)?
    }
}

pub struct Group {
    pub db: Connection,
    pub online: HashMap<String, Device>,
    probes: HashMap<String, String>,
    probe_udp: bool,
    view_gen: u64,
}

pub struct Session {
    pub cancel: CancellationToken,
    pub revoked: AtomicBool,
    pub probe: String,
}

pub struct Device {
    member: String,
    label: String,
    os: Option<DeviceOs>,
    cert: Vec<u8>,
    local: Vec<SocketAddrV4>,
    ipv6: Vec<SocketAddrV6>,
    observed: Option<SocketAddrV4>,
    publications: Vec<Publication>,
    pub session: Arc<Session>,
    outgoing: mpsc::Sender<Utf8Bytes>,
}

impl Device {
    fn candidates(&self) -> Vec<SocketAddrV4> {
        let mut result = self.local.clone();
        if let Some(observed) = self.observed.filter(|a| !result.contains(a)) {
            result.push(observed);
        }
        result
    }
}

impl Group {
    pub fn connect(
        &mut self,
        id: &str,
        member: String,
        label: String,
        os: Option<DeviceOs>,
        cert: Vec<u8>,
        outgoing: mpsc::Sender<Utf8Bytes>,
    ) -> Result<Arc<Session>, Code> {
        if self.online.len() >= MAX_DEVICES && !self.online.contains_key(id) {
            return Err(Code::Limit);
        }
        self.remove(id, false);
        let session = Arc::new(Session {
            cancel: CancellationToken::new(),
            revoked: AtomicBool::new(false),
            probe: if self.probe_udp {
                store::secret()
            } else {
                String::new()
            },
        });
        if self.probe_udp {
            self.probes.insert(session.probe.clone(), id.to_owned());
        }
        self.online.insert(
            id.to_owned(),
            Device {
                member,
                label,
                os,
                cert,
                outgoing,
                session: session.clone(),
                local: vec![],
                ipv6: vec![],
                observed: None,
                publications: vec![],
            },
        );
        Ok(session)
    }

    pub fn remove(&mut self, id: &str, revoked: bool) {
        if let Some(entry) = self.online.remove(id) {
            self.probes.remove(&entry.session.probe);
            if revoked {
                entry.session.revoked.store(true, Ordering::Release);
            }
            entry.session.cancel.cancel();
        }
    }

    pub fn remove_member(&mut self, member: &str) {
        let ids: Vec<_> = self
            .online
            .iter()
            .filter(|(_, device)| device.member == member)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.remove(&id, true);
        }
    }

    pub fn current(&self, id: &str, session: &Arc<Session>) -> bool {
        self.online
            .get(id)
            .is_some_and(|d| Arc::ptr_eq(&d.session, session))
    }

    pub fn observe(&mut self, probe: &str, addr: SocketAddrV4) {
        if !addr.ip().is_unspecified()
            && !addr.ip().is_multicast()
            && addr.port() != 0
            && let Some(device) = self
                .probes
                .get(probe)
                .and_then(|id| self.online.get_mut(id))
        {
            device.observed = Some(addr);
        }
    }

    pub fn next_view(&mut self) -> (u64, View) {
        self.view_gen = self.view_gen.wrapping_add(1);
        (self.view_gen, self.view())
    }

    pub fn services(&self) -> Vec<Offering> {
        let cap: usize = self.online.values().map(|d| d.publications.len()).sum();
        let mut services = Vec::with_capacity(cap);
        for (id, d) in &self.online {
            services.extend(d.publications.iter().map(|s| Offering {
                owner: id.clone(),
                member: d.member.clone(),
                device: Some(d.label.clone()),
                device_os: d.os,
                service: s.clone(),
            }));
        }
        services.sort_unstable_by(|a, b| {
            (&a.member, &a.device, &a.service.name, &a.service.id).cmp(&(
                &b.member,
                &b.device,
                &b.service.name,
                &b.service.id,
            ))
        });
        services
    }

    pub fn view(&self) -> View {
        let mut peers = Vec::with_capacity(self.online.len());
        let cap: usize = self.online.values().map(|d| d.publications.len()).sum();
        let mut services = Vec::with_capacity(cap);
        for (id, d) in &self.online {
            peers.push(Peer {
                id: id.clone(),
                certificate: d.cert.clone(),
            });
            services.extend(d.publications.iter().map(|s| Offering {
                owner: id.clone(),
                member: d.member.clone(),
                device: Some(d.label.clone()),
                device_os: d.os,
                service: s.clone(),
            }));
        }
        peers.sort_unstable_by(|a, b| a.id.cmp(&b.id));
        services.sort_unstable_by(|a, b| {
            (&a.member, &a.device, &a.service.name, &a.service.id).cmp(&(
                &b.member,
                &b.device,
                &b.service.name,
                &b.service.id,
            ))
        });
        View { peers, services }
    }

    pub fn call(&mut self, id: &str, session: &Arc<Session>, call: Call) -> Result<bool, Code> {
        if !self.current(id, session) {
            return Err(Code::Unauthorized);
        }
        match call {
            Call::Publish { service } => {
                let service = service.checked()?;
                let mut pool = Vec::new();
                let mut publications = 0;
                for (other_id, other) in &self.online {
                    for existing in &other.publications {
                        if existing.id == service.id {
                            if other_id == id && existing == &service {
                                return Ok(false);
                            }
                            return Err(Code::Conflict);
                        }
                        if other_id == id && existing.rules.conflicts(&service.rules) {
                            return Err(Code::Conflict);
                        }
                        publications += 1;
                        pool.extend(
                            existing
                                .rules
                                .ips
                                .iter()
                                .map(|r| (u32::from(r.start), u32::from(r.end))),
                        );
                    }
                }
                pool.extend(
                    service
                        .rules
                        .ips
                        .iter()
                        .map(|r| (u32::from(r.start), u32::from(r.end))),
                );
                let mine = &mut self.online.get_mut(id).unwrap().publications;
                if mine.len() >= 32
                    || publications >= MAX_PUBLICATIONS
                    || count(&union(pool)) > MAX_ADDRESSES
                {
                    return Err(Code::Limit);
                }
                mine.push(service);
                Ok(true)
            }
            Call::Unpublish { id: publication } => {
                let mine = &mut self.online.get_mut(id).unwrap().publications;
                let before = mine.len();
                mine.retain(|p| p.id != publication);
                Ok(mine.len() != before)
            }
            Call::Candidates {
                mut local,
                extra,
                mut ipv6,
            } => {
                if local.len() + extra.len() > MAX_CANDIDATES
                    || ipv6.len() > MAX_IPV6_CANDIDATES
                    || ipv6.iter().any(|&a| !candidate6(a))
                    || local.iter().any(|a| {
                        let ip = a.ip();
                        !(ip.is_private() || ip.is_link_local()) || a.port() == 0
                    })
                    || extra.iter().any(|&a| !candidate(a))
                {
                    return Err(Code::Invalid);
                }
                local.extend(extra);
                local.sort_unstable();
                local.dedup();
                ipv6.sort_unstable();
                ipv6.dedup();
                let device = self.online.get_mut(id).unwrap();
                device.local = local;
                device.ipv6 = ipv6;
                Ok(false)
            }
            Call::Punch { peer } => {
                if peer == id {
                    return Err(Code::Invalid);
                }
                let b = self.online.get(&peer).ok_or(Code::Offline)?;
                if b.publications.is_empty() {
                    return Err(Code::Unauthorized);
                }
                let a = &self.online[id];
                let to_b = serde_json::to_string(&Event::Punch {
                    peer: id.to_owned(),
                    candidates: a.candidates(),
                    ipv6: a.ipv6.clone(),
                })
                .map_err(|_| Code::Internal)?;
                let to_a = serde_json::to_string(&Event::Punch {
                    peer,
                    candidates: b.candidates(),
                    ipv6: b.ipv6.clone(),
                })
                .map_err(|_| Code::Internal)?;
                b.outgoing
                    .try_send(Utf8Bytes::from(to_b))
                    .map_err(|_| Code::Busy)?;
                a.outgoing
                    .try_send(Utf8Bytes::from(to_a))
                    .map_err(|_| Code::Busy)?;
                Ok(false)
            }
            Call::Ping => Ok(false),
            Call::Logout => {
                self.db
                    .execute("UPDATE devices SET token=NULL WHERE id=?1", [id])
                    .map_err(|_| Code::Internal)?;
                self.remove(id, false);
                Ok(true)
            }
            Call::RenameDevice { label } => {
                let label = store::rename_device(&self.db, id, &label)?;
                let device = self.online.get_mut(id).unwrap();
                if device.label == label {
                    return Ok(false);
                }
                device.label = label;
                Ok(true)
            }
            Call::SavePref { pref } => {
                let member = self.online[id].member.clone();
                store::save_pref(&self.db, &member, &pref)?;
                self.sync_member(&member);
                Ok(false)
            }
            Call::SaveProfile { profile } => {
                let member = self.online[id].member.clone();
                store::save_profile(&self.db, &member, &profile)?;
                self.sync_member(&member);
                Ok(false)
            }
            Call::DeleteProfile { machine_id } => {
                let member = self.online[id].member.clone();
                store::delete_profile(&self.db, &member, &machine_id)?;
                self.sync_member(&member);
                Ok(false)
            }
        }
    }

    pub fn sync_member(&self, member: &str) {
        let Ok(prefs) = store::list_prefs(&self.db, member) else {
            return;
        };
        let Ok(profiles) = store::list_profiles(&self.db, member) else {
            return;
        };
        let Ok(json) = serde_json::to_string(&Event::Sync { prefs, profiles }) else {
            return;
        };
        let frame = Utf8Bytes::from(json);
        for d in self.online.values().filter(|d| d.member == member) {
            let _ = d.outgoing.try_send(frame.clone());
        }
    }
}
