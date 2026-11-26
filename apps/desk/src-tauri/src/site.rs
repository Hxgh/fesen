use std::net::Ipv4Addr;

#[cfg(any(windows, target_os = "macos", target_os = "android"))]
#[derive(Clone, Copy, PartialEq)]
enum Action {
    Copy,
    Command,
    Browser,
    Site,
}

#[cfg(any(windows, target_os = "macos", target_os = "android"))]
fn address(
    state: &fesen_core::Status,
    id: &str,
    owner: Option<&str>,
    ip: Ipv4Addr,
    port: u16,
    action: Action,
) -> Result<String, &'static str> {
    use fesen_core::ServiceKind;

    if state.member.is_none() {
        return Err("not_connected");
    }
    let (site, target, port) = if let Some(owner) = owner {
        if !state.online || !state.running {
            return Err("not_connected");
        }
        let site = &state
            .services
            .iter()
            .find(|s| s.owner == owner && s.service.id == id)
            .ok_or("offline")?
            .service;
        if !site.rules.allows(ip, port) {
            return Err("unauthorized");
        }
        if let Some(fwd) = state
            .forwards
            .iter()
            .find(|f| f.service == id && f.target_ip == ip && f.target_port == port)
        {
            if !fwd.active {
                return Err("local_port_conflict");
            }
            (site, Ipv4Addr::LOCALHOST, fwd.local_port)
        } else {
            if !state.system_routing {
                return Err("routing_unavailable");
            }
            if !state.routes_ready {
                return Err("route_missing");
            }
            let mapped = state
                .service_virtual_ips
                .get(id)
                .map(|m| fesen_core::routing::access_ip(m, ip))
                .filter(|&v| v != ip)
                .unwrap_or_else(|| fesen_core::routing::access_ip(&state.virtual_ips, ip));
            (site, mapped, port)
        }
    } else {
        let site = state.shares.iter().find(|s| s.id == id).ok_or("offline")?;
        if !site.rules.allows(ip, port) {
            return Err("unauthorized");
        }
        (site, ip, port)
    };
    let kind = site.effective_kind();
    let allowed = match action {
        Action::Copy | Action::Command => true,
        Action::Browser => matches!(kind, ServiceKind::Http | ServiceKind::Https),
        Action::Site => kind == ServiceKind::Http,
    };
    if !allowed {
        return Err("unauthorized");
    }
    Ok(match action {
        Action::Site => format!("http://{target}:{port}/"),
        Action::Copy | Action::Browser => match kind {
            ServiceKind::Http => format!("http://{target}:{port}/"),
            ServiceKind::Https => format!("https://{target}:{port}/"),
            _ => format!("{target}:{port}"),
        },
        Action::Command => match kind {
            ServiceKind::Ssh => {
                if port == 22 {
                    format!("ssh {target}")
                } else {
                    format!("ssh -p {port} {target}")
                }
            }
            ServiceKind::Rdp => {
                #[cfg(windows)]
                {
                    format!("mstsc /v:{target}:{port}")
                }
                #[cfg(not(windows))]
                {
                    format!("{target}:{port}")
                }
            }
            ServiceKind::Smb => {
                #[cfg(windows)]
                {
                    format!("\\\\{target}")
                }
                #[cfg(not(windows))]
                {
                    if port == 445 {
                        format!("smb://{target}")
                    } else {
                        format!("smb://{target}:{port}")
                    }
                }
            }
            ServiceKind::Http => format!("http://{target}:{port}/"),
            ServiceKind::Https => format!("https://{target}:{port}/"),
            ServiceKind::Tcp => format!("{target}:{port}"),
        },
    })
}

#[cfg(any(windows, target_os = "macos", target_os = "android"))]
async fn current_address(
    app: &tauri::AppHandle,
    id: &str,
    owner: Option<&str>,
    ip: Ipv4Addr,
    port: u16,
    action: Action,
) -> Result<String, String> {
    let mut reply = super::call(app, serde_json::json!({"op":"status"}), false).await?;
    let data = reply.get_mut("data").ok_or("not_connected")?.take();
    let state = serde_json::from_value(data).map_err(|_| "not_connected")?;
    Ok(address(&state, id, owner, ip, port, action)?)
}

