use crate::store;
use fesen_acl::{MAX_ADDRESSES, count, union};
use fesen_proto::{
    Call, Code, Event, MAX_CANDIDATES, MAX_DEVICES, MAX_IPV6_CANDIDATES, MAX_PUBLICATIONS,
    Offering, Peer, Publication, View, candidate, candidate6,
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
    pub changed: watch::Sender<u64>,
    pub password_slots: Arc<tokio::sync::Semaphore>,
    pub dummy_hash: Arc<String>,
    pub origin: Url,
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
        Ok(Self {
            inner: Arc::new(Mutex::new(Group {
                db,
                online: HashMap::new(),
                probes: HashMap::new(),
                probe_udp: udp_port != 0,
            })),
            changed: watch::channel(0).0,
            password_slots: Arc::new(tokio::sync::Semaphore::new(3)),
            dummy_hash: Arc::new(store::password_hash(&store::secret())?),
            origin,
            udp_port,
            stun,
        })
    }

    pub fn changed(&self) {
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
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
}

pub struct Session {
    pub cancel: CancellationToken,
    pub revoked: AtomicBool,
    pub probe: String,
}

pub struct Device {
    member: String,
    label: String,
    cert: Vec<u8>,
    local: Vec<SocketAddrV4>,
    ipv6: Vec<SocketAddrV6>,
    observed: Option<SocketAddrV4>,
    publications: Vec<Publication>,
    pub session: Arc<Session>,
    outgoing: mpsc::Sender<Event>,
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
        cert: Vec<u8>,
        outgoing: mpsc::Sender<Event>,
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

    pub fn view(&self) -> View {
        let mut peers = Vec::with_capacity(self.online.len());
        let mut services = Vec::new();
        for (id, d) in &self.online {
            peers.push(Peer {
                id: id.clone(),
                certificate: d.cert.clone(),
            });
            services.extend(d.publications.iter().map(|s| Offering {
                owner: id.clone(),
                member: d.member.clone(),
                device: Some(d.label.clone()),
                service: s.clone(),
            }));
        }
        peers.sort_unstable_by(|a, b| a.id.cmp(&b.id));
        services.sort_unstable_by(|a, b| a.service.id.cmp(&b.service.id));
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
                        if existing.id == service.id && other_id == id {
                            if existing == &service {
                                return Ok(false);
                            }
                            return Err(Code::Conflict);
                        }
                        if existing.id == service.id || existing.rules.conflicts(&service.rules) {
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
                b.outgoing
                    .try_send(Event::Punch {
                        peer: id.to_owned(),
                        candidates: a.candidates(),
                        ipv6: a.ipv6.clone(),
                    })
                    .map_err(|_| Code::Busy)?;
                a.outgoing
                    .try_send(Event::Punch {
                        peer,
                        candidates: b.candidates(),
                        ipv6: b.ipv6.clone(),
                    })
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
        }
    }
}
