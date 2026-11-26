use super::{
    Command, Core, Status,
    forwards::suggest_free_port,
    shares::normalize_ip_host_os,
    types::HostUpdate,
};
use crate::{
    admin_client,
    routing::{Label, alias_name},
};
use fesen_acl::Rules;
use fesen_proto::{
    Call, Code, Credential, LocalMapping, MemberPref, Publication, ServiceKind, origin, valid_id,
    valid_label, valid_machine_id, valid_username,
};
use std::{
    collections::HashSet,
    net::{Ipv4Addr, SocketAddrV4},
    sync::Arc,
    time::Duration,
};
use tokio::time::{Instant, timeout};

impl Core {
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
                            self.bump_status();
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
                            self.bump_status();
                        }
                        return Ok(self.status());
                    }
                    Err(e) => {
                        if e == Code::Unauthorized {
                            self.state.lock().unwrap().admin_token = None;
                            self.bump_status();
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
                    self.bump_status();
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
                self.bump_status();
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
                drop(state);
                self.bump_status();
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
                self.bump_status();
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
                republish,
            } => {
                let id = id.unwrap_or_else(|| hex::encode(rand::random::<[u8; 16]>()));
                let effective_http = match kind {
                    Some(k) => k == ServiceKind::Http,
                    None => http,
                };
                let mut rule = Publication {
                    id: id.clone(),
                    name: name.trim().to_owned(),
                    rules: Rules::parse(&ips, &ports).map_err(|_| Code::Invalid)?,
                    http: effective_http,
                    kind,
                    host_os,
                }
                .checked()?;
                if !matches!(rule.effective_kind(), ServiceKind::Ssh | ServiceKind::Rdp) {
                    rule.host_os = None;
                }
                let (was_sharing, unchanged) = {
                    let state = self.state.lock().unwrap();
                    let sharing = state.sharing.contains_key(&id);
                    let unchanged = state
                        .local
                        .shares()
                        .iter()
                        .find(|s| s.id == id)
                        .is_some_and(|old| old == &rule);
                    (sharing, unchanged)
                };
                if unchanged {
                    return Ok(());
                }
                if was_sharing {
                    self.share(id.clone(), false).await?;
                }
                let single_ip = rule.single_ip();
                self.edit(|local| {
                    let shares = local.shares_mut()?;
                    let old_ip = shares
                        .iter()
                        .find(|s| s.id == id)
                        .and_then(Publication::single_ip);
                    if let Some(old) = shares.iter_mut().find(|s| s.id == id) {
                        *old = rule;
                    } else if shares.len() < 32 {
                        shares.push(rule);
                    } else {
                        return Err(Code::Limit);
                    }
                    if let Some(ip) = old_ip {
                        normalize_ip_host_os(shares, ip);
                    }
                    if let Some(ip) = single_ip
                        && Some(ip) != old_ip
                    {
                        normalize_ip_host_os(shares, ip);
                    }
                    Ok(())
                })?;
                if was_sharing && republish {
                    let res = self.share(id, true).await;
                    self.push_profile_bg();
                    return res;
                }
                self.push_profile_bg();
                Ok(())
            }
            Command::SaveHost {
                ip,
                name,
                host_os,
                services,
                publish,
                replace,
                old_host_name,
            } => {
                self.save_host(HostUpdate {
                    ip,
                    name,
                    host_os,
                    services,
                    publish,
                    replace,
                    old_host_name,
                })
                .await?;
                self.push_profile_bg();
                Ok(())
            }
            Command::Share { id, enabled } => self.share(id, enabled).await,
            Command::ShareMany { ids, enabled } => self.share_many(ids, enabled).await,
            Command::Remove { id } => self.remove_shares(vec![id]).await,
            Command::RemoveMany { ids } => self.remove_shares(ids).await,
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
                            if shares
                                .iter()
                                .any(|cur| cur.rules == s.rules && cur.name == s.name)
                            {
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
                let link = self
                    .state
                    .lock()
                    .unwrap()
                    .link
                    .clone()
                    .ok_or(Code::NotConnected)?;
                link.call(Call::DeleteProfile {
                    machine_id: machine_id.clone(),
                })
                .await?;
                self.state
                    .lock()
                    .unwrap()
                    .cloud_profiles
                    .retain(|p| p.machine_id != machine_id);
                self.bump_status();
                Ok(())
            }
        }
    }
}
