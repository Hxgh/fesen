use crate::{
    group::{App, PeerIp},
    store,
};
use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, Utf8Bytes, WebSocket},
    },
    http::{HeaderMap, HeaderValue, header},
    response::{Html, IntoResponse, Response},
};
use fesen_proto::{
    Code, Credential, Event, HEARTBEAT, Hello, MAX_REQUEST, PRESENCE_TIMEOUT, PROBE_PREFIX,
    PROTOCOL, Request, valid_label, valid_username,
};
use std::{
    net::{IpAddr, SocketAddr},
    sync::atomic::Ordering,
    time::Duration,
};
use tokio::{
    net::UdpSocket,
    sync::mpsc,
    time::{Instant, interval, timeout},
};

pub async fn join(State(app): State<App>, headers: HeaderMap) -> impl IntoResponse {
    let link = app.join_link.as_ref();
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
    let mut res = (
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
        .into_response();
    if app.origin.scheme() == "https" {
        res.headers_mut().insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000"),
        );
    }
    res
}

pub async fn discovery(socket: UdpSocket, app: App) -> std::io::Result<()> {
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

pub async fn upgrade(
    State(app): State<App>,
    PeerIp(ip): PeerIp,
    ws: WebSocketUpgrade,
) -> Response {
    ws.max_message_size(MAX_REQUEST)
        .max_frame_size(MAX_REQUEST)
        .on_upgrade(move |socket| session_loop(socket, app, ip))
        .into_response()
}

async fn send_raw(socket: &mut WebSocket, frame: Utf8Bytes) -> Result<(), ()> {
    timeout(Duration::from_secs(4), socket.send(Message::Text(frame)))
        .await
        .map_err(|_| ())?
        .map_err(|_| ())
}

async fn send(socket: &mut WebSocket, event: Event) -> Result<(), ()> {
    let text = serde_json::to_string(&event).map_err(|_| ())?;
    send_raw(socket, text.into()).await
}

async fn session_loop(mut socket: WebSocket, app: App, ip: Option<IpAddr>) {
    let Ok(Some(Ok(Message::Text(data)))) = timeout(HEARTBEAT, socket.recv()).await else {
        return;
    };
    let Ok(hello) = serde_json::from_str::<Hello>(&data) else {
        return;
    };
    let mut changed = app.view_frame.subscribe();
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
                let hash = app.verify_limited(ip, &username, password, false).await?;
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
        Ok((
            id,
            member,
            login.map(|_| token),
            is_admin,
            admin_token,
            prefs,
            profiles,
            session,
        ))
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
        send(
            &mut socket,
            Event::Welcome {
                device: id.clone(),
                member,
                token,
                probe: session.probe.clone(),
                udp_port: app.udp_port,
                stun: app.stun.clone(),
                is_admin,
                admin_token,
            },
        )
        .await?;
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
                    let frame = changed.borrow_and_update().1.clone();
                    send_raw(&mut socket, frame).await?;
                }
                Ok(()) = changed.changed() => {
                    let frame = changed.borrow_and_update().1.clone();
                    send_raw(&mut socket, frame).await?;
                }
                Some(frame) = rx.recv() => send_raw(&mut socket, frame).await?,
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
    }
    .await;
    let removed = {
        let mut g = app.inner.lock().unwrap();
        if g.current(&id, &session) {
            g.remove(&id, false);
            true
        } else {
            false
        }
    };
    if removed {
        app.changed();
    }
}
