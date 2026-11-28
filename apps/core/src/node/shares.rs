use super::{
    Core,
    types::{HostUpdate, Sharing},
};
use crate::{control::Link, probe, routing::Label};
use fesen_acl::Rules;
use fesen_proto::{
    Call, Code, DeviceProfile, HostOs, MemberPref, Publication, ServiceKind, View, valid_id,
    valid_label,
};
use std::{collections::HashSet, net::Ipv4Addr, sync::Arc};

impl Core {
    pub(super) async fn share(&self, id: String, enabled: bool) -> Result<(), Code> {
        if !enabled {
            let link = {
                let mut state = self.state.lock().unwrap();
                let Some(share) = state.sharing.remove(&id) else {
                    return Ok(());
                };
                share.cancel.cancel();
                state.link.clone()
            };
            self.bump_status();
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
        if !state.network.same_underlay(&net)
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
        drop(state);
        self.bump_status();
        Ok(())
    }

    pub(super) async fn share_many(&self, ids: Vec<String>, enabled: bool) -> Result<(), Code> {
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
        if unique.len() == 1 {
            return self.share(unique.pop().unwrap(), enabled).await;
        }
        if !enabled {
            {
                let state = self.state.lock().unwrap();
                if unique
                    .iter()
                    .any(|id| !state.local.shares().iter().any(|s| &s.id == id))
                {
                    return Err(Code::Invalid);
                }
            }
            return self.unpublish_many(&unique).await;
        }
        if self.refresh_network().await {
            return Err(Code::NetworkChanged);
        }
        let (link, net, to_publish) = {
            let state = self.state.lock().unwrap();
            let shares = state.local.shares();
            if unique.iter().any(|id| !shares.iter().any(|s| &s.id == id)) {
                return Err(Code::Invalid);
            }
            let pending: Vec<Publication> = unique
                .iter()
                .filter(|id| !state.sharing.contains_key(*id))
                .filter_map(|id| shares.iter().find(|s| &s.id == id).cloned())
                .collect();
            if pending.is_empty() {
                return Ok(());
            }
            if !state.network.known() {
                return Err(Code::NetworkChanged);
            }
            let link = state.link.clone().ok_or(Code::NotConnected)?;
            if !link.direct.membership.is_fresh() {
                return Err(Code::NotConnected);
            }
            (link, state.network.clone(), pending)
        };
        let results = futures_util::future::join_all(to_publish.into_iter().map(|service| {
            let link = link.clone();
            async move {
                let res = link
                    .call(Call::Publish {
                        service: service.clone(),
                    })
                    .await;
                (service, res)
            }
        }))
        .await;
        let mut first_error = None;
        let mut disconnected = false;
        let mut published = Vec::new();
        for (service, res) in results {
            match res {
                Ok(()) => published.push(service),
                Err(e) => {
                    if e == Code::NotConnected {
                        disconnected = true;
                    }
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                }
            }
        }
        if disconnected {
            self.disconnect(true);
            return Err(first_error.unwrap_or(Code::NotConnected));
        }
        if !published.is_empty() {
            let mut state = self.state.lock().unwrap();
            if !state.network.same_underlay(&net)
                || !Self::is_link(&state, &link)
                || link.direct.cancel.is_cancelled()
            {
                drop(state);
                link.direct.close();
                return Err(Code::NetworkChanged);
            }
            let mut next = state.local.clone();
            for service in published {
                next.record_share_active(&service.id, true, &net.marker);
                state.sharing.insert(
                    service.id.clone(),
                    Sharing {
                        publication: service,
                        cancel: link.direct.cancel.child_token(),
                    },
                );
            }
            let _ = next.save(&self.dir);
            state.local = next;
            state.auto_share_blocked = false;
            if first_error.is_none() {
                state.error = None;
            }
            drop(state);
            self.bump_status();
        }
        first_error.map_or(Ok(()), Err)
    }

    pub(super) async fn unpublish_many(&self, ids: &[String]) -> Result<(), Code> {
        if ids.is_empty() {
            return Ok(());
        }
        let (link, active_ids) = {
            let mut state = self.state.lock().unwrap();
            let mut removed = Vec::new();
            for id in ids {
                if let Some(share) = state.sharing.remove(id) {
                    share.cancel.cancel();
                    removed.push(id.clone());
                }
            }
            (state.link.clone(), removed)
        };
        if active_ids.is_empty() {
            return Ok(());
        }
        self.bump_status();
        if let Some(link) = link {
            let results = futures_util::future::join_all(active_ids.iter().map(|id| {
                let link = link.clone();
                let id = id.clone();
                async move { link.call(Call::Unpublish { id }).await }
            }))
            .await;
            if results.into_iter().any(|r| r.is_err()) {
                self.disconnect(true);
                return Err(Code::NotConnected);
            }
        }
        let _ = self.edit(|local| {
            for id in &active_ids {
                local.record_share_active(id, false, "");
            }
            Ok(())
        });
        Ok(())
    }

    pub(super) async fn remove_shares(self: &Arc<Self>, ids: Vec<String>) -> Result<(), Code> {
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
        self.unpublish_many(&unique).await?;
        let id_set: HashSet<&str> = unique.iter().map(String::as_str).collect();
        self.edit(|l| {
            for id in &unique {
                l.record_share_active(id, false, "");
            }
            let shares = l.shares_mut()?;
            let mut touched_ips = HashSet::new();
            for s in shares.iter() {
                if id_set.contains(s.id.as_str())
                    && let Some(ip) = s.single_ip()
                {
                    touched_ips.insert(ip);
                }
            }
            shares.retain(|s| !id_set.contains(s.id.as_str()));
            for ip in touched_ips {
                normalize_ip_host_os(shares, ip);
            }
            Ok(())
        })?;
        self.push_profile_bg();
        Ok(())
    }

    pub(super) async fn save_host(&self, update: HostUpdate) -> Result<(), Code> {
        let HostUpdate {
            ip,
            old_ip,
            name,
            host_os,
            services,
            publish,
            replace,
            old_host_name,
        } = update;
        if services.is_empty() || services.len() > probe::MAX_HOST_PROBE_PORTS {
            return Err(Code::Invalid);
        }
        let ip_text = ip.to_string();
        Rules::parse(&ip_text, "80").map_err(|_| Code::Invalid)?;
        let source_ip = old_ip.unwrap_or(ip);
        let migrating_ip = source_ip != ip;
        let source_ip_text = source_ip.to_string();
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
        let old_name_cmp = old_host_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);

        let (remove_ids, stop_before_update_ids) = {
            let state = self.state.lock().unwrap();
            if migrating_ip
                && state
                    .local
                    .shares()
                    .iter()
                    .any(|s| s.single_ip() == Some(ip))
            {
                return Err(Code::Conflict);
            }
            let mut remove_ids = Vec::new();
            let mut stop_ids = Vec::new();
            for share in state.local.shares() {
                if share.single_ip() != Some(source_ip) {
                    continue;
                }
                let [port] = share.rules.ports.as_slice() else {
                    continue;
                };
                if port.start != port.end {
                    continue;
                }
                let found = services.iter().find(|s| s.port == port.start);
                match found {
                    None if replace => remove_ids.push(share.id.clone()),
                    Some(svc) if state.sharing.contains_key(&share.id) => {
                        let rename = !replace
                            || share.name == source_ip_text
                            || share.name == ip_text
                            || old_name_cmp.as_ref().is_some_and(|old| &share.name == old);
                        let next_name = if rename { &label } else { &share.name };
                        let next_os = if matches!(svc.kind, ServiceKind::Ssh | ServiceKind::Rdp) {
                            host_os
                        } else {
                            None
                        };
                        if migrating_ip
                            || next_name != &share.name
                            || share.effective_kind() != svc.kind
                            || share.host_os != next_os
                        {
                            stop_ids.push(share.id.clone());
                        }
                    }
                    _ => {}
                }
            }
            (remove_ids, stop_ids)
        };

        let mut stop_all = Vec::with_capacity(remove_ids.len() + stop_before_update_ids.len());
        stop_all.extend(remove_ids.iter().cloned());
        stop_all.extend(stop_before_update_ids.iter().cloned());
        self.unpublish_many(&stop_all).await?;

        let (publish_ids, republish_ids) = {
            let mut state = self.state.lock().unwrap();
            if migrating_ip
                && state
                    .local
                    .shares()
                    .iter()
                    .any(|s| s.single_ip() == Some(ip))
            {
                return Err(Code::Conflict);
            }
            let remove_set: HashSet<&str> = remove_ids.iter().map(String::as_str).collect();
            let stop_set: HashSet<&str> =
                stop_before_update_ids.iter().map(String::as_str).collect();
            let mut next = state.local.clone();
            for id in &remove_ids {
                next.record_share_active(id, false, "");
            }
            let mut publish_ids = Vec::new();
            let mut republish_ids = Vec::new();
            let mut new_entries = Vec::new();
            {
                let shares = next.shares_mut()?;
                if !remove_set.is_empty() {
                    shares.retain(|s| !remove_set.contains(s.id.as_str()));
                }
                for svc in &services {
                    let existing = shares.iter_mut().find(|s| {
                        s.single_ip() == Some(source_ip)
                            && matches!(
                                s.rules.ports.as_slice(),
                                [p] if p.start == svc.port && p.end == svc.port
                            )
                    });
                    if let Some(share) = existing {
                        if migrating_ip {
                            share.rules = Rules::parse(&ip_text, &svc.port.to_string())
                                .map_err(|_| Code::Invalid)?;
                        }
                        let rename = !replace
                            || share.name == source_ip_text
                            || share.name == ip_text
                            || old_name_cmp.as_ref().is_some_and(|old| &share.name == old);
                        if rename {
                            share.name = label.clone();
                        }
                        share.kind = Some(svc.kind);
                        share.http = svc.kind == ServiceKind::Http;
                        share.host_os = if matches!(svc.kind, ServiceKind::Ssh | ServiceKind::Rdp) {
                            host_os
                        } else {
                            None
                        };
                        if stop_set.contains(share.id.as_str()) {
                            republish_ids.push(share.id.clone());
                        } else if publish {
                            publish_ids.push(share.id.clone());
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
                            host_os: if matches!(svc.kind, ServiceKind::Ssh | ServiceKind::Rdp) {
                                host_os
                            } else {
                                None
                            },
                        }
                        .checked()?;
                        publish_ids.push(id);
                        new_entries.push(rule);
                    }
                }
                if shares.len() + new_entries.len() > 32 {
                    return Err(Code::Limit);
                }
                shares.extend(new_entries);
                if migrating_ip {
                    normalize_ip_host_os(shares, source_ip);
                }
                normalize_ip_host_os(shares, ip);
            }
            next.save(&self.dir)?;
            state.local = next;
            drop(state);
            self.bump_status();
            (publish_ids, republish_ids)
        };

        let mut to_share = republish_ids;
        if publish {
            for id in publish_ids {
                if !to_share.contains(&id) {
                    to_share.push(id);
                }
            }
        }
        if !to_share.is_empty() {
            self.share_many(to_share, true).await?;
        }
        Ok(())
    }

