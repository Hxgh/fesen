mod admin;
mod group;
mod store;
#[cfg(test)]
mod tests;
mod tls;

use axum::{
    Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use group::App;
use fesen_proto::{
    Code, Credential, Event, HEARTBEAT, Hello, MAX_REQUEST, PRESENCE_TIMEOUT, PROBE_PREFIX,
    PROTOCOL, Request, origin, stun_endpoint, valid_label, valid_username,
};
use std::{
    env,
    net::{SocketAddr, SocketAddrV4},
    path::PathBuf,
    sync::atomic::Ordering,
    time::Duration,
};
use tokio::{
    net::{TcpListener, UdpSocket},
    sync::mpsc,
    time::{Instant, interval, timeout},
};

struct Discovery {
    listen: Option<SocketAddrV4>,
    public_port: Option<u16>,
    stun: Option<String>,
}

impl Discovery {
    fn parse(listen: &str, public_port: &str, stun: &str) -> anyhow::Result<Self> {
        let listen = (!listen.is_empty()).then(|| listen.parse()).transpose()?;
        let public_port = (!public_port.is_empty())
            .then(|| public_port.parse::<u16>())
            .transpose()?;
        anyhow::ensure!(
            public_port.is_none() || (listen.is_some() && public_port != Some(0)),
            "FESEN_PUBLIC_UDP_PORT 需要内置 UDP 监听和非零端口"
        );
        let stun = (!stun.is_empty()).then(|| stun.to_owned());
        if let Some(stun) = &stun {
            anyhow::ensure!(
                stun_endpoint(stun).is_some(),
                "FESEN_STUN 应为域名或 IPv4:端口"
            );
            anyhow::ensure!(listen.is_none(), "内置 UDP 探测与 STUN 只能选一种");
        }
        Ok(Self {
            listen,
            public_port,
            stun,
        })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let path =
        PathBuf::from(env::var_os("FESEN_DB").unwrap_or_else(|| "fesen.sqlite".into()));
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("admin") => {
            let name = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("用法: fesen-server admin 账号"))?;
            anyhow::ensure!(args.next().is_none() && valid_username(&name), "账号无效");
            let password = if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                rpassword::prompt_password("管理员密码: ")?
            } else {
                let mut s = String::new();
                std::io::stdin().read_line(&mut s)?;
                s.trim_end_matches(['\r', '\n']).to_owned()
            };
            let hash = store::password_hash(&password)?;
            store::create(&store::db(&path)?, &name, &hash, true)?;
            println!("管理员账号已创建");
            return Ok(());
        }
        Some("serve") | None => anyhow::ensure!(
            args.next().is_none(),
            "用法: fesen-server [serve | admin 账号]"
        ),
        _ => anyhow::bail!("用法: fesen-server [serve | admin 账号]"),
    }
    let origin =
        origin(&env::var("FESEN_ORIGIN").unwrap_or_else(|_| "http://127.0.0.1:8080".into()))?;
    let listen: SocketAddr = env::var("FESEN_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()?;
    let config = Discovery::parse(
        &env::var("FESEN_UDP").unwrap_or_default(),
        &env::var("FESEN_PUBLIC_UDP_PORT").unwrap_or_default(),
        &env::var("FESEN_STUN").unwrap_or_default(),
    )?;
    anyhow::ensure!(
        origin.scheme() == "https" || listen.ip().is_loopback(),
        "明文 HTTP 只允许监听环回地址"
    );
    let tls = tls::Tls::load(
        env::var_os("FESEN_TLS_CERT").map(PathBuf::from),
        env::var_os("FESEN_TLS_KEY").map(PathBuf::from),
        origin.scheme() == "https",
    )
    .await?;
    let tcp = TcpListener::bind(listen).await?;
    let udp = match config.listen {
        Some(listen) => Some(UdpSocket::bind(listen).await?),
        None => None,
    };
    let port = match &udp {
        Some(udp) => config.public_port.unwrap_or(udp.local_addr()?.port()),
        None => 0,
    };
    let app = App::new(store::db(&path)?, origin, port, config.stun)?;
    println!(
        "fesen-server {} {}，探测 {}",
        if tls.is_some() { "HTTPS" } else { "HTTP" },
        tcp.local_addr()?,
        match (&udp, &app.stun) {
            (Some(udp), _) => format!("UDP {}", udp.local_addr()?),
            (None, Some(_)) => "STUN".into(),
            (None, None) => "关闭".into(),
        }
    );
    let web = router(app.clone());
    tokio::select! {
        r = async move { match tls {
            Some(tls) => tls.serve(tcp, web).await,
            None => axum::serve(tcp, web).await,
        } } => r?,
        r = async move { match udp {
            Some(udp) => discovery(udp, app).await,
            None => std::future::pending().await,
        } } => r?,
        () = shutdown() => {},
    }
    Ok(())
}

