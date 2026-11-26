use crate::{
    control,
    platform::{self, Platform},
};
use fesen_proto::{Code, MAX_FRAME, origin};
use http_body_util::{BodyExt, Full, Limited};
use hyper::{
    Method, Request, StatusCode,
    body::Bytes,
    client::conn::http1,
    header::{self, HeaderMap},
};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use serde_json::Value;
use std::{net::SocketAddr, time::Duration};
use tokio::{sync::Mutex, task::JoinHandle, time::timeout};
use tokio_rustls::TlsConnector;
use url::Url;

const MAX_ADMIN_BODY: usize = 4096;

struct PooledConn {
    origin: String,
    sender: http1::SendRequest<Full<Bytes>>,
    driver: JoinHandle<()>,
}

impl Drop for PooledConn {
    fn drop(&mut self) {
        self.driver.abort();
    }
}

static POOL: Mutex<Option<PooledConn>> = Mutex::const_new(None);

pub async fn session(platform: &dyn Platform, server: &str, token: &str) -> Result<Value, Code> {
    let base = origin(server)?;
    let (status, _, payload) =
        send(platform, &base, Method::GET, "/admin/session", Some(token), None).await?;
    if status != StatusCode::OK {
        return Err(parse_error(status, &payload));
    }
    serde_json::from_slice(&payload).map_err(|_| Code::Internal)
}

pub async fn action(
    platform: &dyn Platform,
    server: &str,
    token: &str,
    action: &Value,
) -> Result<(), Code> {
    let base = origin(server)?;
    let body = serde_json::to_vec(action).map_err(|_| Code::Invalid)?;
    if body.len() > MAX_ADMIN_BODY {
        return Err(Code::Invalid);
    }
    let (status, _, payload) = send(
        platform,
        &base,
        Method::POST,
        "/admin/action",
        Some(token),
        Some(body),
    )
    .await?;
    if status != StatusCode::NO_CONTENT {
        return Err(parse_error(status, &payload));
    }
    Ok(())
}

fn parse_error(status: StatusCode, payload: &[u8]) -> Code {
    if let Ok(obj) = serde_json::from_slice::<Value>(payload)
        && let Some(err) = obj.get("error")
        && let Ok(code) = serde_json::from_value::<Code>(err.clone())
    {
        return code;
    }
    match status {
        StatusCode::UNAUTHORIZED => Code::Unauthorized,
        StatusCode::CONFLICT => Code::Conflict,
        StatusCode::TOO_MANY_REQUESTS => Code::Busy,
        StatusCode::BAD_REQUEST => Code::Invalid,
        _ => Code::Internal,
    }
}

async fn open_conn(platform: &dyn Platform, base: &Url, origin_key: String) -> Result<PooledConn, Code> {
    let host = base.host_str().ok_or(Code::Invalid)?;
    let port = base.port_or_known_default().ok_or(Code::Invalid)?;

    let tcp = timeout(Duration::from_secs(10), async {
        let addrs = tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| Code::NotConnected)?;
        for addr in addrs
            .filter_map(|a| match a {
                SocketAddr::V4(v4) => Some(v4),
                _ => None,
            })
            .take(4)
        {
            if let Ok(Ok(stream)) =
                timeout(Duration::from_secs(3), platform::tcp(platform, addr)).await
            {
                return Ok(stream);
            }
        }
        Err(Code::NotConnected)
    })
    .await
    .map_err(|_| Code::NotConnected)??;

    if base.scheme() == "https" {
        let config = control::tls_client_config().await?;
        let server_name = ServerName::try_from(host.to_owned()).map_err(|_| Code::Invalid)?;
        let connector = TlsConnector::from(config);
        let tls = timeout(Duration::from_secs(6), connector.connect(server_name, tcp))
            .await
            .map_err(|_| Code::NotConnected)?
            .map_err(|_| Code::NotConnected)?;
        let (sender, conn) = http1::Builder::new()
            .handshake(TokioIo::new(tls))
            .await
            .map_err(|_| Code::NotConnected)?;
        let driver = tokio::spawn(async move {
            let _ = conn.await;
        });
        Ok(PooledConn {
            origin: origin_key,
            sender,
            driver,
        })
    } else {
        let (sender, conn) = http1::Builder::new()
            .handshake(TokioIo::new(tcp))
            .await
            .map_err(|_| Code::NotConnected)?;
        let driver = tokio::spawn(async move {
            let _ = conn.await;
        });
        Ok(PooledConn {
            origin: origin_key,
            sender,
            driver,
        })
    }
}

async fn send(
    platform: &dyn Platform,
    base: &Url,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Vec<u8>>,
) -> Result<(StatusCode, HeaderMap, Vec<u8>), Code> {
    let host = base.host_str().ok_or(Code::Invalid)?;
    let host_header = match base.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.to_owned(),
    };
    let origin_header = base.origin().ascii_serialization();
    let idempotent = method == Method::GET;

    let cached = {
        let mut slot = POOL.lock().await;
        if slot.as_ref().is_some_and(|c| c.origin == origin_header) {
            slot.take()
        } else {
            None
        }
    };

    let (mut active, reused) = match cached {
        Some(mut c) => {
            if c.sender.ready().await.is_ok() {
                (c, true)
            } else {
                drop(c);
                (open_conn(platform, base, origin_header.clone()).await?, false)
            }
        }
        None => (open_conn(platform, base, origin_header.clone()).await?, false),
    };

    let result = exchange(
        &mut active.sender,
        method.clone(),
        path,
        &host_header,
        &origin_header,
        token,
        body.as_deref(),
    )
    .await;

    let (status, headers, data) = match result {
        Ok(ok) => ok,
        Err(_) if reused && idempotent => {
            drop(active);
            active = open_conn(platform, base, origin_header.clone()).await?;
            exchange(
                &mut active.sender,
                method,
                path,
                &host_header,
                &origin_header,
                token,
                body.as_deref(),
            )
            .await?
        }
        Err(err) => return Err(err),
    };

    let server_closed = headers
        .get(header::CONNECTION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("close"));
    if !server_closed && !active.driver.is_finished() {
        *POOL.lock().await = Some(active);
    }

    Ok((status, headers, data))
}

async fn exchange(
    sender: &mut http1::SendRequest<Full<Bytes>>,
    method: Method,
    path: &str,
    host_header: &str,
    origin_header: &str,
    token: Option<&str>,
    body: Option<&[u8]>,
) -> Result<(StatusCode, HeaderMap, Vec<u8>), Code> {
    timeout(Duration::from_secs(12), async {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, host_header)
            .header(header::CONNECTION, "keep-alive")
            .header(header::ORIGIN, origin_header)
            .header("x-fesen-admin", "1");
        if let Some(tok) = token {
            builder = builder.header(header::COOKIE, format!("fesen_admin={tok}"));
        }
        let bytes = match body {
            Some(b) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Bytes::copy_from_slice(b)
            }
            None => Bytes::new(),
        };
        let req = builder.body(Full::new(bytes)).map_err(|_| Code::Internal)?;
        let resp = sender
            .send_request(req)
            .await
            .map_err(|_| Code::NotConnected)?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let data = Limited::new(resp.into_body(), MAX_FRAME)
            .collect()
            .await
            .map_err(|_| Code::Internal)?
            .to_bytes()
            .to_vec();
        Ok((status, headers, data))
    })
    .await
    .map_err(|_| Code::TargetTimeout)?
}
