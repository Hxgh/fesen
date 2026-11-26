use axum::Router;
use axum_server::tls_rustls::{RustlsConfig, from_tcp_rustls};
use std::{
    io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use tokio::{fs, net::TcpListener, time::interval};

const RELOAD_INTERVAL: Duration = if cfg!(test) {
    Duration::from_millis(25)
} else {
    Duration::from_secs(30)
};

#[derive(PartialEq, Eq)]
struct Stamp {
    path: PathBuf,
    modified: SystemTime,
    len: u64,
}

impl Stamp {
    async fn read(path: &Path) -> io::Result<Self> {
        // Resolve symlinks so a generation switch is visible even with identical mtimes.
        let path = fs::canonicalize(path).await?;
        let metadata = fs::metadata(&path).await?;
        if !metadata.is_file() {
            return Err(io::Error::other("TLS requires regular files"));
        }
        Ok(Self {
            path,
            modified: metadata.modified()?,
            len: metadata.len(),
        })
    }
}

async fn stamps(cert: &Path, key: &Path) -> io::Result<(Stamp, Stamp)> {
    tokio::try_join!(Stamp::read(cert), Stamp::read(key))
}

pub(super) struct Tls {
    cert: PathBuf,
    key: PathBuf,
    loaded: (Stamp, Stamp),
    config: RustlsConfig,
}

impl Tls {
    pub(super) async fn load(
        cert: Option<PathBuf>,
        key: Option<PathBuf>,
        https_origin: bool,
    ) -> anyhow::Result<Option<Self>> {
        let (cert, key) = match (cert, key) {
            (None, None) => return Ok(None),
            (Some(cert), Some(key)) => (cert, key),
            _ => anyhow::bail!("FESEN_TLS_CERT 和 FESEN_TLS_KEY 必须同时设置"),
        };
        anyhow::ensure!(https_origin, "启用 TLS 时 FESEN_ORIGIN 必须使用 HTTPS");
        let unavailable = |_| anyhow::anyhow!("TLS 证书或私钥不可用，请检查文件格式和权限");
        let loaded = stamps(&cert, &key).await.map_err(unavailable)?;
        let config = RustlsConfig::from_pem_file(&loaded.0.path, &loaded.1.path)
            .await
            .map_err(unavailable)?;
        Ok(Some(Self {
            cert,
            key,
            loaded,
            config,
        }))
    }

    async fn watch(self) {
        let mut last = Some(self.loaded);
        let mut poll = interval(RELOAD_INTERVAL);
        loop {
            poll.tick().await;
            let next = stamps(&self.cert, &self.key).await.ok();
            if next == last {
                continue;
            }
            if let Some((cert, key)) = next.as_ref()
                && self
                    .config
                    .reload_from_pem_file(&cert.path, &key.path)
                    .await
                    .is_ok()
            {
                println!("TLS 证书已更新");
            } else {
                eprintln!("TLS 证书更新失败，继续使用上一次配置");
            }
            last = next;
        }
    }

    pub(super) async fn serve(self, listener: TcpListener, app: Router) -> io::Result<()> {
        let server = from_tcp_rustls(listener.into_std()?, self.config.clone())?;
        tokio::select! {
            result = server.serve(app.into_make_service()) => result,
            () = self.watch() => unreachable!(),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use fesen_proto::{Call, Credential, Event, Hello, PROTOCOL, Request, origin};
    use rcgen::{CertifiedKey, generate_simple_self_signed};
    use reqwest::{Client, StatusCode};
    use rustls::{ClientConfig, RootCertStore};
    use std::{fs, os::unix::fs::symlink, sync::Arc};
    use tokio::{
        net::TcpStream,
        time::{sleep, timeout},
    };
    use tokio_tungstenite::{Connector, client_async_tls_with_config, tungstenite::Message};

    const PASSWORD: &str = "tls-test-account-only";

    fn pair() -> CertifiedKey<rcgen::KeyPair> {
        generate_simple_self_signed(vec!["127.0.0.1".to_owned()]).unwrap()
    }

    fn write_pair(
        root: &Path,
        name: &str,
        cert: &CertifiedKey<rcgen::KeyPair>,
        key: &CertifiedKey<rcgen::KeyPair>,
    ) {
        let dir = root.join(name);
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("cert.pem"), cert.cert.pem()).unwrap();
        fs::write(dir.join("key.pem"), key.signing_key.serialize_pem()).unwrap();
    }

    fn rotate(root: &Path, generation: &str) {
        symlink(generation, root.join("next")).unwrap();
        fs::rename(root.join("next"), root.join("current")).unwrap();
    }

    fn client(cert: &CertifiedKey<rcgen::KeyPair>) -> Client {
        Client::builder()
            .no_proxy()
            .https_only(true)
            .add_root_certificate(reqwest::Certificate::from_der(cert.cert.der()).unwrap())
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn bad_tls_settings_do_not_start_plain_http() {
        let root = tempfile::tempdir().unwrap();
        let first = pair();
        write_pair(root.path(), "mismatch", &first, &pair());
        let cert = root.path().join("mismatch/cert.pem");
        let key = root.path().join("mismatch/key.pem");
        assert!(Tls::load(None, None, false).await.unwrap().is_none());
        assert!(Tls::load(Some(cert.clone()), None, true).await.is_err());
        assert!(Tls::load(None, Some(key.clone()), true).await.is_err());
        assert!(
            Tls::load(Some(cert.clone()), Some(key.clone()), false)
                .await
                .is_err()
        );
        assert!(
            Tls::load(Some(cert.clone()), Some(key.clone()), true)
                .await
                .is_err()
        );
        fs::write(cert.clone(), "malformed").unwrap();
        assert!(Tls::load(Some(cert), Some(key), true).await.is_err());
    }

    #[tokio::test]
    async fn direct_https_wss_survives_certificate_rotation() {
        let root = tempfile::tempdir().unwrap();
        let (first, second) = (pair(), pair());
        write_pair(root.path(), "first", &first, &first);
        write_pair(root.path(), "bad", &second, &first);
        write_pair(root.path(), "second", &second, &second);
        rotate(root.path(), "first");
        let tls = Tls::load(
            Some(root.path().join("current/cert.pem")),
            Some(root.path().join("current/key.pem")),
            true,
        )
        .await
        .unwrap()
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let health = format!("https://{address}/healthz");
        let url = origin(&format!("https://{address}/")).unwrap();
        let db = crate::store::db(&root.path().join("db")).unwrap();
        let password = crate::store::password_hash(PASSWORD).unwrap();
        crate::store::create(&db, "tls-test", &password, false).unwrap();
        let app = crate::App::new(db, url, 0, None).unwrap();
        let serving = tokio::spawn(tls.serve(listener, crate::router(app)));

        assert_eq!(
            client(&first).get(&health).send().await.unwrap().status(),
            StatusCode::OK
        );
        let stray = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        assert!(
            stray
                .get(format!("http://{address}/healthz"))
                .send()
                .await
                .is_err()
        );
        let mut roots = RootCertStore::empty();
        roots.add(first.cert.der().clone()).unwrap();
        let trust = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let (mut ws, _) = client_async_tls_with_config(
            format!("wss://{address}/control"),
            TcpStream::connect(address).await.unwrap(),
            None,
            Some(Connector::Rustls(Arc::new(trust))),
        )
        .await
        .unwrap();
        let login = Hello {
            version: PROTOCOL,
            certificate: pair().cert.der().to_vec(),
            credential: Credential::Password {
                username: "tls-test".into(),
                password: PASSWORD.into(),
                label: "TLS Test".into(),
            },
            machine_id: None,
            os: None,
        };
        ws.send(Message::Text(serde_json::to_string(&login).unwrap().into()))
            .await
            .unwrap();
        let welcome = timeout(Duration::from_secs(3), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(
            serde_json::from_str::<Event>(welcome.to_text().unwrap()).unwrap(),
            Event::Welcome { udp_port: 0, probe, stun: None, .. } if probe.is_empty()
        ));

        rotate(root.path(), "bad");
        sleep(RELOAD_INTERVAL * 5).await;
        assert_eq!(
            client(&first).get(&health).send().await.unwrap().status(),
            StatusCode::OK
        );
        assert!(client(&second).get(&health).send().await.is_err());

        rotate(root.path(), "second");
        timeout(Duration::from_secs(5), async {
            let fresh = client(&second);
            while !fresh
                .get(&health)
                .send()
                .await
                .is_ok_and(|r| r.status() == StatusCode::OK)
            {
                sleep(RELOAD_INTERVAL).await;
            }
        })
        .await
        .unwrap();
        assert!(client(&first).get(&health).send().await.is_err());

        let ping = Request {
            seq: 42,
            call: Call::Ping,
        };
        ws.send(Message::Text(serde_json::to_string(&ping).unwrap().into()))
            .await
            .unwrap();
        timeout(Duration::from_secs(3), async {
            while let Some(Ok(msg)) = ws.next().await {
                if matches!(
                    serde_json::from_str::<Event>(msg.to_text().unwrap()).unwrap(),
                    Event::Result {
                        seq: 42,
                        error: None
                    }
                ) {
                    return;
                }
            }
            panic!("WSS was closed by the certificate rotation");
        })
        .await
        .unwrap();
        serving.abort();
    }
}
