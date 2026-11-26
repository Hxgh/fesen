use super::{
    COMMON_HOST_PORTS, DEFAULT_HOST_PORTS, HeadOutcome, HostPortProbe, HostProbe, MAX_EXTRA_PORTS,
    MAX_HOST_PROBE_PORTS, NOISY_OS_PORTS, NON_HTTP_PORTS, parse_ssh_banner, probe_http_head, rdp,
    ssh, tls,
};
use crate::platform::{self, Platform};
use fesen_acl::Rules;
use fesen_proto::{Code, HostOs, ServiceKind};
use futures_util::stream::{self, StreamExt};
use std::{
    collections::{HashMap, HashSet},
    net::{Ipv4Addr, SocketAddrV4},
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    time::{Instant, timeout},
};

#[cfg(test)]
pub async fn host(
    platform: &dyn Platform,
    ip: Ipv4Addr,
    extra_ports: &[u16],
) -> Result<HostProbe, Code> {
    host_with_mode(platform, ip, extra_ports, false, false).await
}

pub async fn host_with_mode(
    platform: &dyn Platform,
    ip: Ipv4Addr,
    extra_ports: &[u16],
    deep: bool,
    is_local_ip: bool,
) -> Result<HostProbe, Code> {
    // Enforce the exact same IPv4 non-reserved boundary as publishing rules.
    Rules::parse(&ip.to_string(), "80").map_err(|_| Code::Invalid)?;
    let mut seen_extra = HashSet::new();
    let mut extras = Vec::new();
    for &port in extra_ports {
        if port == 0 {
            return Err(Code::Invalid);
        }
        if seen_extra.insert(port) {
            extras.push(port);
        }
    }
    if extras.len() > MAX_EXTRA_PORTS {
        return Err(Code::Invalid);
    }
    extras.sort_unstable();

    let local_hints: HashMap<u16, Option<String>> = if is_local_ip {
        platform.local_listeners(ip).into_iter().collect()
    } else {
        HashMap::new()
    };

    let started = Instant::now();
    let mut targets: Vec<(u16, bool)> = Vec::new();
    let mut seen = HashSet::new();

    if !deep {
        for &port in &COMMON_HOST_PORTS {
            if seen.insert(port) {
                targets.push((port, !DEFAULT_HOST_PORTS.contains(&port)));
            }
        }
        for &port in &extras {
            if seen.insert(port) {
                targets.push((port, !DEFAULT_HOST_PORTS.contains(&port)));
            }
        }
        for &port in local_hints.keys() {
            if targets.len() >= 48 {
                break;
            }
            if seen.insert(port) {
                targets.push((port, !DEFAULT_HOST_PORTS.contains(&port)));
            }
        }
        let mut out = probe_targets(platform, ip, targets, &seen_extra, &local_hints).await?;
        out.duration_ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
        return Ok(out);
    }

    // Deep scan: prioritize common/extra/local ports first, then bounded parallel TCP connect sweep
    // across 1..=10000 (+ extras/27017), skipping noisy OS RPC ports.
    let mut sweep_seen = HashSet::with_capacity(10_048);
    let mut sweep_ports: Vec<u16> = Vec::with_capacity(10_016);
    for &p in DEFAULT_HOST_PORTS
        .iter()
        .chain(COMMON_HOST_PORTS.iter())
        .chain(extras.iter())
        .chain(local_hints.keys())
    {
        if sweep_seen.insert(p) {
            sweep_ports.push(p);
        }
    }
    for p in 1u16..=10000 {
        if !NOISY_OS_PORTS.contains(&p) && sweep_seen.insert(p) {
            sweep_ports.push(p);
        }
    }
    if sweep_seen.insert(27017) {
        sweep_ports.push(27017);
    }

    let concurrency = if cfg!(target_os = "android") {
        64
    } else {
        192
    };
    let open_ports: Vec<u16> = timeout(
        Duration::from_millis(12_000),
        stream::iter(sweep_ports)
            .map(|port| async move {
                let target = SocketAddrV4::new(ip, port);
                platform::tcp_timeout(platform, target, Duration::from_millis(160))
                    .await
                    .ok()
                    .map(|_| port)
            })
            .buffer_unordered(concurrency)
            .collect::<Vec<_>>(),
    )
    .await
    .map_err(|_| Code::TargetTimeout)?
    .into_iter()
    .flatten()
    .collect();

    if open_ports.is_empty() {
        return Ok(HostProbe {
            ip,
            host_os: None,
            ports: Vec::new(),
            duration_ms: started.elapsed().as_millis().min(u32::MAX as u128) as u32,
            cancelled: false,
        });
    }

    let open_set: HashSet<u16> = open_ports.iter().copied().collect();
    for &port in DEFAULT_HOST_PORTS
        .iter()
        .chain(COMMON_HOST_PORTS.iter())
        .chain(extras.iter())
        .chain(local_hints.keys())
    {
        if open_set.contains(&port) && seen.insert(port) && targets.len() < MAX_HOST_PROBE_PORTS {
            targets.push((port, !DEFAULT_HOST_PORTS.contains(&port)));
        }
    }
    let mut rest: Vec<u16> = open_ports
        .into_iter()
        .filter(|p| !seen.contains(p))
        .collect();
    rest.sort_unstable();
    for port in rest {
        if targets.len() >= MAX_HOST_PROBE_PORTS {
            break;
        }
        if seen.insert(port) {
            targets.push((port, !DEFAULT_HOST_PORTS.contains(&port)));
        }
    }

    let mut out = probe_targets(platform, ip, targets, &seen_extra, &local_hints).await?;
    out.duration_ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
    Ok(out)
}