async fn shutdown() {
    #[cfg(unix)]
    if let Ok(mut terminate) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    {
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
        return;
    }
    let _ = tokio::signal::ctrl_c().await;
}

fn router(app: App) -> Router {
    let admin_entry = get(|| async { axum::response::Redirect::temporary("/admin/") });
    Router::new()
        .route("/", admin_entry.clone())
        .route("/admin", admin_entry)
        .route("/healthz", get(|| async { "ok" }))
        .route("/join", get(join))
        .route("/control", get(upgrade))
        .nest("/admin/", admin::router())
        .with_state(app)
}

async fn join(State(app): State<App>, headers: HeaderMap) -> impl IntoResponse {
    let mut link = url::Url::parse("fesen://join").unwrap();
    link.query_pairs_mut()
        .append_pair("server", app.origin.as_str());
    let language = headers
        .get(header::ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.split(',')
                .filter_map(|item| item.trim().split(['-', ';']).next())
                .find(|item| item.eq_ignore_ascii_case("zh") || item.eq_ignore_ascii_case("en"))
        });
    let (lang, brand, heading, label, link_label) =
        if language.is_some_and(|v| v.eq_ignore_ascii_case("zh")) {
            ("zh-CN", "飞弦", "接入组", "打开飞弦接入组", "接入链接")
        } else {
            (
                "en",
                "Fesen",
                "Join your group",
                "Open Fesen to join",
                "Join link",
            )
        };
    let host = &app.origin[url::Position::BeforeHost..url::Position::AfterPort];
    let mark = r#"<svg class="brand-mark" data-state="active" viewBox="0 0 64 64" fill="currentColor" aria-hidden="true"><g class="yao-top"><rect x="4" y="6" width="56" height="12"/></g><g class="yao-mid"><rect x="4" y="26" width="24" height="12"/><rect x="36" y="26" width="24" height="12"/></g><g class="yao-bot"><rect x="4" y="46" width="24" height="12"/><rect x="36" y="46" width="24" height="12"/></g></svg>"#;
    (
        [
            (header::VARY, "Accept-Language"),
            (header::CONTENT_LANGUAGE, lang),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'self'; base-uri 'none'; frame-ancestors 'none'",
            ),
        ],
        Html(format!(
            r#"<!doctype html>
<html lang="{lang}"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light dark">
<title>{heading} · {brand}</title><link rel="stylesheet" href="/admin/style.css">
</head><body>
<header class="topbar"><div class="wrap topbar-inner"><div class="brand">{mark}<span>{brand}</span></div></div></header>
<main class="wrap"><section class="signin stack join-card"><h1 class="form-title">{heading}</h1><p class="muted mono">{host}</p>
<a href="{link}" class="button primary">{label}</a>
<details><summary>{link_label}</summary><textarea class="join-address" aria-label="{link_label}" readonly rows="2">{link}</textarea></details>
</section></main></body></html>"#
        )),
    )
}

async fn discovery(socket: UdpSocket, app: App) -> std::io::Result<()> {
    let mut packet = [0; 256];
    loop {
        let (len, from) = socket.recv_from(&mut packet).await?;
        if let SocketAddr::V4(from) = from
            && let Some(probe) = packet[..len].strip_prefix(PROBE_PREFIX)
            && probe.len() == 64
            && let Ok(probe) = std::str::from_utf8(probe)
        {
            app.inner.lock().unwrap().observe(probe, from);
        }
    }
}

async fn upgrade(State(app): State<App>, ws: WebSocketUpgrade) -> Response {
    ws.max_message_size(MAX_REQUEST)
        .max_frame_size(MAX_REQUEST)
        .on_upgrade(|socket| control(socket, app))
        .into_response()
}

async fn send(socket: &mut WebSocket, event: Event) -> Result<(), ()> {
    let text = serde_json::to_string(&event).map_err(|_| ())?;
    timeout(
        Duration::from_secs(4),
        socket.send(Message::Text(text.into())),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())
}

