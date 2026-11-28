use serde_json::{Value, json};
use std::sync::Arc;
#[cfg(any(windows, target_os = "macos"))]
use std::time::Duration;

#[cfg(any(windows, target_os = "macos"))]
use tauri::Manager;

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "macos")]
mod macos;
mod site;
mod update;
#[cfg(windows)]
mod windows;

#[tauri::command]
async fn update_state(
    window: tauri::Webview,
    updater: tauri::State<'_, Arc<update::ManagerState>>,
) -> Result<update::UpdateState, String> {
    if window.label() != "main" {
        return Err("unauthorized".into());
    }
    Ok(updater.snapshot().await)
}

#[tauri::command]
async fn check_for_update(
    window: tauri::Webview,
    app: tauri::AppHandle,
    updater: tauri::State<'_, Arc<update::ManagerState>>,
    silent: Option<bool>,
) -> Result<update::UpdateState, String> {
    if window.label() != "main" {
        return Err("unauthorized".into());
    }
    Ok(update::check_for_update(&app, &updater, silent.unwrap_or(false)).await)
}

#[tauri::command]
async fn download_update(
    window: tauri::Webview,
    app: tauri::AppHandle,
    updater: tauri::State<'_, Arc<update::ManagerState>>,
) -> Result<update::UpdateState, String> {
    if window.label() != "main" {
        return Err("unauthorized".into());
    }
    update::download_update(app, updater.inner().clone()).await
}

#[tauri::command]
async fn pause_update_download(
    window: tauri::Webview,
    app: tauri::AppHandle,
    updater: tauri::State<'_, Arc<update::ManagerState>>,
) -> Result<update::UpdateState, String> {
    if window.label() != "main" {
        return Err("unauthorized".into());
    }
    Ok(update::pause_update_download(&app, &updater).await)
}

#[tauri::command]
async fn cancel_update_download(
    window: tauri::Webview,
    app: tauri::AppHandle,
    updater: tauri::State<'_, Arc<update::ManagerState>>,
) -> Result<update::UpdateState, String> {
    if window.label() != "main" {
        return Err("unauthorized".into());
    }
    Ok(update::cancel_update_download(&app, &updater).await)
}

#[tauri::command]
async fn install_update(
    window: tauri::Webview,
    app: tauri::AppHandle,
    updater: tauri::State<'_, Arc<update::ManagerState>>,
) -> Result<(), String> {
    if window.label() != "main" {
        return Err("unauthorized".into());
    }
    update::install_update(&app, &updater).await
}

#[tauri::command]
async fn open_release_url(
    window: tauri::Webview,
    app: tauri::AppHandle,
    url: String,
) -> Result<(), String> {
    if window.label() != "main" {
        return Err("unauthorized".into());
    }
    update::open_release_url(&app, &url)
}

#[tauri::command]
async fn core_command(
    window: tauri::Webview,
    app: tauri::AppHandle,
    request: Value,
    launch: bool,
) -> Value {
    if window.label() != "main" {
        return json!({ "error": "unauthorized" });
    }
    match call(&app, request, launch).await {
        Ok(result) => result,
        Err(error) => json!({ "error": error }),
    }
}

#[cfg(target_os = "macos")]
#[tauri::command]
async fn open_extension_settings(window: tauri::Webview) -> Result<(), String> {
    if window.label() != "main" {
        return Err("unauthorized".into());
    }
    macos::open_settings()
}

#[cfg(target_os = "macos")]
#[tauri::command]
async fn menu_bar_setting(
    window: tauri::Webview,
    app: tauri::AppHandle,
    enabled: Option<bool>,
) -> Result<bool, String> {
    if window.label() != "main" {
        return Err("unauthorized".into());
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let _ = tx.send(macos::menu_bar_setting(enabled));
    })
    .map_err(|_| "internal")?;
    rx.await.map_err(|_| "internal")?
}

#[cfg(target_os = "macos")]
async fn call(_app: &tauri::AppHandle, request: Value, launch: bool) -> Result<Value, String> {
    macos::request(request, launch).await
}

#[cfg(target_os = "android")]
#[tauri::command]
async fn add_quick_tile(window: tauri::Webview, app: tauri::AppHandle) -> Result<String, String> {
    if window.label() != "main" {
        return Err("unauthorized".into());
    }
    tauri_plugin_fesen::add_tile(&app).await
}

#[cfg(target_os = "android")]
async fn call(app: &tauri::AppHandle, request: Value, launch: bool) -> Result<Value, String> {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        tauri_plugin_fesen::request(&app, request, launch)
    })
    .await
    .map_err(|_| "not_connected".to_owned())?
}

#[cfg(windows)]
#[tauri::command]
async fn sync_snap_button(
    window: tauri::WebviewWindow,
    rect: windows::SnapRect,
) -> Result<(), String> {
    if window.label() != "main" {
        return Err("unauthorized".into());
    }
    windows::sync_snap_button(&window, rect)
}