#[tauri::command]
pub async fn service_action(
    window: tauri::Webview,
    id: String,
    owner: Option<String>,
    ip: Ipv4Addr,
    port: u16,
    action: String,
) -> Result<String, String> {
    if window.label() != "main" {
        return Err("unauthorized".into());
    }
    #[cfg(any(windows, target_os = "macos", target_os = "android"))]
    {
        use tauri::Manager;
        use tauri_plugin_clipboard_manager::ClipboardExt;
        use tauri_plugin_opener::OpenerExt;
        let action = match action.as_str() {
            "copy" => Action::Copy,
            "command" => Action::Command,
            "browser" => Action::Browser,
            _ => return Err("invalid".into()),
        };
        let app = window.app_handle();
        let value = current_address(app, &id, owner.as_deref(), ip, port, action).await?;
        match action {
            Action::Copy | Action::Command => app
                .clipboard()
                .write_text(value.as_str())
                .map_err(|_| "clipboard_failed")?,
            Action::Browser => app
                .opener()
                .open_url(value.as_str(), None::<&str>)
                .map_err(|_| "external_browser_failed")?,
            Action::Site => unreachable!(),
        }
        Ok(value)
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "android")))]
    {
        let _ = (id, owner, ip, port, action);
        Err("not_connected".into())
    }
}

#[tauri::command]
pub async fn open_site(
    window: tauri::Webview,
    id: String,
    owner: Option<String>,
    ip: Ipv4Addr,
    port: u16,
    attempt: u64,
    bounds: [i32; 4],
) -> Result<String, String> {
    if window.label() != "main" {
        return Err("unauthorized".into());
    }
    #[cfg(any(windows, target_os = "macos", target_os = "android"))]
    {
        use tauri::Manager;
        let app = window.app_handle();
        let url = current_address(app, &id, owner.as_deref(), ip, port, Action::Site).await?;
        #[cfg(target_os = "macos")]
        if owner.is_none() {
            let probe = super::call(
                app,
                serde_json::json!({"op":"test_local_site","id":id,"ip":ip,"port":port}),
                false,
            )
            .await?;
            if let Some(error) = probe["error"].as_str() {
                return Err(if error == "target_unavailable" {
                    "publisher_target_unavailable"
                } else {
                    error
                }
                .into());
            }
            match probe["data"]["http_status"].as_u64() {
                Some(200..=399) => {}
                Some(status) => return Err(format!("HTTP {status}")),
                None => return Err("http_invalid".into()),
            }
        }
        let closed = CLOSED.lock().await;
        if attempt <= *closed || !valid_bounds(bounds) {
            return Err("cancelled".into());
        }
        #[cfg(target_os = "android")]
        tauri_plugin_fesen::site(
            app,
            serde_json::json!({"op":"open","attempt":attempt,
            "bounds":bounds,"url":url,"group":owner.is_some() && !url.starts_with("http://127.0.0.1:")}),
        )
        .await?;
        #[cfg(any(windows, target_os = "macos"))]
        desktop(&window, &url, attempt, bounds)?;
        Ok(url)
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "android")))]
    {
        let _ = (id, owner, ip, port, attempt, bounds);
        Err("browser_failed".into())
    }
}

static CLOSED: tokio::sync::Mutex<u64> = tokio::sync::Mutex::const_new(0);
const VISIBLE: &str = include_str!("site-visible.js");
fn valid_bounds([x, y, w, h]: [i32; 4]) -> bool {
    x >= 0 && y >= 0 && (1..=32768).contains(&w) && (1..=32768).contains(&h)
}