async fn control(mut socket: WebSocket, app: App) {
    let Ok(Some(Ok(Message::Text(data)))) = timeout(HEARTBEAT, socket.recv()).await else {
        return;
    };
    let Ok(hello) = serde_json::from_str::<Hello>(&data) else {
        return;
    };
    let mut changed = app.changed.subscribe();
    let (tx, mut rx) = mpsc::channel(24);
    let cert = &hello.certificate;
    let authorized = async {
        if hello.version != PROTOCOL || !(96..=512).contains(&cert.len()) {
            return Err(Code::Invalid);
        }
        let (login, token) = match hello.credential {
            Credential::Password {
                username,
                password,
                label,
            } => {
                if !valid_username(&username) || !valid_label(&label) {
                    return Err(Code::Invalid);
                }
                let hash = app.verify(&username, password, false).await?;
                (Some((username, label, hash)), store::secret())
            }
            Credential::Resume { token } if token.len() == 64 => (None, token),
            Credential::Resume { .. } => return Err(Code::Unauthorized),
        };
        let mut g = app.inner.lock().unwrap();
        if let Some((username, _, hash)) = &login {
            if store::login_hash(&g.db, username, false).as_ref() != Some(hash) {
                return Err(Code::Unauthorized);
            }
            match store::device_owner(&g.db, cert)? {
                Some((member, false)) if &member == username => {}
                Some(_) => return Err(Code::Unauthorized),
                None => {}
            }
        }
        let (id, member, label, os) = store::authorize_device(
            &g.db,
            cert,
            login.as_ref().map(|(u, l, _)| (u.as_str(), l.as_str())),
            hello.machine_id.as_deref(),
            hello.os,
            &token,
        )?;
        let is_admin = store::is_admin(&g.db, &member);
        let admin_token = if is_admin {
            let t = store::secret();
            store::admin_cookie(&g.db, &member, &t)?;
            Some(t)
        } else {
            None
        };
        let prefs = store::list_prefs(&g.db, &member)?;
        let profiles = store::list_profiles(&g.db, &member)?;
        let session = g.connect(&id, member.clone(), label, os, cert.clone(), tx)?;
        Ok((id, member, login.map(|_| token), is_admin, admin_token, prefs, profiles, session))
    }
    .await;
    let (id, member, token, is_admin, admin_token, prefs, profiles, session) = match authorized {
        Ok(v) => v,
        Err(error) => {
            let _ = send(&mut socket, Event::Rejected { error }).await;
            return;
        }
    };
    app.changed();
    let _ = async {
        send(&mut socket, Event::Welcome {
            device: id.clone(), member, token, probe: session.probe.clone(),
            udp_port: app.udp_port, stun: app.stun.clone(), is_admin, admin_token,
        }).await?;
        send(&mut socket, Event::Sync { prefs, profiles }).await?;
        let mut tick = interval(HEARTBEAT);
        let mut last = Instant::now();
        let mut allowance = 128u16;
        loop {
            tokio::select! {
                biased;
                _ = session.cancel.cancelled() => {
                    if session.revoked.load(Ordering::Acquire) {
                        let _ = send(&mut socket, Event::Revoked).await;
                    }
                    break;
                }
                _ = tick.tick() => {
                    if last.elapsed() >= PRESENCE_TIMEOUT { break; }
                    allowance = 128;
                    changed.borrow_and_update();
                    let view = app.inner.lock().unwrap().view();
                    send(&mut socket, Event::View { view }).await?;
                }
                Ok(()) = changed.changed() => {
                    changed.borrow_and_update();
                    let view = app.inner.lock().unwrap().view();
                    send(&mut socket, Event::View { view }).await?;
                }
                Some(event) = rx.recv() => send(&mut socket, event).await?,
                data = socket.recv() => {
                    let Some(Ok(message)) = data else { break };
                    match message {
                        Message::Text(text) => {
                            let request = serde_json::from_str::<Request>(&text).map_err(|_| ())?;
                            if allowance == 0 { break; }
                            allowance -= 1;
                            last = Instant::now();
                            let result = app.inner.lock().unwrap().call(&id, &session, request.call);
                            if result == Ok(true) { app.changed(); }
                            send(&mut socket, Event::Result { seq: request.seq, error: result.err() }).await?;
                        }
                        Message::Close(_) | Message::Binary(_) => break,
                        _ => {},
                    }
                }
            }
        }
        Ok::<(), ()>(())
    }.await;
    let mut g = app.inner.lock().unwrap();
    if g.current(&id, &session) {
        g.remove(&id, false);
        app.changed();
    }
}
