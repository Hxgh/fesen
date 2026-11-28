use fesen_core::{ipc, platform::macos::publisher_dir};
use serde_json::Value;
use std::time::Duration;
use tokio::sync::oneshot;

type Reply = unsafe extern "C" fn(usize, *const u8, usize);

unsafe extern "C" {
    fn ph_site_viewport_top(view: *mut std::ffi::c_void) -> i32;
    fn ph_host_configure_window(window: *mut std::ffi::c_void);
    fn ph_host_call(bytes: *const u8, count: usize, mode: i32, context: usize, callback: Reply);
    fn ph_host_open_settings() -> i32;
    fn ph_host_menu_bar_setting(value: i32) -> i32;
    fn ph_host_can_route() -> i32;
    fn ph_host_notify_status(state: i32);
    fn ph_host_tray();
    fn ph_host_close_tray();
    fn ph_host_launch_publisher(context: usize, done: unsafe extern "C" fn(usize, i32));
    fn ph_host_publisher_main();
}

pub fn configure_window(window: &tauri::WebviewWindow) {
    if let Ok(ns_window) = window.ns_window() {
        unsafe { ph_host_configure_window(ns_window) };
    }
}

// Runs on the executable's main thread, before Tauri, its WebView or the GUI singleton.
pub fn native_publisher() -> bool {
    if !std::env::args_os()
        .skip(1)
        .eq([std::ffi::OsString::from("--publisher")])
    {
        return false;
    }
    unsafe { ph_host_publisher_main() };
    true
}

unsafe extern "C" fn launched(context: usize, ok: i32) {
    let sender = unsafe { Box::from_raw(context as *mut oneshot::Sender<bool>) };
    let _ = sender.send(ok != 0);
}

pub fn site_viewport_top(main: &tauri::Webview) -> i32 {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    if main
        .with_webview(move |w| {
            let _ = tx.send(unsafe { ph_site_viewport_top(w.inner()) });
        })
        .is_err()
    {
        return 0;
    }
    rx.recv_timeout(Duration::from_secs(1)).unwrap_or(0)
}

unsafe extern "C" fn done(context: usize, bytes: *const u8, len: usize) {
    let reply = unsafe { Box::from_raw(context as *mut oneshot::Sender<Result<Value, String>>) };
    let result = if bytes.is_null() || len > 2 * 1024 * 1024 {
        Err("internal".into())
    } else {
        serde_json::from_slice(unsafe { std::slice::from_raw_parts(bytes, len) })
            .map_err(|_| "internal".into())
    };
    let _ = reply.send(result);
}

fn notify_menu_status(reply: &Result<Value, String>) {
    use std::sync::atomic::{AtomicI32, Ordering};
    static LAST: AtomicI32 = AtomicI32::new(-1);
    let next = match reply {
        Ok(val) => {
            if val.get("unchanged").and_then(Value::as_bool) == Some(true) {
                return;
            }
            let Some(data) = val.get("data").filter(|v| v.is_object()) else {
                return;
            };
            if data["online"].as_bool() == Some(true) {
                2
            } else if data["running"].as_bool() == Some(true) {
                1
            } else {
                0
            }
        }
        Err(_) => 0,
    };
    if LAST.swap(next, Ordering::AcqRel) != next {
        unsafe { ph_host_notify_status(next) };
    }
}

pub async fn request(value: Value, launch: bool) -> Result<Value, String> {
    if !can_route() {
        let start = launch || value["op"] == "attach";
        let res = publisher(&value, start).await;
        notify_menu_status(&res);
        return res;
    }
    let (tx, rx) = oneshot::channel();
    let mode = match (launch, value["op"] == "attach") {
        (false, false) => 0,
        (true, false) => 1,
        (false, true) => 2,
        (true, true) => 3,
    };
    let bytes = serde_json::to_vec(&value).map_err(|_| String::from("internal"))?;
    let context = Box::into_raw(Box::new(tx)) as usize;
    unsafe {
        ph_host_call(bytes.as_ptr(), bytes.len(), mode, context, done);
    }
    let res = rx.await.map_err(|_| String::from("not_connected"))?;
    notify_menu_status(&res);
    res
}

fn can_route() -> bool {
    unsafe { ph_host_can_route() != 0 }
}

pub fn is_signed() -> bool {
    can_route()
}

pub async fn prepare_update() {
    if can_route() {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            request(serde_json::json!({"op": "stop"}), false),
        )
        .await;
    } else if let Ok(dir) = publisher_dir() {
        let _ = tokio::time::timeout(
            Duration::from_millis(800),
            ipc::request(&dir, &serde_json::json!({"op": "quit"})),
        )
        .await;
    }
    close_tray();
}

async fn publisher(value: &Value, start: bool) -> Result<Value, String> {
    static START: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let unavailable = || String::from("not_connected");
    let dir = publisher_dir().map_err(|_| unavailable())?;
    if let Ok(reply) = ipc::request(&dir, value).await {
        return Ok(reply);
    }
    if !start {
        return Err(unavailable());
    }
    let _lock = START.lock().await;
    if let Ok(reply) = ipc::request(&dir, value).await {
        return Ok(reply);
    }
    let (tx, rx) = oneshot::channel::<bool>();
    unsafe { ph_host_launch_publisher(Box::into_raw(Box::new(tx)) as usize, launched) };
    if !rx.await.unwrap_or(false) {
        return Err(unavailable());
    }
    for _ in 0..25 {
        if let Ok(reply) = ipc::request(&dir, value).await {
            return Ok(reply);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(unavailable())
}

pub fn open_settings() -> Result<(), String> {
    if unsafe { ph_host_open_settings() } != 0 {
        Ok(())
    } else {
        Err("extension_failed".into())
    }
}

pub fn tray() {
    unsafe { ph_host_tray() }
}

pub fn close_tray() {
    unsafe { ph_host_close_tray() }
}

pub fn menu_bar_setting(enabled: Option<bool>) -> Result<bool, String> {
    match unsafe { ph_host_menu_bar_setting(enabled.map(i32::from).unwrap_or(-1)) } {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err("internal".into()),
    }
}