#[cfg(any(windows, target_os = "macos"))]
fn child_top(main: &tauri::Webview, y: i32) -> i32 {
    // AppKit's child view is positioned in the titlebar-inclusive parent, unlike the DOM.
    #[cfg(target_os = "macos")]
    return y.saturating_add(super::macos::site_viewport_top(main));
    #[cfg(windows)]
    {
        let _ = main;
        y
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn desktop(
    main: &tauri::Webview,
    url: &str,
    attempt: u64,
    [x, y, w, h]: [i32; 4],
) -> Result<(), String> {
    use tauri::{
        PhysicalPosition, PhysicalSize, WebviewBuilder, WebviewUrl,
        webview::{NewWindowResponse, PermissionResponse},
    };
    let page = tauri::Url::parse(url).map_err(|_| "invalid")?;
    let origin = page.origin();
    let builder = WebviewBuilder::new(format!("site-{attempt}"), WebviewUrl::External(page))
        .incognito(true)
        .on_navigation(move |url| {
            url.origin() == origin && url.username().is_empty() && url.password().is_none()
        })
        .on_new_window(|_, _| NewWindowResponse::Deny)
        .on_permission_request(|_, _| PermissionResponse::Deny)
        .on_download(|_, _| false)
        .disable_drag_drop_handler()
        .devtools(false);
    main.window()
        .add_child(
            builder,
            PhysicalPosition::new(x, child_top(main, y)),
            PhysicalSize::new(w as u32, h as u32),
        )
        .map_err(|_| "browser_failed")?;
    Ok(())
}

#[tauri::command]
pub async fn site_view(
    window: tauri::Webview,
    app: tauri::AppHandle,
    op: String,
    attempt: u64,
    bounds: Option<[i32; 4]>,
    url: Option<String>,
) -> Result<String, String> {
    if window.label() != "main" {
        return Err("unauthorized".into());
    }
    let mut closed = CLOSED.lock().await;
    match op.as_str() {
        "close" => *closed = (*closed).max(attempt),
        "inspect" | "move" if attempt <= *closed => return Err("cancelled".into()),
        "inspect" => {}
        "move" if bounds.is_some_and(valid_bounds) => {}
        _ => return Err("invalid".into()),
    }
    let script = format!(
        "({VISIBLE})({})",
        serde_json::to_string(&url.unwrap_or_default()).unwrap()
    );
    // Never hold the lifecycle lock waiting for a renderer.
    drop(closed);
    #[cfg(target_os = "android")]
    return tauri_plugin_fesen::site(
        &app,
        serde_json::json!({"op":op,"attempt":attempt,"bounds":bounds,"script":script}),
    )
    .await;
    #[cfg(any(windows, target_os = "macos"))]
    {
        use tauri::{Manager, PhysicalPosition, PhysicalSize, Rect};
        let Some(view) = app.get_webview(&format!("site-{attempt}")) else {
            return Ok("closed".into());
        };
        match op.as_str() {
            "close" => view.close().map_err(|_| "browser_failed")?,
            "move" => {
                let [x, y, w, h] = bounds.unwrap();
                view.set_bounds(Rect {
                    position: PhysicalPosition::new(x, child_top(&window, y)).into(),
                    size: PhysicalSize::new(w as u32, h as u32).into(),
                })
                .map_err(|_| "browser_failed")?;
            }
            _ => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let tx = std::sync::Mutex::new(Some(tx));
                view.eval_with_callback(script, move |v| {
                    if let Some(tx) = tx.lock().unwrap().take() {
                        let _ = tx.send(v);
                    }
                })
                .map_err(|_| "browser_failed")?;
                let result = tokio::time::timeout(std::time::Duration::from_secs(2), rx)
                    .await
                    .ok()
                    .and_then(Result::ok);
                return Ok(result
                    .and_then(|s| serde_json::from_str::<String>(&s).ok())
                    .unwrap_or_else(|| "loading".into()));
            }
        }
        Ok("ok".into())
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "android")))]
    {
        let _ = (app, script);
        Err("browser_failed".into())
    }
}

