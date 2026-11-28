use crate::{
    Command, Core, Status,
    local::{private_dir, private_file, save},
};
use fesen_proto::{Code, MAX_FRAME};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs, io,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    time::timeout,
};

const MAX_COMMAND: usize = 64 * 1024;
const UNCHANGED_REPLY: &[u8] = br#"{"unchanged":true}"#;

#[derive(Deserialize)]
struct PollHint<'a> {
    op: &'a str,
    #[serde(default)]
    ui: bool,
    #[serde(default)]
    since: Option<u64>,
}

#[derive(Serialize)]
struct DataReply<'a> {
    data: &'a Status,
}

#[derive(Serialize)]
struct ErrorReply {
    error: Code,
}

pub async fn dispatch_bytes(core: &Arc<Core>, data: &[u8]) -> Vec<u8> {
    if data.len() > MAX_COMMAND {
        return serde_json::to_vec(&ErrorReply {
            error: Code::Invalid,
        })
        .unwrap_or_default();
    }
    if let Ok(hint) = serde_json::from_slice::<PollHint<'_>>(data)
        && hint.op == "status"
        && let Some(since) = hint.since
    {
        return match core.poll_status(hint.ui, since) {
            Ok(None) => UNCHANGED_REPLY.to_vec(),
            Ok(Some(status)) => serde_json::to_vec(&DataReply { data: &status }).unwrap_or_default(),
            Err(error) => serde_json::to_vec(&ErrorReply { error }).unwrap_or_default(),
        };
    }
    match serde_json::from_slice::<Command>(data) {
        Ok(command) => match core.execute(command).await {
            Ok(status) => serde_json::to_vec(&DataReply { data: &status }).unwrap_or_default(),
            Err(error) => serde_json::to_vec(&ErrorReply { error }).unwrap_or_default(),
        },
        Err(_) => serde_json::to_vec(&ErrorReply {
            error: Code::Invalid,
        })
        .unwrap_or_default(),
    }
}

pub async fn dispatch(core: &Arc<Core>, data: &[u8]) -> Value {
    if data.len() > MAX_COMMAND {
        return json!({"error": Code::Invalid});
    }
    if let Ok(hint) = serde_json::from_slice::<PollHint<'_>>(data)
        && hint.op == "status"
        && let Some(since) = hint.since
    {
        return match core.poll_status(hint.ui, since) {
            Ok(None) => json!({"unchanged": true}),
            Ok(Some(status)) => json!({"data": status}),
            Err(error) => json!({"error": error}),
        };
    }
    match serde_json::from_slice::<Command>(data) {
        Ok(command) => match core.execute(command).await {
            Ok(status) => json!({"data": status}),
            Err(error) => json!({"error": error}),
        },
        Err(_) => json!({"error": Code::Invalid}),
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Address {
    port: u16,
    token: [u8; 32],
}

static ADDR_CACHE: Mutex<Option<(PathBuf, Address)>> = Mutex::new(None);

pub struct Endpoint {
    path: PathBuf,
    _lock: fs::File,
    listener: Option<TcpListener>,
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
            listener: Some(listener),
            token,
            #[cfg(windows)]
            _directories: pins,
        })
    }

    pub async fn serve(mut self, core: Arc<Core>) {
        let capacity = Arc::new(Semaphore::new(48));
        if let Some(listener) = self.listener.take() {
            loop {
                let conn = tokio::select! {
                    _ = core.quit.cancelled() => break,
                    conn = listener.accept() => conn,
                };
                let Ok((mut stream, _)) = conn else { break };
                let _ = stream.set_nodelay(true);
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
                        let reply = dispatch_bytes(&core, &command).await;
                        write(&mut stream, &reply).await
                    })
                    .await;
                });
            }
            drop(listener);
        }
        // Release the listening socket and ipc.json immediately so GUI quit/detach never blocks on core teardown.
        let _ = fs::remove_file(&self.path);
        core.stop();
        core.wait_stopped().await;
    }
}

fn load_address(dir: &Path, fresh: bool) -> io::Result<Address> {
    if !fresh
        && let Some((cached_dir, addr)) = ADDR_CACHE.lock().unwrap().as_ref()
        && cached_dir == dir
    {
        return Ok(addr.clone());
    }
    let addr: Address =
        serde_json::from_slice(&fs::read(dir.join("ipc.json"))?).map_err(io::Error::other)?;
    *ADDR_CACHE.lock().unwrap() = Some((dir.to_path_buf(), addr.clone()));
    Ok(addr)
}

async fn request_with(addr: &Address, payload: &[u8]) -> io::Result<Value> {
    let mut stream = timeout(
        Duration::from_secs(2),
        TcpStream::connect((Ipv4Addr::LOCALHOST, addr.port)),
    )
    .await??;
    let _ = stream.set_nodelay(true);
    let mut frame = Vec::with_capacity(32 + 4 + payload.len());
    frame.extend_from_slice(&addr.token);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    timeout(Duration::from_secs(29), async {
        stream.write_all(&frame).await?;
        serde_json::from_slice(&read(&mut stream, MAX_FRAME).await?).map_err(io::Error::other)
    })
    .await?
}

pub async fn request(dir: &Path, value: &Value) -> io::Result<Value> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > MAX_COMMAND {
        return Err(io::Error::other("invalid command"));
    }
    let addr = load_address(dir, false)?;
    match request_with(&addr, &bytes).await {
        Ok(reply) => Ok(reply),
        Err(first_err) => {
            // Core may have restarted on a new port; refresh ipc.json once.
            let fresh = load_address(dir, true)?;
            if fresh.port == addr.port && fresh.token == addr.token {
                return Err(first_err);
            }
            request_with(&fresh, &bytes).await
        }
    }
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
    let mut frame = Vec::with_capacity(4 + data.len());
    frame.extend_from_slice(&(data.len() as u32).to_be_bytes());
    frame.extend_from_slice(data);
    stream.write_all(&frame).await
}
