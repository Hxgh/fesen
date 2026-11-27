use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_account_supports_multiple_concurrent_devices() {
    let dir = tempfile::tempdir().unwrap();
    let db = store::db(&dir.path().join("db")).unwrap();
    let hash = store::password_hash(PASSWORD).unwrap();
    store::create(&db, "boss", &hash, true).unwrap();

    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = http.local_addr().unwrap();
    let base = origin(&format!("http://{server}")).unwrap();
    let app = App::new(db, base.clone(), 0, None).unwrap();
    let web = tokio::spawn(axum::serve(http, router(app)).into_future());

    let mac = Core::new(
        dir.path().join("mac"),
        Adapter::new("mac", Ipv4Addr::new(192, 0, 2, 10), None),
        false,
    )
    .unwrap();
    let phone = Core::new(
        dir.path().join("phone"),
        Adapter::new("phone", Ipv4Addr::new(198, 51, 100, 20), None),
        false,
    )
    .unwrap();

    for (core, label) in [(&mac, "BossMac"), (&phone, "BossPhone")] {
        core.execute(Command::Join {
            address: base.to_string(),
        })
        .await
        .unwrap();
        let st = core
            .execute(Command::Login {
                username: "boss".into(),
                password: PASSWORD.into(),
                label: label.into(),
            })
            .await
            .unwrap();
        assert!(st.is_admin && st.admin_ready);
        eventually(|| core.status().online).await;
    }

    // Both concurrent devices under the same account show up online in admin session.
    let snap = mac
        .execute(Command::AdminSession)
        .await
        .unwrap()
        .admin_data
        .unwrap();
    let online_devices: Vec<_> = snap["devices"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["member"] == "boss" && d["online"] == true)
        .collect();
    assert_eq!(online_devices.len(), 2);

    // Mac publishes a service; same-account Phone receives it while Mac does not duplicate itself.
    let saved = mac
        .execute(Command::Save {
            id: None,
            name: "Office".into(),
            ips: "192.0.2.35".into(),
            ports: "80".into(),
            http: true,
            kind: Some(fesen_proto::ServiceKind::Http),
            host_os: None,
            republish: false,
        })
        .await
        .unwrap();
    let share_id = saved.shares[0].id.clone();
    mac.execute(Command::Share {
        id: share_id.clone(),
        enabled: true,
    })
    .await
    .unwrap();
    eventually(|| phone.status().services.iter().any(|o| o.service.id == share_id)).await;
    let phone_view = phone.status().services;
    let offered = phone_view.iter().find(|o| o.service.id == share_id).unwrap();
    assert_eq!(offered.member, "boss");
    assert_eq!(offered.device.as_deref(), Some("BossMac"));
    assert!(mac.status().services.is_empty());

    // Logging out Phone only logs out Phone; Mac stays online with active share & valid admin session.
    phone.execute(Command::Logout).await.unwrap();
    assert!(phone.status().member.is_none());
    let mac_st = mac.status();
    assert!(mac_st.online && mac_st.active.contains(&share_id));
    assert!(mac.execute(Command::AdminSession).await.is_ok());

    for core in [mac, phone] {
        core.stop();
        core.wait_stopped().await;
    }
    web.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn startup_preferences_auto_restore_only_on_same_network() {
    let dir = tempfile::tempdir().unwrap();
    let db = store::db(&dir.path().join("db")).unwrap();
    let hash = store::password_hash(PASSWORD).unwrap();
    store::create(&db, "alice", &hash, false).unwrap();

    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = http.local_addr().unwrap();
    let base = origin(&format!("http://{server}")).unwrap();
    let app = App::new(db, base.clone(), 0, None).unwrap();
    let app_handle = app.clone();
    let web = tokio::spawn(axum::serve(http, router(app)).into_future());

    let net = Adapter::new("home-lan", Ipv4Addr::new(192, 0, 2, 10), None);
    let core1 = Core::new(dir.path().join("alice"), net.clone(), false).unwrap();
    assert!(core1.status().auto_connect && core1.status().auto_share);
    core1
        .execute(Command::Join {
            address: base.to_string(),
        })
        .await
        .unwrap();
    core1
        .execute(Command::Login {
            username: "alice".into(),
            password: PASSWORD.into(),
            label: "AliceMac".into(),
        })
        .await
        .unwrap();
    eventually(|| core1.status().online).await;

    let s1 = core1
        .execute(Command::Save {
            id: None,
            name: "Web".into(),
            ips: "192.0.2.35".into(),
            ports: "80".into(),
            http: true,
            kind: Some(fesen_proto::ServiceKind::Http),
            host_os: None,
            republish: false,
        })
        .await
        .unwrap();
    let keep_on = s1.shares[0].id.clone();
    let s2 = core1
        .execute(Command::Save {
            id: None,
            name: "SSH".into(),
            ips: "192.0.2.35".into(),
            ports: "22".into(),
            http: false,
            kind: Some(fesen_proto::ServiceKind::Ssh),
            host_os: None,
            republish: false,
        })
        .await
        .unwrap();
    let keep_off = s2
        .shares
        .iter()
        .find(|s| s.id != keep_on)
        .unwrap()
        .id
        .clone();

    core1
        .execute(Command::Share {
            id: keep_on.clone(),
            enabled: true,
        })
        .await
        .unwrap();
    core1
        .execute(Command::Share {
            id: keep_off.clone(),
            enabled: true,
        })
        .await
        .unwrap();
    core1
        .execute(Command::Share {
            id: keep_off.clone(),
            enabled: false,
        })
        .await
        .unwrap();

    core1.execute(Command::Quit).await.unwrap();
    core1.wait_stopped().await;

    // Restarting on the SAME network automatically restores only `keep_on`!
    let core2 = Core::new(dir.path().join("alice"), net.clone(), false).unwrap();
    core2.execute(Command::Start).await.unwrap();
    eventually(|| core2.status().active.contains(&keep_on)).await;
    assert!(!core2.status().active.contains(&keep_off));
    assert!(!core2.status().auto_share_blocked);
    // Transient control-link drop on the same network auto-restores shares on reconnect.
    let dev_id = core2.status().device_id.unwrap();
    app_handle.inner.lock().unwrap().remove(&dev_id, false);
    eventually(|| !core2.status().online).await;
    eventually(|| core2.status().online && core2.status().active.contains(&keep_on)).await;
    assert!(!core2.status().active.contains(&keep_off));
    core2.execute(Command::Quit).await.unwrap();
    core2.wait_stopped().await;

    // Restarting on a DIFFERENT network refuses to auto-publish shares and flags auto_share_blocked.
    net.network.lock().unwrap().marker = "coffee-wifi".into();
    let core3 = Core::new(dir.path().join("alice"), net.clone(), false).unwrap();
    core3.execute(Command::Start).await.unwrap();
    eventually(|| core3.status().online && core3.status().auto_share_blocked).await;
    assert!(core3.status().active.is_empty());

    // Disabling preferences persists across restarts.
    let updated = core3
        .execute(Command::SetPreferences {
            auto_connect: Some(false),
            auto_share: Some(false),
            cloud_backup: None,
        })
        .await
        .unwrap();
    assert!(!updated.auto_connect && !updated.auto_share && !updated.auto_share_blocked);
    core3.execute(Command::Quit).await.unwrap();
    core3.wait_stopped().await;

    net.network.lock().unwrap().marker = "home-lan".into();
    let core4 = Core::new(dir.path().join("alice"), net, false).unwrap();
    assert!(!core4.status().auto_connect && !core4.status().auto_share);
    core4.execute(Command::Start).await.unwrap();
    eventually(|| core4.status().online).await;
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(core4.status().active.is_empty());

    core4.stop();
    core4.wait_stopped().await;
    web.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_device_overlap_and_local_127_port_forwarding_with_conflict_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let db = store::db(&dir.path().join("db")).unwrap();
    let hash = store::password_hash(PASSWORD).unwrap();
    for who in ["dev1", "dev2", "consumer"] {
        store::create(&db, who, &hash, false).unwrap();
    }
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", http.local_addr().unwrap());
    let app = App::new(db, origin(&base).unwrap(), 0, None).unwrap();
    let web = tokio::spawn(axum::serve(http, router(app)).into_future());

    let shared_ip: Ipv4Addr = "192.0.2.81".parse().unwrap();
    let shared_port = 8080u16;
    let d1 = Core::new(
        dir.path().join("dev1"),
        Adapter::with_device("D1", Ipv4Addr::new(192, 0, 2, 11), None, DeviceOs::Macos, "m1"),
        false,
    )
    .unwrap();
    let d2 = Core::new(
        dir.path().join("dev2"),
        Adapter::with_device("D2", Ipv4Addr::new(198, 51, 100, 12), None, DeviceOs::Windows, "m2"),
        false,
    )
    .unwrap();
    let consumer = Core::new(
        dir.path().join("consumer"),
        Adapter::with_device("Reader", Ipv4Addr::new(203, 0, 113, 13), None, DeviceOs::Linux, "m3"),
        false,
    )
    .unwrap();

    for (core, user) in [(&d1, "dev1"), (&d2, "dev2"), (&consumer, "consumer")] {
        core.execute(Command::Join {
            address: base.clone(),
        })
        .await
        .unwrap();
        core.execute(Command::Login {
            username: user.into(),
            password: PASSWORD.into(),
            label: String::new(),
        })
        .await
        .unwrap();
        eventually(|| core.status().online).await;
    }

    // Both dev1 and dev2 publish the EXACT same IP and port — center allows both!
    let (id1, res1) = publish(&d1, "SameA", shared_ip, shared_port).await;
    let (id2, res2) = publish(&d2, "SameB", shared_ip, shared_port).await;
    res1.unwrap();
    res2.unwrap();
    eventually(|| consumer.status().services.len() == 2).await;

    let c_status = consumer.status();
    let svc1_mapped = c_status.service_virtual_ips.contains_key(&id1);
    let svc2_mapped = c_status.service_virtual_ips.contains_key(&id2);
    assert!(svc1_mapped ^ svc2_mapped, "exactly one overlapping peer gets virtual remap");

    // Pick a free 127.0.0.1 local port via SuggestLocalPort and forward id1.
    let lp1 = consumer
        .execute(Command::SuggestLocalPort { preferred: None })
        .await
        .unwrap()
        .suggested_port
        .unwrap();
    let st = consumer
        .execute(Command::SetLocalForward {
            service: id1.clone(),
            target_ip: shared_ip,
            target_port: shared_port,
            local_port: lp1,
        })
        .await
        .unwrap();
    let fwd1 = st.forwards.iter().find(|f| f.service == id1).unwrap();
    assert!(fwd1.active && fwd1.error.is_none() && fwd1.local_port == lp1);

    // Localhost listener is actually open on 127.0.0.1:lp1.
    let _tcp = TcpStream::connect((Ipv4Addr::LOCALHOST, lp1)).await.unwrap();

    // Trying to assign the exact same local_port to id2 is rejected with Conflict.
    assert_eq!(
        consumer
            .execute(Command::SetLocalForward {
                service: id2.clone(),
                target_ip: shared_ip,
                target_port: shared_port,
                local_port: lp1,
            })
            .await
            .err(),
        Some(Code::Conflict)
    );

    // Simulate runtime port occupation: save lp2 while offline, occupy lp2 externally, reconnect -> Conflict + suggested_port!
    let lp2 = consumer
        .execute(Command::SuggestLocalPort {
            preferred: Some(lp1 + 1),
        })
        .await
        .unwrap()
        .suggested_port
        .unwrap();
    consumer
        .execute(Command::SetLocalForward {
            service: id2.clone(),
            target_ip: shared_ip,
            target_port: shared_port,
            local_port: lp2,
        })
        .await
        .unwrap();
    consumer.execute(Command::Stop).await.unwrap();
    let external_holder = TcpListener::bind((Ipv4Addr::LOCALHOST, lp2)).await.unwrap();
    consumer.execute(Command::Start).await.unwrap();
    eventually(|| {
        consumer.status().online
            && consumer.status().forwards.iter().any(|f| {
                f.service == id2
                    && !f.active
                    && f.error == Some(Code::Conflict)
                    && f.suggested_port.is_some()
            })
    })
    .await;
    let conflict_fwd = consumer
        .status()
        .forwards
        .into_iter()
        .find(|f| f.service == id2)
        .unwrap();
    let replacement = conflict_fwd.suggested_port.unwrap();
    assert_ne!(replacement, lp1);
    assert_ne!(replacement, lp2);

    // One-click switch to suggested_port recovers immediately.
    let recovered = consumer
        .execute(Command::SetLocalForward {
            service: id2.clone(),
            target_ip: shared_ip,
            target_port: shared_port,
            local_port: replacement,
        })
        .await
        .unwrap();
    let fwd2 = recovered.forwards.iter().find(|f| f.service == id2).unwrap();
    assert!(fwd2.active && fwd2.error.is_none() && fwd2.local_port == replacement);
    let _tcp2 = TcpStream::connect((Ipv4Addr::LOCALHOST, replacement))
        .await
        .unwrap();

    drop(external_holder);
    for core in [d1, d2, consumer] {
        core.stop();
        core.wait_stopped().await;
    }
    web.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "requires local non-loopback TCP; run in a Linux container"
)]
async fn local_127_forward_relays_real_tcp_payload_to_overlapping_peers() {
    let dir = tempfile::tempdir().unwrap();
    let db = store::db(&dir.path().join("db")).unwrap();
    let hash = store::password_hash(PASSWORD).unwrap();
    for who in ["dev1", "dev2", "consumer"] {
        store::create(&db, who, &hash, false).unwrap();
    }
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", http.local_addr().unwrap());
    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let app = App::new(db, origin(&base).unwrap(), udp.local_addr().unwrap().port(), None).unwrap();
    let discovery = tokio::spawn(discovery(udp, app.clone()));
    let web = tokio::spawn(axum::serve(http, router(app)).into_future());

    let ip = local_ipv4();
    let (echo_port, echo_task) = echo(ip, b'X').await;
    let d1 = Core::new(
        dir.path().join("dev1"),
        Adapter::with_device("D1", ip, None, DeviceOs::Macos, "m1"),
        false,
    )
    .unwrap();
    let d2 = Core::new(
        dir.path().join("dev2"),
        Adapter::with_device("D2", ip, None, DeviceOs::Windows, "m2"),
        false,
    )
    .unwrap();
    let consumer = Core::new(
        dir.path().join("consumer"),
        Adapter::with_device("Reader", ip, None, DeviceOs::Linux, "m3"),
        false,
    )
    .unwrap();

    for (core, user) in [(&d1, "dev1"), (&d2, "dev2"), (&consumer, "consumer")] {
        core.execute(Command::Join {
            address: base.clone(),
        })
        .await
        .unwrap();
        core.execute(Command::Login {
            username: user.into(),
            password: PASSWORD.into(),
            label: String::new(),
        })
        .await
        .unwrap();
        eventually(|| core.status().online).await;
    }

    let (id1, res1) = publish(&d1, "SameA", ip, echo_port).await;
    let (id2, res2) = publish(&d2, "SameB", ip, echo_port).await;
    res1.unwrap();
    res2.unwrap();
    eventually(|| consumer.status().services.len() == 2).await;

    let lp1 = consumer
        .execute(Command::SuggestLocalPort { preferred: None })
        .await
        .unwrap()
        .suggested_port
        .unwrap();
    let lp2 = consumer
        .execute(Command::SuggestLocalPort {
            preferred: Some(lp1 + 1),
        })
        .await
        .unwrap()
        .suggested_port
        .unwrap();
    consumer
        .execute(Command::SetLocalForward {
            service: id1,
            target_ip: ip,
            target_port: echo_port,
            local_port: lp1,
        })
        .await
        .unwrap();
    consumer
        .execute(Command::SetLocalForward {
            service: id2,
            target_ip: ip,
            target_port: echo_port,
            local_port: lp2,
        })
        .await
        .unwrap();

    for port in [lp1, lp2] {
        let mut tcp = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await.unwrap();
        tcp.write_all(b"hello").await.unwrap();
        let mut reply = [0u8; 6];
        timeout(Duration::from_secs(6), tcp.read_exact(&mut reply))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&reply, b"Xhello");
    }

    for core in [d1, d2, consumer] {
        core.stop();
        core.wait_stopped().await;
    }
    discovery.abort();
    web.abort();
    echo_task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn device_identity_os_reinstall_restore_and_cloud_sync() {
    let dir = tempfile::tempdir().unwrap();
    let db = store::db(&dir.path().join("db")).unwrap();
    let hash = store::password_hash(PASSWORD).unwrap();
    store::create(&db, "cloud", &hash, true).unwrap();

    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", http.local_addr().unwrap());
    let app = App::new(db, origin(&base).unwrap(), 0, None).unwrap();
    let web = tokio::spawn(axum::serve(http, router(app)).into_future());

    let mac_adapter = Adapter::with_device(
        "mac-dev",
        Ipv4Addr::new(192, 0, 2, 10),
        None,
        DeviceOs::Macos,
        "hw-mac-uuid",
    );
    let phone_adapter = Adapter::with_device(
        "Pixel-8",
        Ipv4Addr::new(198, 51, 100, 20),
        None,
        DeviceOs::Android,
        "hw-android-id",
    );
    let mac = Core::new(dir.path().join("mac"), mac_adapter.clone(), false).unwrap();
    let phone = Core::new(dir.path().join("phone"), phone_adapter.clone(), false).unwrap();

    for core in [&mac, &phone] {
        core.execute(Command::Join {
            address: base.clone(),
        })
        .await
        .unwrap();
        core.execute(Command::Login {
            username: "cloud".into(),
            password: PASSWORD.into(),
            label: String::new(),
        })
        .await
        .unwrap();
        eventually(|| core.status().online).await;
    }

    // Verify auto-detected OS & machine_id are recorded on server admin session.
    let snap = mac
        .execute(Command::AdminSession)
        .await
        .unwrap()
        .admin_data
        .unwrap();
    let devices = snap["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 2);
    assert!(devices.iter().any(|d| d["label"] == "mac-dev" && d["os"] == "macos"));
    assert!(devices.iter().any(|d| d["label"] == "Pixel-8" && d["os"] == "android"));
    let orig_mac_id = devices
        .iter()
        .find(|d| d["label"] == "mac-dev")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Phone publishes a service; Mac renames itself and customizes remark on Phone's service.
    let phone_share = phone
        .execute(Command::Save {
            id: None,
            name: "NAS".into(),
            ips: "192.0.2.30".into(),
            ports: "80".into(),
            http: true,
            kind: Some(fesen_proto::ServiceKind::Http),
            host_os: None,
            republish: false,
        })
        .await
        .unwrap()
        .shares[0]
        .id
        .clone();
    phone
        .execute(Command::Share {
            id: phone_share.clone(),
            enabled: true,
        })
        .await
        .unwrap();
    eventually(|| mac.status().services.len() == 1).await;

    mac.execute(Command::RenameDevice {
        label: "Office-Mac".into(),
    })
    .await
    .unwrap();
    assert_eq!(mac.status().device_name, "Office-Mac");

    let fwd_port = mac
        .execute(Command::SuggestLocalPort { preferred: None })
        .await
        .unwrap()
        .suggested_port
        .unwrap();
    mac.execute(Command::SetLocalForward {
        service: phone_share.clone(),
        target_ip: "192.0.2.30".parse().unwrap(),
        target_port: 80,
        local_port: fwd_port,
    })
    .await
    .unwrap();

    mac.execute(Command::Customize {
        id: phone_share.clone(),
        remark: "Office NAS".into(),
        alias: String::new(),
        ip: None,
    })
    .await
    .unwrap();

    // Multi-device pref sync: Phone receives the remark update in real time.
    eventually(|| {
        phone
            .status()
            .labels
            .iter()
            .any(|l| l.service == phone_share && l.remark == "Office NAS")
    })
    .await;

    // Mac also creates a local share rule (kept OFF) so cloud backup holds 1 share + 1 forward.
    let mac_share = mac
        .execute(Command::Save {
            id: None,
            name: "Mac-SSH".into(),
            ips: "192.0.2.40".into(),
            ports: "22".into(),
            http: false,
            kind: Some(fesen_proto::ServiceKind::Ssh),
            host_os: Some(fesen_proto::HostOs::Linux),
            republish: false,
        })
        .await
        .unwrap()
        .shares[0]
        .id
        .clone();
    eventually(|| {
        mac.status()
            .cloud_profiles
            .iter()
            .any(|p| p.machine_id == mac.status().machine_id && p.shares.len() == 1 && p.forwards.len() == 1)
    })
    .await;

    mac.stop();
    mac.wait_stopped().await;

    // Simulate clean reinstall on the SAME Mac hardware (brand new empty data dir, same machine_id).
    let reinstalled = Core::new(dir.path().join("mac_reinstalled"), mac_adapter, false).unwrap();
    reinstalled
        .execute(Command::Join {
            address: base,
        })
        .await
        .unwrap();
    reinstalled
        .execute(Command::Login {
            username: "cloud".into(),
            password: PASSWORD.into(),
            label: String::new(),
        })
        .await
        .unwrap();
    eventually(|| {
        reinstalled.status().online
            && reinstalled.status().restore_prompt.is_some()
            && reinstalled
                .status()
                .labels
                .iter()
                .any(|l| l.service == phone_share && l.remark == "Office NAS")
    })
    .await;

    // Same physical machine_id reuses the existing device row instead of duplicating devices.
    let snap2 = reinstalled
        .execute(Command::AdminSession)
        .await
        .unwrap()
        .admin_data
        .unwrap();
    let devices2 = snap2["devices"].as_array().unwrap();
    assert_eq!(devices2.len(), 2);
    assert!(devices2.iter().any(|d| d["id"] == orig_mac_id));

    // Importing the matched cloud profile restores both shares (OFF by default) and 127 forwards.
    let machine_id = reinstalled.status().machine_id;
    let restored = reinstalled
        .execute(Command::ImportProfile {
            machine_id,
            import_shares: true,
            import_forwards: true,
        })
        .await
        .unwrap();
    assert!(restored.restore_prompt.is_none());
    assert_eq!(restored.shares.len(), 1);
    assert_eq!(restored.shares[0].id, mac_share);
    assert!(restored.active.is_empty(), "restored shares must start OFF");
    assert!(restored.forwards.iter().any(|f| f.service == phone_share && f.local_port == fwd_port));

    for core in [reinstalled, phone] {
        core.stop();
        core.wait_stopped().await;
    }
    web.abort();
}
