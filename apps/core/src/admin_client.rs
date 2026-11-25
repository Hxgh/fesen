use crate::platform::{self, Platform};
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
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::timeout,
};
use tokio_rustls::TlsConnector;
use url::Url;

const MAX_ADMIN_BODY: usize = 4096;

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

async fn send(
    platform: &dyn Platform,
    base: &Url,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Vec<u8>>,
) -> Result<(StatusCode, HeaderMap, Vec<u8>), Code> {
    let host = base.host_str().ok_or(Code::Invalid)?;
    let port = base.port_or_known_default().ok_or(Code::Invalid)?;
    let host_header = match base.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.to_owned(),
    };
    let origin_header = base.origin().ascii_serialization();

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
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        if let Ok(native) =
            tokio::task::spawn_blocking(rustls_native_certs::load_native_certs).await
        {
            roots.add_parsable_certificates(native.certs);
        }
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| Code::Internal)?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let server_name = ServerName::try_from(host.to_owned()).map_err(|_| Code::Invalid)?;
        let connector = TlsConnector::from(Arc::new(config));
        let tls = timeout(Duration::from_secs(6), connector.connect(server_name, tcp))
            .await
            .map_err(|_| Code::NotConnected)?
            .map_err(|_| Code::NotConnected)?;
        exchange(tls, method, path, &host_header, &origin_header, token, body).await
    } else {
        exchange(tcp, method, path, &host_header, &origin_header, token, body).await
    }
}

async fn exchange(
    io: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
    method: Method,
    path: &str,
    host_header: &str,
    origin_header: &str,
    token: Option<&str>,
    body: Option<Vec<u8>>,
) -> Result<(StatusCode, HeaderMap, Vec<u8>), Code> {
    timeout(Duration::from_secs(12), async {
        let (mut sender, conn) = http1::Builder::new()
            .handshake(TokioIo::new(io))
            .await
            .map_err(|_| Code::NotConnected)?;
        let driver = tokio::spawn(async move {
            let _ = conn.await;
        });

        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, host_header)
            .header(header::CONNECTION, "close")
            .header(header::ORIGIN, origin_header)
            .header("x-fesen-admin", "1");
        if let Some(tok) = token {
            builder = builder.header(header::COOKIE, format!("fesen_admin={tok}"));
        }
        let bytes = match body {
            Some(b) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Bytes::from(b)
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
        driver.abort();
        Ok((status, headers, data))
    })
    .await
    .map_err(|_| Code::TargetTimeout)?
}