#[cfg(all(test, any(windows, target_os = "macos", target_os = "android")))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn service_actions_use_only_the_selected_share_and_its_current_route() {
        let site = json!({"id":"site","name":"website","http":true,"rules":{
            "ips":[{"start":"198.51.100.10","end":"198.51.100.11"}],
            "ports":[{"start":8080,"end":8082}]
        }});
        let mut offered = site.clone();
        offered["owner"] = json!("other");
        offered["member"] = json!("member");
        let mut state: fesen_core::Status = serde_json::from_value(json!({
            "server":"https://example.invalid","member":"member","online":true,"running":true,
            "background":false,"system_routing":true,"routes_ready":true,"services":[offered],"shares":[site],
            "active":[],"labels":[],"aliases":{},"virtual_ips":[],"error":null
        }))
        .unwrap();
        let target: Ipv4Addr = "198.51.100.11".parse().unwrap();
        let remote = Some("other");
        for action in [Action::Copy, Action::Browser] {
            assert_eq!(
                address(&state, "site", remote, target, 8081, action).unwrap(),
                "http://198.51.100.11:8081/"
            );
        }
        state.virtual_ips = serde_json::from_value(
            json!([{"original":{"start":"198.51.100.10","end":"198.51.100.11"},
            "virtual_ip":{"start":"198.18.0.8","end":"198.18.0.9"}}]),
        )
        .unwrap();
        for action in [Action::Copy, Action::Browser] {
            assert_eq!(
                address(&state, "site", remote, target, 8081, action).unwrap(),
                "http://198.18.0.9:8081/"
            );
            assert_eq!(
                address(&state, "site", Some("wrong"), target, 8081, action),
                Err("offline")
            );
            assert_eq!(
                address(&state, "site", remote, target, 8083, action),
                Err("unauthorized")
            );
            assert_eq!(
                address(&state, "site", remote, Ipv4Addr::LOCALHOST, 8081, action),
                Err("unauthorized")
            );
        }
        state.services[0].service.http = false;
        state.services[0].service.kind = Some(fesen_core::ServiceKind::Https);
        assert_eq!(
            address(&state, "site", remote, target, 8081, Action::Copy).unwrap(),
            "https://198.18.0.9:8081/"
        );
        assert_eq!(
            address(&state, "site", remote, target, 8081, Action::Browser).unwrap(),
            "https://198.18.0.9:8081/"
        );
        assert_eq!(
            address(&state, "site", remote, target, 8081, Action::Site),
            Err("unauthorized")
        );
        state.services[0].service.kind = Some(fesen_core::ServiceKind::Ssh);
        assert_eq!(
            address(&state, "site", remote, target, 8081, Action::Command).unwrap(),
            "ssh -p 8081 198.18.0.9"
        );
        state.services[0].service.kind = None;
        assert_eq!(
            address(&state, "site", remote, target, 8081, Action::Copy).unwrap(),
            "198.18.0.9:8081"
        );
        assert_eq!(
            address(&state, "site", remote, target, 8081, Action::Browser),
            Err("unauthorized")
        );
        state.routes_ready = false;
        state.virtual_ips.clear();
        for action in [Action::Copy, Action::Browser] {
            assert_eq!(
                address(&state, "site", remote, target, 8081, action),
                Err("route_missing")
            );
        }
        state.system_routing = false;
        assert_eq!(
            address(&state, "site", remote, target, 8081, Action::Browser),
            Err("routing_unavailable")
        );
        state.online = false;
        assert_eq!(
            address(&state, "site", remote, target, 8081, Action::Copy),
            Err("not_connected")
        );
        for action in [Action::Copy, Action::Browser] {
            assert_eq!(
                address(&state, "site", None, target, 8081, action).unwrap(),
                "http://198.51.100.11:8081/"
            );
        }
        state.member = None;
        assert_eq!(
            address(&state, "site", None, target, 8081, Action::Copy),
            Err("not_connected")
        );
    }
}