pub async fn ports(
    platform: &dyn Platform,
    ip: Ipv4Addr,
    requested: &[u16],
) -> Result<HostProbe, Code> {
    Rules::parse(&ip.to_string(), "80").map_err(|_| Code::Invalid)?;
    let mut seen = HashSet::new();
    let mut targets = Vec::new();
    for &port in requested {
        if port == 0 {
            return Err(Code::Invalid);
        }
        if seen.insert(port) {
            targets.push((port, !DEFAULT_HOST_PORTS.contains(&port)));
        }
    }
    if targets.is_empty() || targets.len() > MAX_HOST_PROBE_PORTS {
        return Err(Code::Invalid);
    }
    let empty_hints = HashMap::new();
    probe_targets(platform, ip, targets, &seen, &empty_hints).await
}

pub(super) async fn probe_targets(
    platform: &dyn Platform,
    ip: Ipv4Addr,
    targets: Vec<(u16, bool)>,
    user_requested: &HashSet<u16>,
    local_hints: &HashMap<u16, Option<String>>,
) -> Result<HostProbe, Code> {
    let started = Instant::now();
    // Each port enforces its own bounded budget (<=2200ms) so one slow custom port never discards
    // healthy results from sibling ports; outer timeout is a pure safety guard.
    let raw = timeout(
        Duration::from_millis(3200),
        stream::iter(targets.into_iter().map(|(port, extra)| {
            let timeout_ms = if DEFAULT_HOST_PORTS.contains(&port) || user_requested.contains(&port)
            {
                950
            } else {
                550
            };
            let local_proc = local_hints.get(&port).and_then(|v| v.clone());
            async move {
                let (mut probe, os) =
                    probe_host_port_bounded(platform, ip, port, extra, timeout_ms).await;
                if probe.open && probe.detail.is_none() && local_proc.is_some() {
                    probe.detail = local_proc;
                }
                (probe, os)
            }
        }))
        .buffer_unordered(48)
        .collect::<Vec<_>>(),
    )
    .await
    .map_err(|_| Code::TargetTimeout)?;

    let mut host_os = None;
    let mut saw_linux = false;
    let mut has_rdp = false;
    let mut has_remote_entry = false;

    for (entry, os_hint) in &raw {
        if !entry.open {
            continue;
        }
        if matches!(entry.kind, ServiceKind::Ssh | ServiceKind::Rdp) {
            has_remote_entry = true;
        }
        if entry.kind == ServiceKind::Rdp {
            has_rdp = true;
        }
        match os_hint {
            Some(HostOs::Windows) => host_os = Some(HostOs::Windows),
            Some(HostOs::Linux) => saw_linux = true,
            None => {}
        }
    }
    if host_os.is_none() {
        if has_rdp {
            host_os = Some(HostOs::Windows);
        } else if saw_linux {
            host_os = Some(HostOs::Linux);
        }
    }

    let mut ports: Vec<_> = raw
        .into_iter()
        .map(|(mut entry, _)| {
            let explicit = user_requested.contains(&entry.port);
            entry.recommended = entry.open
                && match entry.kind {
                    ServiceKind::Ssh | ServiceKind::Rdp => true,
                    ServiceKind::Smb => explicit,
                    ServiceKind::Http | ServiceKind::Https => entry.extra || !has_remote_entry,
                    ServiceKind::Tcp => {
                        (explicit || local_hints.contains_key(&entry.port))
                            && !NON_HTTP_PORTS.contains(&entry.port)
                    }
                };
            entry
        })
        .collect();
    ports.sort_unstable_by_key(|p| p.port);

    Ok(HostProbe {
        ip,
        host_os,
        ports,
        duration_ms: started.elapsed().as_millis().min(u32::MAX as u128) as u32,
        cancelled: false,
    })
}

