use crate::{
    Command, Core,
    local::{private_dir, private_file, save},
};
use fesen_proto::{Code, MAX_FRAME};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs, io,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    time::timeout,
};

const MAX_COMMAND: usize = 64 * 1024;

pub async fn dispatch(core: &Arc<Core>, data: &[u8]) -> Value {
    if data.len() > MAX_COMMAND {
        return json!({"error": Code::Invalid});
    }
    match serde_json::from_slice::<Command>(data) {
        Ok(command) => match core.execute(command).await {
            Ok(status) => json!({"data": status}),
            Err(error) => json!({"error": error}),
        },
        Err(_) => json!({"error": Code::Invalid}),
    }
}

#[derive(Serialize, Deserialize)]
struct Address {
    port: u16,
    token: [u8; 32],
}

pub struct Endpoint {
    path: PathBuf,
    _lock: fs::File,
    listener: TcpListener,
    token: [u8; 32],
    #[cfg(windows)]
    _directories: Vec<fs::File>,
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl Endpoint {
    pub async fn bind(dir: &Path) -> io::Result<Self> {
        #[cfg(windows)]
        let pins = crate::platform::windows::pin_profile(dir)?;
        private_dir(dir)?;
        let lock = private_file()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join("core.lock"))?;
        lock.try_lock().map_err(io::Error::other)?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let token = rand::random();
        let path = dir.join("ipc.json");
        let bytes = serde_json::to_vec(&Address {
            port: listener.local_addr()?.port(),
            token,
        })
        .map_err(io::Error::other)?;
        save(&path, &bytes)?;
        Ok(Self {
            path,
            _lock: lock,
            listener,
            token,
            #[cfg(windows)]
            _directories: pins,
        })
    }

    pub async fn serve(self, core: Arc<Core>) {
        let capacity = Arc::new(Semaphore::new(24));
        loop {
            let conn = tokio::select! {
                _ = core.quit.cancelled() => break,
                conn = self.listener.accept() => conn,
            };
            let Ok((mut stream, _)) = conn else { break };
            let Ok(slot) = capacity.clone().try_acquire_owned() else {
                continue;
            };
            let core = core.clone();
            let token = self.token;
            tokio::spawn(async move {
                let _slot = slot;
                let _ = timeout(Duration::from_secs(30), async {
                    let mut supplied = [0; 32];
                    stream.read_exact(&mut supplied).await?;
                    if supplied
                        .into_iter()
                        .zip(token)
                        .fold(0, |diff, (a, b)| diff | (a ^ b))
                        != 0
                    {
                        return Ok(());
                    }
                    let command = read(&mut stream, MAX_COMMAND).await?;
                    let reply = dispatch(&core, &command).await;
                    let reply = serde_json::to_vec(&reply).map_err(io::Error::other)?;
                    write(&mut stream, &reply).await
                })
                .await;
            });
        }
        core.stop();
        core.wait_stopped().await;
    }
}

pub async fn request(dir: &Path, value: &Value) -> io::Result<Value> {
    let Address { port, token } =
        serde_json::from_slice(&fs::read(dir.join("ipc.json"))?).map_err(io::Error::other)?;
    let mut stream = timeout(
        Duration::from_secs(2),
        TcpStream::connect((Ipv4Addr::LOCALHOST, port)),
    )
    .await??;
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > MAX_COMMAND {
        return Err(io::Error::other("invalid command"));
    }
    timeout(Duration::from_secs(29), async {
        stream.write_all(&token).await?;
        write(&mut stream, &bytes).await?;
        serde_json::from_slice(&read(&mut stream, MAX_FRAME).await?).map_err(io::Error::other)
    })
    .await?
}

async fn read(stream: &mut TcpStream, limit: usize) -> io::Result<Vec<u8>> {
    let size = stream.read_u32().await? as usize;
    if size == 0 || size > limit {
        return Err(io::Error::other("invalid frame"));
    }
    let mut data = vec![0; size];
    stream.read_exact(&mut data).await?;
    Ok(data)
}

async fn write(stream: &mut TcpStream, data: &[u8]) -> io::Result<()> {
    stream.write_u32(data.len() as u32).await?;
    stream.write_all(data).await
}
