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

    // Create a new member and confirm single super admin cannot be duplicated or self-disabled.
    boss.execute(Command::AdminAction {
        action: json!({"op": "create", "username": "rookie", "password": PASSWORD}),
    })
    .await
    .unwrap();
    let snap2 = boss.execute(Command::AdminSession).await.unwrap();
    let users = snap2.admin_data.unwrap()["users"].as_array().unwrap().clone();
    assert_eq!(users.len(), 3);
    assert_eq!(users.iter().filter(|u| u["admin"] == true).count(), 1);
    assert_eq!(
        boss.execute(Command::AdminAction {
            action: json!({"op": "enable", "username": "boss", "enabled": false}),
        })
        .await
        .err(),
        Some(Code::Invalid)
    );

    // 2. Normal member has no admin module.
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

    for core in [boss, mem] {
        core.stop();
        core.wait_stopped().await;
    }
    web.abort();
}

#[test]
fn single_super_admin_and_bootstrap_env_are_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let db = store::db(&dir.path().join("env.sqlite")).unwrap();
    assert!(!store::has_admin(&db).unwrap());

    // Reject both password env and file set simultaneously.
    assert!(
        bootstrap_admin(
            &db,
            Some("owner"),
            Some("initial-pass-1"),
            Some(&dir.path().join("secret")),
        )
        .is_err()
    );

    // First bootstrap initializes the single super admin.
    bootstrap_admin(&db, Some("owner"), Some("initial-pass-1"), None).unwrap();
    assert!(store::has_admin(&db).unwrap());
    let first_hash = store::login_hash(&db, "owner", true).expect("owner hash");
    assert!(store::verify("initial-pass-1", &first_hash));

    // Second admin creation is rejected atomically.
    assert_eq!(
        store::create(&db, "second", &first_hash, true),
        Err(Code::Conflict)
    );

    // Re-running bootstrap with a different password does NOT overwrite existing admin.
    bootstrap_admin(&db, Some("other"), Some("different-pass-2"), None).unwrap();
    assert_eq!(store::login_hash(&db, "owner", true), Some(first_hash));
    assert!(store::login_hash(&db, "other", true).is_none());

    // Password file bootstrap works on a fresh database with default user "admin".
    let pass_file = dir.path().join("admin.pass");
    std::fs::write(&pass_file, "file-secret-123\n").unwrap();
    let db2 = store::db(&dir.path().join("file.sqlite")).unwrap();
    bootstrap_admin(&db2, None, None, Some(&pass_file)).unwrap();
    let hash2 = store::login_hash(&db2, "admin", true).expect("admin hash");
    assert!(store::verify("file-secret-123", &hash2));
}

#[tokio::test]
async fn admin_change_password_rotates_cookie_and_rate_limits_bruteforce() {
    let dir = tempfile::tempdir().unwrap();
    let db = store::db(&dir.path().join("db")).unwrap();
    let hash = store::password_hash(PASSWORD).unwrap();
    store::create(&db, "admin", &hash, true).unwrap();

    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", http.local_addr().unwrap());
    let app = App::new(db, origin(&base).unwrap(), 0, None).unwrap();
    let web = tokio::spawn(async move {
        axum::serve(
            http,
            router(app).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap()
    });
    let client = Client::new();

    // Brute-force protection: 5 failures -> 401, 6th -> 429 Busy before Argon2.
    for _ in 0..5 {
        let r = client
            .post(format!("{base}admin/login"))
            .header("X-Fesen-Admin", "1")
            .json(&json!({"username": "bruteforce-target", "password": "wrong-password-1"}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    }
    let blocked = client
        .post(format!("{base}admin/login"))
        .header("X-Fesen-Admin", "1")
        .json(&json!({"username": "bruteforce-target", "password": "wrong-password-1"}))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::TOO_MANY_REQUESTS);

    // Real admin account is unaffected and can log in cleanly.
    let login = client
        .post(format!("{base}admin/login"))
        .header("X-Fesen-Admin", "1")
        .json(&json!({"username": "admin", "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::NO_CONTENT);
    let cookie1 = login.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();

    // Wrong current password rejected.
    let bad = client
        .post(format!("{base}admin/action"))
        .header("X-Fesen-Admin", "1")
        .header(header::COOKIE, &cookie1)
        .json(&json!({"op": "change_password", "current": "wrong-current-pass", "password": "brand-new-password-9"}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);

    // Valid change_password rotates cookie and invalidates old cookie.
    let changed = client
        .post(format!("{base}admin/action"))
        .header("X-Fesen-Admin", "1")
        .header(header::COOKIE, &cookie1)
        .json(&json!({"op": "change_password", "current": PASSWORD, "password": "brand-new-password-9"}))
        .send()
        .await
        .unwrap();
    assert_eq!(changed.status(), StatusCode::NO_CONTENT);
    let cookie2 = changed.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    assert_ne!(cookie1, cookie2);

    assert_eq!(
        client
            .get(format!("{base}admin/session"))
            .header(header::COOKIE, &cookie1)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(format!("{base}admin/session"))
            .header(header::COOKIE, &cookie2)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    // Re-login works with new password and fails with old password.
    assert_eq!(
        client
            .post(format!("{base}admin/login"))
            .header("X-Fesen-Admin", "1")
            .json(&json!({"username": "admin", "password": "brand-new-password-9"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    web.abort();
}
