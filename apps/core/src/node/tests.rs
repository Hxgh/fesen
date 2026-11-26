use super::{
    Command, Core, HostServiceInput,
    types::Sharing,
};
use crate::{
    control::Link,
    direct,
    local::{Identity, Local, Login},
    platform::{Network, Platform, Tun},
};
use fesen_acl::Rules;
use fesen_proto::{Code, HostOs, Offering, Open, Peer, Publication, ServiceKind, View};
use std::{
    io,
    net::{Ipv4Addr, SocketAddrV4},
    sync::Arc,
    time::Duration,
};
use tokio::time::timeout;

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
        direct::Direct::new(member.clone(), &*platform, core.quit.child_token(), false).unwrap();
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
            replace: false,
            old_host_name: None,
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
            replace: false,
            old_host_name: None,
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
            republish: false,
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
            replace: false,
            old_host_name: None,
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

#[tokio::test]
async fn host_replace_edit_and_remove_many_manage_full_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::new(dir.path().to_path_buf(), Arc::new(LocalPlatform), false).unwrap();
    core.state.lock().unwrap().local.session = Some(Login {
        member: "alice".into(),
        token: "t".repeat(64),
    });

    let host_ip = Ipv4Addr::new(198, 51, 100, 50);
    let s1 = core
        .execute(Command::SaveHost {
            ip: host_ip,
            name: "Server-A".into(),
            host_os: Some(HostOs::Linux),
            services: vec![
                HostServiceInput {
                    port: 22,
                    kind: ServiceKind::Ssh,
                },
                HostServiceInput {
                    port: 80,
                    kind: ServiceKind::Tcp,
                },
                HostServiceInput {
                    port: 8080,
                    kind: ServiceKind::Http,
                },
            ],
            publish: false,
            replace: false,
            old_host_name: None,
        })
        .await
        .unwrap();
    assert_eq!(s1.shares.len(), 3);
    let id_80 = s1
        .shares
        .iter()
        .find(|s| s.rules.ports[0].start == 80)
        .unwrap()
        .id
        .clone();

    // Give port 80 a custom subname and change it via Command::Save.
    let s2 = core
        .execute(Command::Save {
            id: Some(id_80.clone()),
            name: "Web Gateway".into(),
            ips: "198.51.100.50".into(),
            ports: "80".into(),
            http: true,
            kind: Some(ServiceKind::Http),
            host_os: Some(HostOs::Linux),
            republish: false,
        })
        .await
        .unwrap();
    assert_eq!(
        s2.shares.iter().find(|s| s.id == id_80).unwrap().name,
        "Web Gateway"
    );

    // Manage Host with replace=true: remove 22 (SSH), keep 80 (preserve custom name "Web Gateway"),
    // rename host-named 8080 -> "Server-B" & upgrade kind, add 443 (HTTPS).
    let s3 = core
        .execute(Command::SaveHost {
            ip: host_ip,
            name: "Server-B".into(),
            host_os: None,
            services: vec![
                HostServiceInput {
                    port: 80,
                    kind: ServiceKind::Http,
                },
                HostServiceInput {
                    port: 8080,
                    kind: ServiceKind::Https,
                },
                HostServiceInput {
                    port: 443,
                    kind: ServiceKind::Https,
                },
            ],
            publish: false,
            replace: true,
            old_host_name: Some("Server-A".into()),
        })
        .await
        .unwrap();
    assert_eq!(s3.shares.len(), 3);
    assert!(!s3.shares.iter().any(|s| s.rules.ports[0].start == 22));
    assert_eq!(
        s3.shares.iter().find(|s| s.id == id_80).unwrap().name,
        "Web Gateway"
    );
    let port_8080 = s3
        .shares
        .iter()
        .find(|s| s.rules.ports[0].start == 8080)
        .unwrap();
    assert_eq!(port_8080.name, "Server-B");
    assert_eq!(port_8080.effective_kind(), ServiceKind::Https);
    // Since SSH was removed, host_os is cleared across remaining ports.
    assert!(s3.shares.iter().all(|s| s.host_os.is_none()));

    // RemoveMany removes the entire host atomically.
    let all_ids: Vec<String> = s3.shares.iter().map(|s| s.id.clone()).collect();
    let s4 = core
        .execute(Command::RemoveMany { ids: all_ids })
        .await
        .unwrap();
    assert!(s4.shares.is_empty());

    core.stop();
    core.wait_stopped().await;
}

#[tokio::test]
async fn conditional_status_poll_skips_unchanged_and_tracks_mutations() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::new(dir.path().to_path_buf(), Arc::new(LocalPlatform), false).unwrap();

    let initial_bytes = crate::ipc::dispatch_bytes(&core, br#"{"op":"status","ui":true}"#).await;
    let initial: serde_json::Value = serde_json::from_slice(&initial_bytes).unwrap();
    let version = initial["data"]["version"].as_u64().unwrap();
    assert!(version > 0);

    // Matching version returns the static 18-byte unchanged sentinel.
    let poll_req = format!(r#"{{"op":"status","ui":true,"since":{version}}}"#);
    let unchanged = crate::ipc::dispatch_bytes(&core, poll_req.as_bytes()).await;
    assert_eq!(unchanged, br#"{"unchanged":true}"#);

    // Mutating state bumps version and returns full fresh status.
    core.report(Code::Offline);
    let updated_bytes = crate::ipc::dispatch_bytes(&core, poll_req.as_bytes()).await;
    let updated: serde_json::Value = serde_json::from_slice(&updated_bytes).unwrap();
    assert_eq!(updated["data"]["error"], "offline");
    let next_version = updated["data"]["version"].as_u64().unwrap();
    assert_ne!(next_version, version);

    // Polling with the new version returns unchanged again.
    let next_req = format!(r#"{{"op":"status","ui":true,"since":{next_version}}}"#);
    assert_eq!(
        crate::ipc::dispatch_bytes(&core, next_req.as_bytes()).await,
        br#"{"unchanged":true}"#
    );

    core.stop();
    core.wait_stopped().await;
}
