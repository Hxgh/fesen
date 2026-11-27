use crate::platform::{Network, Tun};
use fesen_acl::{Cidr, Range, VIRTUAL, difference, union};
pub use fesen_proto::alias_name;
use fesen_proto::{Code, Offering};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    net::{Ipv4Addr, SocketAddrV4},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Label {
    pub service: String,
    pub remark: String,
    pub alias: Option<String>,
    pub ip: Option<Ipv4Addr>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Mapping {
    pub original: Range<Ipv4Addr>,
    pub virtual_ip: Range<Ipv4Addr>,
}

pub fn access_ip(mappings: &[Mapping], original: Ipv4Addr) -> Ipv4Addr {
    let i = mappings.partition_point(|m| m.original.end < original);
    mappings
        .get(i)
        .filter(|m| m.original.contains(original))
        .map_or(original, |m| {
            (u32::from(m.virtual_ip.start) + (u32::from(original) - u32::from(m.original.start)))
                .into()
        })
}

#[derive(Default)]
pub struct Routes {
    pub tun: Tun,
    pub(super) installed: bool,
    pub mappings: Vec<Mapping>,
    pub service_mappings: HashMap<String, Vec<Mapping>>,
    pub aliases: HashMap<String, Ipv4Addr>,
    protected: Vec<(u32, u32)>,
    to_original: HashMap<Ipv4Addr, Ipv4Addr>,
    to_service_original: HashMap<Ipv4Addr, (String, Ipv4Addr)>,
    primary_indices: Vec<usize>,
    primary_keys: HashSet<(String, String)>,
    remapped_keys: HashSet<(String, String)>,
    remapped_by_owner: HashMap<String, Vec<usize>>,
    reserved: BTreeMap<u32, u32>,
    reserved_peers: HashMap<String, BTreeMap<u32, u32>>,
    retired: HashSet<u32>,
    directory: Vec<Offering>,
}

fn intervals_contain(intervals: &[(u32, u32)], n: u32) -> bool {
    let i = intervals.partition_point(|&(_, b)| b < n);
    intervals.get(i).is_some_and(|&(a, _)| a <= n)
}

pub enum Destination {
    Peer {
        owner: String,
        service: String,
        target: SocketAddrV4,
    },
    Local,
    Reject,
}

impl Routes {
    pub(super) fn ready_for(&self, view: &[Offering]) -> bool {
        self.installed && self.directory == view
    }

    pub fn cleared(&self) -> Self {
        Self {
            reserved: self.reserved.clone(),
            reserved_peers: self.reserved_peers.clone(),
            retired: self.retired.clone(),
            ..Self::default()
        }
    }

    pub fn build(
        previous: &Self,
        network: &Network,
        directory: Vec<Offering>,
        labels: &[Label],
        control: Option<Ipv4Addr>,
        keep_empty_tun: bool,
        local_shares: &[Range<Ipv4Addr>],
    ) -> Result<Self, Code> {
        if directory.is_empty() && !keep_empty_tun {
            return Ok(previous.cleared());
        }
        let extract = |r: Range<Ipv4Addr>| (u32::from(r.start), u32::from(r.end));
        let protected = union(
            network
                .routes
                .iter()
                .filter(|r| r.cidr.prefix > 0)
                .map(|r| extract(r.cidr.range()))
                .chain(local_shares.iter().copied().map(extract))
                .chain(
                    network
                        .addresses
                        .iter()
                        .chain(network.dns.iter())
                        .copied()
                        .chain(control)
                        .map(|ip| (ip.into(), ip.into())),
                ),
        );
        let mut primary_flags = vec![false; directory.len()];
        let mut remapped_flags = vec![false; directory.len()];
        let mut primary: Vec<&Offering> = Vec::with_capacity(directory.len());
        let has_conflict = |primary: &[&Offering], o: &Offering| {
            primary
                .iter()
                .any(|p| p.owner != o.owner && p.service.rules.conflicts(&o.service.rules))
        };
        // Keep currently connected primaries first so new arrivals never hijack an active peer's route.
        for (idx, o) in directory.iter().enumerate() {
            let key = (o.owner.clone(), o.service.id.clone());
            if previous.primary_keys.contains(&key) && !has_conflict(&primary, o) {
                primary.push(o);
                primary_flags[idx] = true;
            }
        }
        // Keep continuously online remapped peers pinned to their virtual IP across primary blips.
        for (idx, o) in directory.iter().enumerate() {
            if primary_flags[idx] {
                continue;
            }
            let key = (o.owner.clone(), o.service.id.clone());
            if previous.remapped_keys.contains(&key) {
                remapped_flags[idx] = true;
            }
        }
        for (idx, o) in directory.iter().enumerate() {
            if primary_flags[idx] || remapped_flags[idx] {
                continue;
            }
            if has_conflict(&primary, o) {
                remapped_flags[idx] = true;
            } else {
                primary.push(o);
                primary_flags[idx] = true;
            }
        }
        // Keep all ports of a remapped peer host unified on that peer's virtual IP.
        loop {
            let mut remapped_owner_ips: HashMap<&str, Vec<(u32, u32)>> = HashMap::new();
            for (idx, o) in directory.iter().enumerate() {
                if remapped_flags[idx] {
                    remapped_owner_ips
                        .entry(o.owner.as_str())
                        .or_default()
                        .extend(o.service.rules.ips.iter().copied().map(extract));
                }
            }
            if remapped_owner_ips.is_empty() {
                break;
            }
            for spans in remapped_owner_ips.values_mut() {
                *spans = union(spans.iter().copied());
            }
            let mut moved = false;
            for (idx, o) in directory.iter().enumerate() {
                if !primary_flags[idx] {
                    continue;
                }
                if let Some(spans) = remapped_owner_ips.get(o.owner.as_str())
                    && o.service.rules.ips.iter().any(|r| {
                        let (a, b) = extract(*r);
                        let pos = spans.partition_point(|&(_, end)| end < a);
                        spans.get(pos).is_some_and(|&(sa, _)| sa <= b)
                    })
                {
                    primary_flags[idx] = false;
                    remapped_flags[idx] = true;
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
        let mut primary: Vec<&Offering> = Vec::with_capacity(directory.len());
        let mut primary_indices: Vec<usize> = Vec::with_capacity(directory.len());
        let mut primary_keys = HashSet::new();
        let mut remapped_keys = HashSet::new();
        let mut remapped_services = HashSet::new();
        let mut remapped_by_owner: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, o) in directory.iter().enumerate() {
            let key = (o.owner.clone(), o.service.id.clone());
            if remapped_flags[idx] {
                remapped_keys.insert(key);
                remapped_services.insert(o.service.id.clone());
                remapped_by_owner
                    .entry(o.owner.clone())
                    .or_default()
                    .push(idx);
            } else {
                primary.push(o);
                primary_indices.push(idx);
                primary_keys.insert(key);
            }
        }
        let targets = union(
            primary
                .iter()
                .flat_map(|s| s.service.rules.ips.iter().copied().map(extract)),
        );
        let visible = difference(&targets, &protected);
        let overlap = difference(&targets, &visible);
        let available = difference(&[extract(VIRTUAL)], &protected);
        // Do not repurpose virtual destinations as the TUN or resolver address.
        let mut used = previous.retired.clone();
        used.extend(previous.reserved.values().copied());
        used.extend(
            previous
                .reserved_peers
                .values()
                .flat_map(|m| m.values().copied()),
        );
        let mut free = available.iter().flat_map(|&(a, b)| a..=b);
        let address = free.find(|n| used.insert(*n)).ok_or(Code::Conflict)?;
        let dns_slot = free.find(|n| used.insert(*n)).ok_or(Code::Conflict)?;
        let active_labels: Vec<_> = labels
            .iter()
            .filter(|l| {
                l.alias.is_some()
                    && l.ip.is_some_and(|ip| {
                        directory.iter().any(|s| {
                            s.service.id == l.service
                                && s.service.rules.ips.iter().any(|r| r.contains(ip))
                        })
                    })
            })
            .collect();
        let dns = (!active_labels.is_empty()).then_some(dns_slot);
        let mut to_virtual = BTreeMap::new();
        let mut reserved = previous.reserved.clone();
        let mut reserved_peers = previous.reserved_peers.clone();
        let mut retired = previous.retired.clone();
        for original in overlap.iter().flat_map(|&(a, b)| a..=b) {
            if let Some(&mapped) = previous.reserved.get(&original)
                && intervals_contain(&available, mapped)
            {
                to_virtual.insert(original, mapped);
            }
        }
        for original in overlap.iter().flat_map(|&(a, b)| a..=b) {
            if let std::collections::btree_map::Entry::Vacant(slot) = to_virtual.entry(original) {
                let mapped = free.find(|n| used.insert(*n)).ok_or(Code::Conflict)?;
                if let Some(old) = reserved.insert(original, mapped) {
                    retired.insert(old);
                }
                slot.insert(mapped);
            }
        }
        let to_original = to_virtual
            .iter()
            .map(|(&original, &virt)| (virt.into(), original.into()))
            .collect();
        let mut peer_virtual: HashMap<&str, BTreeMap<u32, u32>> = HashMap::new();
        for o in &directory {
            if !remapped_services.contains(&o.service.id) {
                continue;
            }
            let prev_owner = previous.reserved_peers.get(&o.owner);
            let owner_virt = peer_virtual.entry(o.owner.as_str()).or_default();
            let owner_reserved = reserved_peers.entry(o.owner.clone()).or_default();
            for r in &o.service.rules.ips {
                for original in u32::from(r.start)..=u32::from(r.end) {
                    if owner_virt.contains_key(&original) {
                        continue;
                    }
                    if let Some(&mapped) = prev_owner.and_then(|m| m.get(&original))
                        && intervals_contain(&available, mapped)
                    {
                        owner_virt.insert(original, mapped);
                    } else {
                        let mapped = free.find(|n| used.insert(*n)).ok_or(Code::Conflict)?;
                        if let Some(old) = owner_reserved.insert(original, mapped) {
                            retired.insert(old);
                        }
                        owner_virt.insert(original, mapped);
                    }
                }
            }
        }
        let mut to_service_original = HashMap::new();
        for (&owner, map) in &peer_virtual {
            for (&original, &virt) in map {
                to_service_original
                    .insert(Ipv4Addr::from(virt), (owner.to_owned(), Ipv4Addr::from(original)));
            }
        }
        let collapse = |pairs: Vec<(u32, u32)>| -> Vec<Mapping> {
            let mut out: Vec<Mapping> = Vec::new();
            for (original, virt) in pairs {
                if let Some(last) = out.last_mut()
                    && u32::from(last.original.end).checked_add(1) == Some(original)
                    && u32::from(last.virtual_ip.end).checked_add(1) == Some(virt)
                {
                    last.original.end = original.into();
                    last.virtual_ip.end = virt.into();
                } else {
                    out.push(Mapping {
                        original: Range {
                            start: original.into(),
                            end: original.into(),
                        },
                        virtual_ip: Range {
                            start: virt.into(),
                            end: virt.into(),
                        },
                    });
                }
            }
            out
        };
        let mappings = collapse(to_virtual.iter().map(|(&o, &v)| (o, v)).collect());
        let mut service_mappings = HashMap::new();
        for o in &directory {
            if !remapped_services.contains(&o.service.id) {
                continue;
            }
            let Some(owner_virt) = peer_virtual.get(o.owner.as_str()) else {
                continue;
            };
            let mut pairs = Vec::new();
            for r in &o.service.rules.ips {
                for original in u32::from(r.start)..=u32::from(r.end) {
                    if let Some(&virt) = owner_virt.get(&original) {
                        pairs.push((original, virt));
                    }
                }
            }
            pairs.sort_unstable_by_key(|&(orig, _)| orig);
            pairs.dedup_by_key(|p| p.0);
            if !pairs.is_empty() {
                service_mappings.insert(o.service.id.clone(), collapse(pairs));
            }
        }
        let routes: Vec<_> = union(
            visible
                .into_iter()
                .chain(mappings.iter().map(|m| extract(m.virtual_ip)))
                .chain(
                    to_service_original
                        .keys()
                        .map(|ip| (u32::from(*ip), u32::from(*ip))),
                )
                .chain(dns.map(|n| (n, n))),
        )
        .into_iter()
        .flat_map(Cidr::cover)
        .collect();
        if routes.len() > 2048 {
            return Err(Code::Limit);
        }
        let aliases = active_labels
            .into_iter()
            .map(|l| {
                let ip = l.ip.expect("checked label");
                let resolved = service_mappings
                    .get(&l.service)
                    .map(|m| access_ip(m, ip))
                    .filter(|&v| v != ip)
                    .unwrap_or_else(|| {
                        to_virtual
                            .get(&u32::from(ip))
                            .copied()
                            .map_or(ip, Into::into)
                    });
                (l.alias.as_ref().unwrap().clone(), resolved)
            })
            .collect();
        Ok(Self {
            installed: false,
            tun: Tun {
                address: Some(address.into()),
                dns: dns.map(Into::into),
                routes,
            },
            mappings,
            service_mappings,
            aliases,
            protected,
            to_original,
            to_service_original,
            primary_indices,
            primary_keys,
            remapped_keys,
            remapped_by_owner,
            directory,
            reserved,
            reserved_peers,
            retired,
        })
    }

    pub fn access_ip(&self, original: Ipv4Addr) -> Ipv4Addr {
        access_ip(&self.mappings, original)
    }

    pub fn access_ip_for(&self, service: &str, original: Ipv4Addr) -> Ipv4Addr {
        if let Some(m) = self.service_mappings.get(service) {
            let v = access_ip(m, original);
            if v != original {
                return v;
            }
        }
        self.access_ip(original)
    }

    pub fn destination(&self, addr: SocketAddrV4) -> Destination {
        let ip = *addr.ip();
        if let Some((owner, original)) = self.to_service_original.get(&ip) {
            if let Some(indices) = self.remapped_by_owner.get(owner)
                && let Some(o) = indices
                    .iter()
                    .map(|&idx| &self.directory[idx])
                    .find(|o| o.service.rules.allows(*original, addr.port()))
            {
                return Destination::Peer {
                    owner: o.owner.clone(),
                    service: o.service.id.clone(),
                    target: SocketAddrV4::new(*original, addr.port()),
                };
            }
            return Destination::Reject;
        }
        let target = if let Some(&original) = self.to_original.get(&ip) {
            original
        } else if VIRTUAL.contains(ip) {
            return Destination::Reject;
        } else {
            if intervals_contain(&self.protected, u32::from(ip)) {
                return Destination::Local;
            }
            ip
        };
        if let Some(o) = self
            .primary_indices
            .iter()
            .map(|&idx| &self.directory[idx])
            .find(|o| o.service.rules.allows(target, addr.port()))
        {
            return Destination::Peer {
                owner: o.owner.clone(),
                service: o.service.id.clone(),
                target: SocketAddrV4::new(target, addr.port()),
            };
        }
        if ip == target {
            Destination::Local
        } else {
            Destination::Reject
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::Underlay;
    use fesen_acl::Rules;
    use fesen_proto::Publication;

    #[test]
    fn connected_mobile_has_an_empty_split_tunnel_until_a_share_exists() {
        let network = Network::default();
        let active =
            Routes::build(&Routes::default(), &network, vec![], &[], None, true, &[]).unwrap();
        assert!(active.tun.address.is_some());
        assert!(active.tun.dns.is_none() && active.tun.routes.is_empty());
        let off = Routes::build(&active, &network, vec![], &[], None, false, &[]).unwrap();
        assert_eq!(off.tun, Tun::default());
    }

    #[test]
    fn overlap_keeps_every_local_port_and_alias_maps_to_remote() {
        let offer = |id: &str, ips, ports| Offering {
            owner: id.into(),
            member: id.into(),
            device: None,
            device_os: None,
            service: Publication {
                id: id.into(),
                name: id.into(),
                rules: Rules::parse(ips, ports).unwrap(),
                http: false,
                kind: None,
                host_os: None,
            },
        };
        let mut network = Network {
            routes: vec![
                Underlay {
                    cidr: "10.1.0.0/24".parse().unwrap(),
                    index: 1,
                    metric: 0,
                },
                Underlay {
                    cidr: "0.0.0.0/0".parse().unwrap(),
                    index: 1,
                    metric: 0,
                },
            ],
            ..Network::default()
        };
        let offers = vec![
            offer("a", "10.1.0.9,10.2.0.5", "80"),
            offer("b", "10.2.0.5", "81"),
        ];
        let labels = [Label {
            service: "a".into(),
            remark: "备注".into(),
            alias: Some("app.fesen".into()),
            ip: Some("10.1.0.9".parse().unwrap()),
        }];
        let mut routes = Routes::build(
            &Routes::default(),
            &network,
            offers.clone(),
            &labels,
            None,
            false,
            &[],
        )
        .unwrap();
        assert!(!routes.ready_for(&offers));
        routes.installed = true;
        assert!(routes.ready_for(&offers));
        assert!(!routes.ready_for(&offers[..1]));
        assert!(!routes.cleared().ready_for(&offers));
        assert!(matches!(
            routes.destination("10.1.0.9:80".parse().unwrap()),
            Destination::Local
        ));
        assert!(
            matches!(routes.destination("10.2.0.5:81".parse().unwrap()), Destination::Peer { owner, .. } if owner == "b")
        );
        assert!(matches!(
            routes.destination("10.2.0.5:82".parse().unwrap()),
            Destination::Local
        ));
        let alias = routes.aliases["app.fesen"];
        assert!(
            matches!(routes.destination(SocketAddrV4::new(alias,80)), Destination::Peer { target, .. } if target.to_string() == "10.1.0.9:80")
        );
        assert!(matches!(
            routes.destination(SocketAddrV4::new(alias, 81)),
            Destination::Reject
        ));
        assert!(
            routes
                .tun
                .routes
                .iter()
                .all(|r| r.prefix != 0 && !r.range().contains("10.1.0.9".parse().unwrap()))
        );
        network.routes.push(Underlay {
            cidr: "198.18.0.0/31".parse().unwrap(),
            index: 1,
            metric: 0,
        });
        let rebuilt = Routes::build(&routes, &network, offers, &labels, None, false, &[]).unwrap();
        assert_ne!(rebuilt.tun.address, routes.tun.address);
        assert_ne!(rebuilt.tun.dns, routes.tun.dns);
        assert_ne!(rebuilt.tun.address, Some(alias));
        assert_eq!(rebuilt.aliases["app.fesen"], alias);
    }

    #[test]
    fn cross_device_same_ip_and_port_assigns_distinct_virtual_routes() {
        let offer = |owner: &str, id: &str, ips, ports| Offering {
            owner: owner.into(),
            member: owner.into(),
            device: Some(owner.into()),
            device_os: None,
            service: Publication {
                id: id.into(),
                name: id.into(),
                rules: Rules::parse(ips, ports).unwrap(),
                http: false,
                kind: None,
                host_os: None,
            },
        };
        let network = Network::default();
        let sa = offer("dev-a", "0000000000000000000000000000000a", "192.0.2.10", "80");
        let sb_http = offer("dev-b", "0000000000000000000000000000000b", "192.0.2.10", "80");
        let sb_tls = offer("dev-b", "0000000000000000000000000000000c", "192.0.2.10", "443");
        let routes = Routes::build(
            &Routes::default(),
            &network,
            vec![sa.clone(), sb_http.clone(), sb_tls.clone()],
            &[],
            None,
            false,
            &[],
        )
        .unwrap();
        let orig: Ipv4Addr = "192.0.2.10".parse().unwrap();
        let ip_a = routes.access_ip_for(&sa.service.id, orig);
        let ip_b_http = routes.access_ip_for(&sb_http.service.id, orig);
        let ip_b_tls = routes.access_ip_for(&sb_tls.service.id, orig);
        assert_eq!(ip_a, orig);
        assert_ne!(ip_b_http, orig);
        assert!(VIRTUAL.contains(ip_b_http));
        // All ports on the same remapped peer host stay unified on one virtual IP.
        assert_eq!(ip_b_http, ip_b_tls);
        assert!(matches!(
            routes.destination(SocketAddrV4::new(ip_a, 80)),
            Destination::Peer { owner, .. } if owner == "dev-a"
        ));
        assert!(matches!(
            routes.destination(SocketAddrV4::new(ip_b_http, 80)),
            Destination::Peer { owner, .. } if owner == "dev-b"
        ));
        assert!(matches!(
            routes.destination(SocketAddrV4::new(ip_b_tls, 443)),
            Destination::Peer { owner, .. } if owner == "dev-b"
        ));

        // If dev-a drops transiently while dev-b stays connected, dev-b retains its virtual IP
        // and does not silently hijack dev-a's original IP.
        let routes_b_only = Routes::build(
            &routes,
            &network,
            vec![sb_http.clone(), sb_tls.clone()],
            &[],
            None,
            false,
            &[],
        )
        .unwrap();
        assert_eq!(routes_b_only.access_ip_for(&sb_http.service.id, orig), ip_b_http);
        assert_eq!(routes_b_only.access_ip_for(&sb_tls.service.id, orig), ip_b_tls);

        // Active local share protects that IP even when not present in a specific interface route.
        let local_span = Range {
            start: orig,
            end: orig,
        };
        let routes_with_local = Routes::build(
            &Routes::default(),
            &network,
            vec![sa.clone()],
            &[],
            None,
            false,
            &[local_span],
        )
        .unwrap();
        let mapped_a = routes_with_local.access_ip_for(&sa.service.id, orig);
        assert_ne!(mapped_a, orig);
        assert!(VIRTUAL.contains(mapped_a));
        assert!(matches!(
            routes_with_local.destination(SocketAddrV4::new(orig, 80)),
            Destination::Local
        ));
    }

    #[test]
    fn existing_primary_owner_keeps_original_ip_when_alphabetically_earlier_peer_joins() {
        let offer = |owner: &str, id: &str, ips, ports| Offering {
            owner: owner.into(),
            member: owner.into(),
            device: Some(owner.into()),
            device_os: None,
            service: Publication {
                id: id.into(),
                name: id.into(),
                rules: Rules::parse(ips, ports).unwrap(),
                http: false,
                kind: None,
                host_os: None,
            },
        };
        let network = Network::default();
        let bob = offer("bob", "00000000000000000000000000000002", "198.51.100.55", "80");
        let alice = offer("alice", "00000000000000000000000000000001", "198.51.100.55", "80");
        let orig: Ipv4Addr = "198.51.100.55".parse().unwrap();

        let r1 = Routes::build(&Routes::default(), &network, vec![bob.clone()], &[], None, false, &[]).unwrap();
        assert_eq!(r1.access_ip_for(&bob.service.id, orig), orig);

        // Server sorts alice ahead of bob; bob must retain 198.51.100.55 and alice maps virtually.
        let r2 = Routes::build(&r1, &network, vec![alice.clone(), bob.clone()], &[], None, false, &[]).unwrap();
        assert_eq!(r2.access_ip_for(&bob.service.id, orig), orig);
        assert_ne!(r2.access_ip_for(&alice.service.id, orig), orig);
    }
}