#[cfg(windows)]
async fn call(_app: &tauri::AppHandle, request: Value, launch: bool) -> Result<Value, String> {
    use fesen_core::{
        ipc,
        platform::windows::{data_dir, elevate, verify_core},
    };
    static START: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let error = || String::from("not_connected");
    let dir = data_dir().map_err(|_| error())?;
    if let Ok(reply) = ipc::request(&dir, &request).await {
        return Ok(reply);
    }
    if !launch {
        return Err(error());
    }
    let _lock = START.lock().await;
    if let Ok(reply) = ipc::request(&dir, &request).await {
        return Ok(reply);
    }
    let core = std::env::current_exe()
        .map_err(|_| error())?
        .with_file_name("fesen-core.exe");
    verify_core(&core).map_err(|e| String::from(e.code()))?;
    tauri::async_runtime::spawn_blocking(move || elevate(&core))
        .await
        .map_err(|_| error())?
        .map_err(|e| String::from(e.code()))?;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(150)).await;
        if let Ok(reply) = ipc::request(&dir, &request).await {
            return Ok(reply);
        }
    }
    Err(error())
}

#[cfg(not(any(windows, target_os = "macos", target_os = "android")))]
async fn call(_app: &tauri::AppHandle, _request: Value, _launch: bool) -> Result<Value, String> {
    Err(String::from("not_connected"))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "macos")]
    if macos::native_publisher() {
        return;
    }
    let app = tauri::Builder::default();
    #[cfg(any(windows, target_os = "macos"))]
    let app = app.plugin(tauri_plugin_single_instance::init(|app, _args, _| {
        #[cfg(target_os = "macos")]
        if let [_, mode, url] = _args.as_slice()
            && mode == "--fesen-join"
            && url.starts_with("fesen://join?")
            && url.len() <= 1024
        {
            use tauri::Emitter;
            let _ = app.emit_to("main", "deep-link://new-url", [url]);
        }
        if let Some(window) = app.get_window("main") {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        }
    }));
    #[cfg(target_os = "android")]
    let app = app.plugin(tauri_plugin_fesen::init());
    let app = app
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(
            tauri_plugin_opener::Builder::new()
                .open_js_links_on_click(false)
                .build(),
        );
    #[cfg(target_os = "macos")]
    let app = app.invoke_handler(tauri::generate_handler![
        core_command,
        site::open_site,
        site::site_view,
        site::service_action,
        update_state,
        check_for_update,
        download_update,
        pause_update_download,
        cancel_update_download,
        install_update,
        open_release_url,
        open_extension_settings,
        menu_bar_setting
    ]);
    #[cfg(target_os = "android")]
    let app = app.invoke_handler(tauri::generate_handler![
        core_command,
        site::open_site,
        site::site_view,
        site::service_action,
        update_state,
        check_for_update,
        download_update,
        pause_update_download,
        cancel_update_download,
        install_update,
        open_release_url,
        add_quick_tile
    ]);
    #[cfg(windows)]
    let app = app.invoke_handler(tauri::generate_handler![
        core_command,
        site::open_site,
        site::site_view,
        site::service_action,
        update_state,
        check_for_update,
        download_update,
        pause_update_download,
        cancel_update_download,
        install_update,
        open_release_url,
        sync_snap_button
    ]);
    #[cfg(not(any(windows, target_os = "macos", target_os = "android")))]
    let app = app.invoke_handler(tauri::generate_handler![
        core_command,
        site::open_site,
        site::site_view,
        site::service_action,
        update_state,
        check_for_update,
        download_update,
        pause_update_download,
        cancel_update_download,
        install_update,
        open_release_url
    ]);
    let app = app.manage(Arc::new(update::ManagerState::new()));
    let app = app.setup(|app| {
        let cleanup_handle = app.handle().clone();
        tauri::async_runtime::spawn(async move {
            update::cleanup_stale_updates(cleanup_handle).await;
        });
        #[cfg(target_os = "macos")]
        {
            if macos::menu_bar_setting(None).unwrap_or(true) {
                macos::tray();
            } else {
                macos::close_tray();
            }
            if let Some(window) = app.get_webview_window("main") {
                macos::configure_window(&window);
            }
        }
        Ok(())
    });
    #[cfg(any(windows, target_os = "macos"))]
    let app = app.on_window_event(|window, event| {
        if window.label() != "main" {
            return;
        }
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let handle = window.app_handle().clone();
            #[cfg(target_os = "macos")]
            let keep_menu = macos::menu_bar_setting(None).unwrap_or(true);
            tauri::async_runtime::spawn(async move {
                #[cfg(target_os = "macos")]
                let op = if keep_menu { "detach" } else { "quit" };
                #[cfg(not(target_os = "macos"))]
                let op = "detach";
                let _ = tokio::time::timeout(
                    Duration::from_secs(1),
                    call(&handle, json!({"op": op}), false),
                )
                .await;
                #[cfg(target_os = "macos")]
                if keep_menu {
                    macos::tray();
                } else {
                    macos::close_tray();
                }
                handle.exit(0);
            });
        }
    });
    app.run(tauri::generate_context!())
        .expect("无法启动飞弦（Fesen）");
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    #[test]
    fn windows_window_config_matches_main_dimensions_and_uses_custom_titlebar() {
        let base: Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let win: Value = serde_json::from_str(include_str!("../tauri.windows.conf.json")).unwrap();
        let base_win = &base["app"]["windows"][0];
        let win_win = &win["app"]["windows"][0];
        for key in ["label", "width", "height", "minWidth", "minHeight", "center"] {
            assert_eq!(base_win[key], win_win[key], "mismatch on {key}");
        }
        assert_eq!(win_win["decorations"], false);
        assert_eq!(win_win["shadow"], true);
    }
}
