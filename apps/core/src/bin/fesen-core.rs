#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    use fesen_core::{
        Core,
        ipc::Endpoint,
        platform::windows::{Windows, data_dir, tray, verify_core},
    };
    use std::sync::Arc;

    let binary = std::env::current_exe()?;
    let (_, dll) = verify_core(&binary).map_err(|e| anyhow::anyhow!("{}", e.code()))?;
    let dir = data_dir()?;
    let endpoint = Endpoint::bind(&dir).await?;
    let p = Windows::new(dll);
    let core = Core::new(dir, Arc::new(p), true)?;
    tokio::join!(
        endpoint.serve(core.clone()),
        tray(core, binary.with_file_name("fesen.exe"))
    );
    Ok(())
}

#[cfg(not(windows))]
fn main() {}
