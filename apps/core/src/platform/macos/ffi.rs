use super::{Apple, Apply, Callback, Expired, TunnelIo};
use crate::{Core, ipc, platform::MTU};
use std::{
    ffi::{CStr, c_char},
    io,
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
}

pub fn publisher_dir() -> io::Result<PathBuf> {
    std::env::home_dir()
        .filter(|p| p.is_absolute())
        .map(|p| p.join("Library/Application Support/io.fesen.client/publisher"))
        .ok_or_else(|| io::Error::other("home directory unavailable"))
}

struct Publisher {
    host: Arc<Apple>,
    core: Arc<Core>,
}

static PUBLISHER: OnceLock<Publisher> = OnceLock::new();
static STOP_PUBLISHER: AtomicBool = AtomicBool::new(false);

fn serve_publisher() -> io::Result<()> {
    let dir = publisher_dir()?;
    let runtime = runtime()?;
    let result = runtime.block_on(async {
        let endpoint = ipc::Endpoint::bind(&dir).await?;
        let host = Arc::new(Apple::new(None));
        let core = Core::new(dir, host.clone(), true)?;
        PUBLISHER
            .set(Publisher {
                host,
                core: core.clone(),
            })
            .map_err(|_| io::Error::other("publisher already started"))?;
        if STOP_PUBLISHER.load(Ordering::Acquire) {
            core.stop();
        }
        endpoint.serve(core).await;
        Ok(())
    });
    runtime.shutdown_timeout(Duration::from_secs(4));
    result
}

#[unsafe(no_mangle)]
pub extern "C" fn ph_publisher_run() {
    let _ = serve_publisher();
}

#[unsafe(no_mangle)]
pub extern "C" fn ph_publisher_changed() {
    if let Some(p) = PUBLISHER.get() {
        p.host.physical_event.fetch_add(1, Ordering::Release);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ph_publisher_stop() {
    STOP_PUBLISHER.store(true, Ordering::Release);
    if let Some(p) = PUBLISHER.get() {
        p.core.stop();
    }
}

pub struct Engine {
    core: Arc<Core>,
    host: Arc<Apple>,
    runtime: tokio::runtime::Runtime,
}

/// # Safety
/// The path is a NUL-terminated string. Callbacks and their context must remain valid until stop returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ph_core_start(
    path: *const c_char,
    context: usize,
    adjust: Apply,
    output: Callback,
    expired: Expired,
) -> *mut Engine {
    if path.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(path) = (unsafe { CStr::from_ptr(path) }).to_str() else {
        return std::ptr::null_mut();
    };
    let host = Arc::new(Apple::new(Some(TunnelIo {
        context,
        adjust,
        output,
        input: OnceLock::new(),
    })));
    let Ok(runtime) = runtime() else {
        return std::ptr::null_mut();
    };
    let core = {
        let _enter = runtime.enter();
        match Core::new(PathBuf::from(path), host.clone(), true) {
            Ok(core) => core,
            Err(_) => return std::ptr::null_mut(),
        }
    };
    let watched = core.clone();
    runtime.spawn(async move {
        watched.wait_stopped().await;
        unsafe { expired(context) };
    });
    Box::into_raw(Box::new(Engine {
        core,
        host,
        runtime,
    }))
}

/// # Safety
/// Engine must be live. Bytes must be readable for len bytes; reply must retain its context until called.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ph_core_call(
    engine: *mut Engine,
    bytes: *const u8,
    len: usize,
    context: usize,
    reply: Callback,
) {
    if engine.is_null() || bytes.is_null() || len > 65536 {
        unsafe { reply(context, b"{}".as_ptr(), 2) };
        return;
    }
    let engine = unsafe { &*engine };
    let data = unsafe { std::slice::from_raw_parts(bytes, len) }.to_vec();
    let core = engine.core.clone();
    engine.runtime.spawn(async move {
        let bytes = ipc::dispatch_bytes(&core, &data).await;
        unsafe { reply(context, bytes.as_ptr(), bytes.len()) };
    });
}

/// # Safety
/// Engine must be live and bytes readable for len bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ph_core_packet(engine: *mut Engine, bytes: *const u8, len: usize) {
    if engine.is_null() || bytes.is_null() || !(20..=MTU).contains(&len) {
        return;
    }
    let e = unsafe { &*engine };
    if let Some(tunnel) = &e.host.tunnel
        && let Some(tx) = tunnel.input.get()
    {
        let _ = tx.try_send(unsafe { std::slice::from_raw_parts(bytes, len) }.to_vec());
    }
}

/// # Safety
/// Engine must be live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ph_core_changed(engine: *mut Engine) {
    if let Some(e) = unsafe { engine.as_ref() } {
        e.host.physical_event.fetch_add(1, Ordering::Release);
    }
}

/// # Safety
/// Stop once after all foreign calls into this engine have returned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ph_core_stop(engine: *mut Engine) {
    if engine.is_null() {
        return;
    }
    let e = unsafe { Box::from_raw(engine) };
    e.core.stop();
    e.runtime.block_on(e.core.wait_stopped());
    e.runtime.shutdown_timeout(Duration::from_secs(4));
}