pub(super) async fn probe_host_port_bounded(
    platform: &dyn Platform,
    ip: Ipv4Addr,
    port: u16,
    extra: bool,
    connect_ms: u64,
) -> (HostPortProbe, Option<HostOs>) {
    let default_kind = match port {
        22 => ServiceKind::Ssh,
        3389 => ServiceKind::Rdp,
        445 => ServiceKind::Smb,
        80 => ServiceKind::Http,
        443 => ServiceKind::Https,
        _ => ServiceKind::Tcp,
    };
    let target = SocketAddrV4::new(ip, port);
    let started = Instant::now();
    let Ok(mut stream) =
        platform::tcp_timeout(platform, target, Duration::from_millis(connect_ms)).await
    else {
        return (
            HostPortProbe {
                port,
                open: false,
                extra,
                kind: default_kind,
                recommended: false,
                rtt_ms: None,
                http_status: None,
                detail: None,
                os_hint: None,
            },
            None,
        );
    };
    // Measure pure TCP connect latency before protocol fingerprinting waits.
    let rtt_ms = started.elapsed().as_millis().clamp(1, u32::MAX as u128) as u32;

    let fingerprint = async {
        match port {
            22 => match ssh(&mut stream, Duration::from_millis(1150)).await {
                Ok(info) => (ServiceKind::Ssh, None, Some(info.banner), info.os),
                Err(_) => (ServiceKind::Tcp, None, None, None),
            },
            3389 => match rdp(&mut stream, Duration::from_millis(800)).await {
                Ok(info) => (
                    ServiceKind::Rdp,
                    None,
                    Some(if info.nla { "NLA" } else { "RDP" }.into()),
                    Some(HostOs::Windows),
                ),
                Err(_) => (ServiceKind::Tcp, None, None, None),
            },
            445 => (ServiceKind::Smb, None, None, None),
            80 => match probe_http_head(&mut stream, target, Duration::from_millis(800)).await {
                HeadOutcome::Http(code) => (ServiceKind::Http, Some(code), None, None),
                _ => (ServiceKind::Tcp, None, None, None),
            },
            443 => match tls(&mut stream, Duration::from_millis(800)).await {
                Ok(()) => (ServiceKind::Https, None, None, None),
                Err(_) => (ServiceKind::Tcp, None, None, None),
            },
            _ => {
                // Custom user-requested port: check passive banner first (catches custom SSH like 2222).
                let mut banner_buf = [0u8; 160];
                match timeout(Duration::from_millis(240), stream.read(&mut banner_buf)).await {
                    Ok(Ok(n @ 1..)) => {
                        if let Some(info) = parse_ssh_banner(&banner_buf[..n]) {
                            (ServiceKind::Ssh, None, Some(info.banner), info.os)
                        } else {
                            (ServiceKind::Tcp, None, None, None)
                        }
                    }
                    Ok(Ok(0)) | Ok(Err(_)) => (ServiceKind::Tcp, None, None, None),
                    Err(_) => {
                        if NON_HTTP_PORTS.contains(&port) {
                            return (ServiceKind::Tcp, None, None, None);
                        }
                        match probe_http_head(&mut stream, target, Duration::from_millis(650)).await
                        {
                            HeadOutcome::Http(code) => (ServiceKind::Http, Some(code), None, None),
                            HeadOutcome::ClosedOrAlert => {
                                drop(stream);
                                if let Ok(mut tls_stream) = platform::tcp_timeout(
                                    platform,
                                    target,
                                    Duration::from_millis(500),
                                )
                                .await
                                    && tls(&mut tls_stream, Duration::from_millis(650))
                                        .await
                                        .is_ok()
                                {
                                    (ServiceKind::Https, None, None, None)
                                } else {
                                    (ServiceKind::Tcp, None, None, None)
                                }
                            }
                            HeadOutcome::Other => (ServiceKind::Tcp, None, None, None),
                        }
                    }
                }
            }
        }
    };

    // If protocol identification exceeds remaining budget, still preserve the open TCP port.
    let (kind, http_status, detail, os_hint) = timeout(Duration::from_millis(1200), fingerprint)
        .await
        .unwrap_or((ServiceKind::Tcp, None, None, None));

    (
        HostPortProbe {
            port,
            open: true,
            extra,
            kind,
            recommended: false,
            rtt_ms: Some(rtt_ms),
            http_status,
            detail,
            os_hint,
        },
        os_hint,
    )
}
