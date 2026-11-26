use super::*;

#[tokio::test]
async fn admin_entry_loads_page_and_assets() {
    let dir = tempfile::tempdir().unwrap();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", http.local_addr().unwrap());
    let db = store::db(&dir.path().join("db")).unwrap();
    let app = App::new(db, origin(&base).unwrap(), 0, None).unwrap();
    let web = tokio::spawn(async move { axum::serve(http, router(app)).await.unwrap() });
    let client = Client::new();
    for path in ["", "admin", "admin/"] {
        let page = client.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(page.url().path(), "/admin/");
        assert_eq!(page.status(), StatusCode::OK);
        assert!(page.headers().contains_key(header::CONTENT_SECURITY_POLICY));
    }
    // Non-canonical Host (e.g. apex domain vs www) redirects to canonical origin /admin/ before login.
    let no_redirect = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let alt_host = no_redirect
        .get(format!("{base}admin/"))
        .header(header::HOST, "example.com:8866")
        .send()
        .await
        .unwrap();
    assert_eq!(alt_host.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        alt_host.headers()[header::LOCATION],
        format!("{base}admin/")
    );

    // HTTP/2 can send multiple Cookie header fields (other site cookies first, fesen_admin later).
    let admin_hash = store::password_hash(PASSWORD).unwrap();
    let app2 = App::new(
        {
            let db = store::db(&dir.path().join("db2")).unwrap();
            store::create(&db, "admin", &admin_hash, true).unwrap();
            db
        },
        origin(&base).unwrap(),
        0,
        None,
    )
    .unwrap();
    let http2 = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base2 = format!("http://{}/", http2.local_addr().unwrap());
    let web2 = tokio::spawn(async move { axum::serve(http2, router(app2)).await.unwrap() });
    let login = client
        .post(format!("{base2}admin/login"))
        .header("X-Fesen-Admin", "1")
        .json(&json!({"username": "admin", "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::NO_CONTENT);
    let set_cookie = login.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let multi = client
        .get(format!("{base2}admin/session"))
        .header(header::COOKIE, "other_app_cookie=abc; fesen_admin=0000000000000000000000000000000000000000000000000000000000000000")
        .header(header::COOKIE, &set_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(multi.status(), StatusCode::OK);
    web2.abort();
    for path in [
        "admin/style.css",
        "admin/app.js",
        "admin/icons.js",
        "admin/theme.js",
        "admin/i18n.js",
        "admin/popover.js",
        "admin/host-card.js",
        "admin/settings.js",
        "admin/admin-view.js",
    ] {
        let asset = client.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(asset.status(), StatusCode::OK, "{path}");
        let etag = asset
            .headers()
            .get(header::ETAG)
            .expect("asset etag")
            .to_str()
            .unwrap()
            .to_owned();
        let cached = client
            .get(format!("{base}{path}"))
            .header(header::IF_NONE_MATCH, etag)
            .send()
            .await
            .unwrap();
        assert_eq!(cached.status(), StatusCode::NOT_MODIFIED, "{path}");
    }
    for (preferred, expected, text) in [
        ("zh-CN,en;q=0.8", "zh-CN", "接入组"),
        ("en-US,zh-CN;q=0.8", "en", "Open Fesen to join"),
    ] {
        let response = client
            .get(format!("{base}join"))
            .header(header::ACCEPT_LANGUAGE, preferred)
            .send()
            .await
            .unwrap();
        assert_eq!(response.headers()[header::CONTENT_LANGUAGE], expected);
        assert!(response.text().await.unwrap().contains(text));
    }
    web.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_core_admin_bridge_follows_single_account_role() {
    let dir = tempfile::tempdir().unwrap();
    let db = store::db(&dir.path().join("db")).unwrap();
    let hash = store::password_hash(PASSWORD).unwrap();
    store::create(&db, "boss", &hash, true).unwrap();
    store::create(&db, "member", &hash, false).unwrap();

    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = http.local_addr().unwrap();
    let base = origin(&format!("http://{server}")).unwrap();
    let app = App::new(db, base.clone(), 0, None).unwrap();
    let web = tokio::spawn(axum::serve(http, router(app)).into_future());

    // 1. Admin account unlocks admin_ready directly on normal device login (single login).
    let boss_net = Adapter::new("boss", Ipv4Addr::LOCALHOST, None);
    let boss = Core::new(dir.path().join("boss"), boss_net, false).unwrap();
    boss.execute(Command::Join {
        address: base.to_string(),
    })
    .await
    .unwrap();
    let s = boss
        .execute(Command::Login {
            username: "boss".into(),
            password: PASSWORD.into(),
            label: "BossMac".into(),
        })
        .await
        .unwrap();
    assert!(s.is_admin && s.admin_ready);
    let device_id = s.device_id.clone().expect("device_id");
    let snap = boss.execute(Command::AdminSession).await.unwrap();
    let data = snap.admin_data.expect("admin_data");
    assert_eq!(data["admin"], "boss");
    assert_eq!(data["users"].as_array().unwrap().len(), 2);
    assert!(
        data["devices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["id"] == device_id && d["machine_id"] == s.machine_id)
    );

    // Create a new member and confirm self-demote is rejected.
    boss.execute(Command::AdminAction {
        action: json!({"op": "create", "username": "rookie", "password": PASSWORD}),
    })
    .await
    .unwrap();
    let snap2 = boss.execute(Command::AdminSession).await.unwrap();
    assert_eq!(snap2.admin_data.unwrap()["users"].as_array().unwrap().len(), 3);
    assert_eq!(
        boss.execute(Command::AdminAction {
            action: json!({"op": "set_admin", "username": "boss", "admin": false}),
        })
        .await
        .err(),
        Some(Code::Invalid)
    );

    // 2. Normal member has no admin module; once promoted, resuming its own session unlocks admin directly.
    let mem_net = Adapter::new("mem", Ipv4Addr::LOCALHOST, None);
    let mem = Core::new(dir.path().join("mem"), mem_net, false).unwrap();
    mem.execute(Command::Join {
        address: base.to_string(),
    })
    .await
    .unwrap();
    let ms = mem
        .execute(Command::Login {
            username: "member".into(),
            password: PASSWORD.into(),
            label: "MemMac".into(),
        })
        .await
        .unwrap();
    assert!(!ms.is_admin && !ms.admin_ready);
    assert_eq!(
        mem.execute(Command::AdminSession).await.err(),
        Some(Code::Unauthorized)
    );

    boss.execute(Command::AdminAction {
        action: json!({"op": "set_admin", "username": "member", "admin": true}),
    })
    .await
    .unwrap();
    mem.execute(Command::Stop).await.unwrap();
    let ms2 = mem.execute(Command::Start).await.unwrap();
    assert!(ms2.is_admin && ms2.admin_ready);
    assert_eq!(
        mem.execute(Command::AdminSession)
            .await
            .unwrap()
            .admin_data
            .unwrap()["admin"],
        "member"
    );

    // Demoting revokes admin_sessions immediately and clears role on resume.
    boss.execute(Command::AdminAction {
        action: json!({"op": "set_admin", "username": "member", "admin": false}),
    })
    .await
    .unwrap();
    assert_eq!(
        mem.execute(Command::AdminSession).await.err(),
        Some(Code::Unauthorized)
    );
    mem.execute(Command::Stop).await.unwrap();
    let ms3 = mem.execute(Command::Start).await.unwrap();
    assert!(!ms3.is_admin && !ms3.admin_ready);

    for core in [boss, mem] {
        core.stop();
        core.wait_stopped().await;
    }
    web.abort();
}
