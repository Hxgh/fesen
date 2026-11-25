use crate::platform::{Network, Tun};
use fesen_acl::{Cidr, Range, VIRTUAL, difference, union};
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

pub fn alias_name(label: &str) -> Result<String, Code> {
    let bare = label.trim().trim_end_matches('.').to_ascii_lowercase();
    let bare = bare.strip_suffix(".fesen").unwrap_or(&bare);
    if !(1..=63).contains(&bare.len())
        || bare.starts_with('-')
        || bare.ends_with('-')
        || !bare.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
    {
        return Err(Code::Invalid);
    }
    Ok(format!("{bare}.fesen"))
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
    pub aliases: HashMap<String, Ipv4Addr>,
    protected: Vec<(u32, u32)>,
    to_original: HashMap<Ipv4Addr, Ipv4Addr>,
    reserved: BTreeMap<u32, u32>,
    retired: HashSet<u32>,
    directory: Vec<Offering>,
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
        let targets = union(
            directory
                .iter()
                .flat_map(|s| s.service.rules.ips.iter().copied().map(extract)),
        );
        let visible = difference(&targets, &protected);
        let overlap = difference(&targets, &visible);
        let available = difference(&[extract(VIRTUAL)], &protected);
        // Do not repurpose virtual destinations as the TUN or resolver address.
        let mut used = previous.retired.clone();
        used.extend(previous.reserved.values().copied());
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
        let mut retired = previous.retired.clone();
        let mut reuse = |original: u32| {
            if let Some(&mapped) = previous.reserved.get(&original)
                && available.iter().any(|&(a, b)| a <= mapped && mapped <= b)
            {
                to_virtual.insert(original, mapped);
            }
        };
        for original in overlap.iter().flat_map(|&(a, b)| a..=b) {
            reuse(original);
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
        let mut mappings: Vec<Mapping> = Vec::new();
        for (&original, &virt) in &to_virtual {
            if let Some(last) = mappings.last_mut()
                && u32::from(last.original.end).checked_add(1) == Some(original)
                && u32::from(last.virtual_ip.end).checked_add(1) == Some(virt)
            {
                last.original.end = original.into();
                last.virtual_ip.end = virt.into();
            } else {
                mappings.push(Mapping {
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
        let routes: Vec<_> = union(
            visible
                .into_iter()
                .chain(mappings.iter().map(|m| extract(m.virtual_ip)))
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
                let resolved = to_virtual
                    .get(&u32::from(ip))
                    .copied()
                    .map_or(ip, Into::into);
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
            aliases,
            protected,
            to_original,
            directory,
            reserved,
            retired,
        })
    }

    pub fn access_ip(&self, original: Ipv4Addr) -> Ipv4Addr {
        access_ip(&self.mappings, original)
    }

    pub fn destination(&self, addr: SocketAddrV4) -> Destination {
        let ip = *addr.ip();
        let target = if let Some(&original) = self.to_original.get(&ip) {
            original
        } else if VIRTUAL.contains(ip) {
            return Destination::Reject;
        } else {
            let n = u32::from(ip);
            if self.protected.iter().any(|&(a, b)| (a..=b).contains(&n)) {
                return Destination::Local;
            }
            ip
        };
        if let Some(o) = self
            .directory
            .iter()
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
        let active = Routes::build(&Routes::default(), &network, vec![], &[], None, true).unwrap();
        assert!(active.tun.address.is_some());
        assert!(active.tun.dns.is_none() && active.tun.routes.is_empty());
        let off = Routes::build(&active, &network, vec![], &[], None, false).unwrap();
        assert_eq!(off.tun, Tun::default());
    }

    #[test]
    fn overlap_keeps_every_local_port_and_alias_maps_to_remote() {
        let offer = |id: &str, ips, ports| Offering {
            owner: id.into(),
            member: id.into(),
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
        let rebuilt = Routes::build(&routes, &network, offers, &labels, None, false).unwrap();
        assert_ne!(rebuilt.tun.address, routes.tun.address);
        assert_ne!(rebuilt.tun.dns, routes.tun.dns);
        assert_ne!(rebuilt.tun.address, Some(alias));
        assert_eq!(rebuilt.aliases["app.fesen"], alias);
    }
}