    pub(super) fn restrict_local(&self) {
        let mut state = self.state.lock().unwrap();
        let had = !state.sharing.is_empty();
        for (_, r) in state.sharing.drain() {
            r.cancel.cancel();
        }
        drop(state);
        if had {
            self.bump_status();
        }
    }

    pub(super) fn push_profile_bg(self: &Arc<Self>) {
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
        mut profiles: Vec<DeviceProfile>,
    ) {
        let (labels_changed, to_push, initial_profile) = {
            let mut state = self.state.lock().unwrap();
            if !Self::is_link(&state, link) {
                return;
            }
            for p in &mut profiles {
                if p.machine_id != self.device_info.machine_id {
                    p.forwards.clear();
                }
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
        } else {
            self.bump_status();
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
        let (changed, blocked_changed) = {
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
            let prev_blocked = state.auto_share_blocked;
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
            (changed, state.auto_share_blocked != prev_blocked)
        };
        if changed {
            self.changed();
        } else if blocked_changed {
            self.bump_status();
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
}

pub(super) fn normalize_ip_host_os(shares: &mut [Publication], ip: Ipv4Addr) {
    let resolved = shares
        .iter()
        .filter(|s| s.single_ip() == Some(ip))
        .find_map(|s| match s.effective_kind() {
            ServiceKind::Rdp => Some(HostOs::Windows),
            ServiceKind::Ssh => s.host_os,
            _ => None,
        });
    for s in shares.iter_mut() {
        if s.single_ip() == Some(ip) {
            s.host_os = resolved;
        }
    }
}
