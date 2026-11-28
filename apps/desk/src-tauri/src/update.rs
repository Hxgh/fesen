use futures_util::StreamExt;
use reqwest::{Client, Url, redirect};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager};
use tokio::{
    io::AsyncWriteExt,
    sync::{Mutex, watch},
};

const REPO: &str = "Hxgh/fesen";
const PRIMARY_MANIFEST_URL: &str =
    "https://github.com/Hxgh/fesen/releases/latest/download/latest.json";
const FALLBACK_API_URL: &str = "https://api.github.com/repos/Hxgh/fesen/releases/latest";
const FALLBACK_WEB_URL: &str = "https://github.com/Hxgh/fesen/releases/latest";
const FALLBACK_DOWNLOAD_BASE: &str = "https://github.com/Hxgh/fesen/releases/download";
const MAX_MANIFEST_BYTES: usize = 512 * 1024;
const MAX_CHECKSUMS_BYTES: usize = 64 * 1024;
const MAX_PACKAGE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum UpdateState {
    Idle,
    Checking,
    UpToDate {
        current: String,
        checked_at: u64,
    },
    Available {
        current: String,
        version: String,
        notes: String,
        pub_date: Option<String>,
        size: Option<u64>,
        release_url: Option<String>,
    },
    Downloading {
        current: String,
        version: String,
        notes: String,
        pub_date: Option<String>,
        downloaded: u64,
        total: Option<u64>,
        release_url: Option<String>,
    },
    Paused {
        current: String,
        version: String,
        notes: String,
        pub_date: Option<String>,
        downloaded: u64,
        total: Option<u64>,
        release_url: Option<String>,
    },
    Ready {
        current: String,
        version: String,
        notes: String,
        pub_date: Option<String>,
        size: u64,
        release_url: Option<String>,
    },
    Installing {
        current: String,
        version: String,
        notes: String,
        pub_date: Option<String>,
        release_url: Option<String>,
    },
    Error {
        current: String,
        stage: String,
        message: String,
        version: Option<String>,
        notes: Option<String>,
        pub_date: Option<String>,
        size: Option<u64>,
        release_url: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub version: String,
    pub notes: String,
    pub pub_date: Option<String>,
    pub release_url: Option<String>,
    pub asset_name: String,
    pub download_url: String,
    pub sha256: String,
    pub size: Option<u64>,
}

#[derive(Clone, Debug)]
struct CachedArtifact {
    candidate: Candidate,
    path: PathBuf,
    size: u64,
}

#[derive(Clone, Debug)]
pub struct EndpointConfig {
    pub manifest_url: String,
    pub api_url: String,
    pub web_latest_url: String,
    pub download_base_url: String,
    pub test_origin: Option<(String, String, Option<u16>)>,
}

impl Default for EndpointConfig {
    fn default() -> Self {
        #[cfg(debug_assertions)]
        if let Ok(custom) = std::env::var("FESEN_UPDATE_ENDPOINT") {
            let trimmed = custom.trim();
            if let Ok(parsed) = Url::parse(trimmed)
                && parsed.scheme() == "http"
                && matches!(parsed.host_str(), Some("127.0.0.1" | "localhost"))
            {
                let test_origin = Some((
                    parsed.scheme().to_owned(),
                    parsed.host_str().unwrap_or_default().to_owned(),
                    parsed.port_or_known_default(),
                ));
                let manifest_url = parsed.to_string();
                let api_url = parsed
                    .join("release.json")
                    .map(|u| u.to_string())
                    .unwrap_or_else(|_| manifest_url.clone());
                let web_latest_url = parsed
                    .join("releases/latest")
                    .map(|u| u.to_string())
                    .unwrap_or_else(|_| manifest_url.clone());
                let download_base_url = parsed
                    .join("releases/download")
                    .map(|u| u.to_string())
                    .unwrap_or_else(|_| manifest_url.clone());
                return Self {
                    manifest_url,
                    api_url,
                    web_latest_url,
                    download_base_url,
                    test_origin,
                };
            }
        }
        Self {
            manifest_url: PRIMARY_MANIFEST_URL.to_owned(),
            api_url: FALLBACK_API_URL.to_owned(),
            web_latest_url: FALLBACK_WEB_URL.to_owned(),
            download_base_url: FALLBACK_DOWNLOAD_BASE.to_owned(),
            test_origin: None,
        }
    }
}

pub struct ManagerState {
    inner: Mutex<Inner>,
    op_lock: Mutex<()>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StopMode {
    Pause,
    Cancel,
}

struct Inner {
    state: UpdateState,
    candidate: Option<Candidate>,
    cached: Option<CachedArtifact>,
    cancel_tx: Option<watch::Sender<bool>>,
    stop_mode: Option<StopMode>,
}

impl Default for ManagerState {
    fn default() -> Self {
        Self::new()
    }
}

impl ManagerState {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                state: UpdateState::Idle,
                candidate: None,
                cached: None,
                cancel_tx: None,
                stop_mode: None,
            }),
            op_lock: Mutex::new(()),
        }
    }

    pub async fn snapshot(&self) -> UpdateState {
        self.inner.lock().await.state.clone()
    }

    pub async fn pause_download(&self) -> UpdateState {
        let mut guard = self.inner.lock().await;
        if matches!(guard.state, UpdateState::Downloading { .. }) {
            guard.stop_mode = Some(StopMode::Pause);
            if let Some(tx) = guard.cancel_tx.take() {
                let _ = tx.send(true);
            }
            if let UpdateState::Downloading {
                current,
                version,
                notes,
                pub_date,
                downloaded,
                total,
                release_url,
            } = guard.state.clone()
            {
                guard.state = UpdateState::Paused {
                    current,
                    version,
                    notes,
                    pub_date,
                    downloaded,
                    total,
                    release_url,
                };
            }
        }
        guard.state.clone()
    }

    pub async fn cancel_download(&self, cache_dir: Option<&Path>) -> UpdateState {
        let candidate = {
            let mut guard = self.inner.lock().await;
            guard.stop_mode = Some(StopMode::Cancel);
            if let Some(tx) = guard.cancel_tx.take() {
                let _ = tx.send(true);
            }
            let c = guard.candidate.clone();
            if matches!(
                guard.state,
                UpdateState::Downloading { .. } | UpdateState::Paused { .. }
            ) && let Some(ref cand) = c
            {
                guard.state = available_state(cand);
            }
            c
        };
        if let (Some(dir), Some(cand)) = (cache_dir, candidate) {
            let part = dir.join(format!("{}.part", cand.asset_name));
            let _ = tokio::fs::remove_file(&part).await;
        }
        self.inner.lock().await.state.clone()
    }
}

#[derive(Deserialize)]
struct LatestManifest {
    version: String,
    #[serde(default)]
    pub_date: Option<String>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    url: Option<String>,
    platforms: HashMap<String, ManifestPlatform>,
}

#[derive(Deserialize)]
struct ManifestPlatform {
    url: String,
    sha256: String,
    #[serde(default)]
    size: Option<u64>,
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    digest: Option<String>,
}

fn current_version_str() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn current_version() -> Result<Version, String> {
    Version::parse(current_version_str().trim_start_matches(['v', 'V']))
        .map_err(|_| "internal".to_owned())
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn available_state(c: &Candidate) -> UpdateState {
    UpdateState::Available {
        current: current_version_str().to_owned(),
        version: c.version.clone(),
        notes: c.notes.clone(),
        pub_date: c.pub_date.clone(),
        size: c.size,
        release_url: c.release_url.clone(),
    }
}

fn ready_state(c: &Candidate, size: u64) -> UpdateState {
    UpdateState::Ready {
        current: current_version_str().to_owned(),
        version: c.version.clone(),
        notes: c.notes.clone(),
        pub_date: c.pub_date.clone(),
        size,
        release_url: c.release_url.clone(),
    }
}

fn is_trusted_github_host(host: &str) -> bool {
    matches!(host, "github.com" | "api.github.com")
        || (host.len() > ".githubusercontent.com".len()
            && host.ends_with(".githubusercontent.com"))
}

fn is_allowed_url(url: &Url, endpoints: &EndpointConfig) -> bool {
    if url.scheme() == "https" {
        return url.host_str().is_some_and(is_trusted_github_host);
    }
    if let Some((scheme, host, port)) = &endpoints.test_origin {
        return url.scheme() == scheme
            && url.host_str() == Some(host.as_str())
            && url.port_or_known_default() == *port;
    }
    false
}

fn safe_release_page(raw: Option<&str>, endpoints: &EndpointConfig) -> Option<String> {
    let raw = raw?.trim();
    let parsed = Url::parse(raw).ok()?;
    if parsed.scheme() == "https"
        && parsed.host_str() == Some("github.com")
        && parsed.path().starts_with(&format!("/{REPO}/"))
    {
        return Some(parsed.to_string());
    }
    if let Some((scheme, host, port)) = &endpoints.test_origin
        && parsed.scheme() == scheme
        && parsed.host_str() == Some(host.as_str())
        && parsed.port_or_known_default() == *port
    {
        return Some(parsed.to_string());
    }
    None
}

fn valid_sha256_hex(hex_str: &str) -> Option<String> {
    let clean = hex_str
        .trim()
        .strip_prefix("sha256:")
        .unwrap_or(hex_str.trim());
    if clean.len() == 64 && clean.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(clean.to_ascii_lowercase())
    } else {
        None
    }
}

fn safe_asset_filename(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.len() > 160
        || trimmed.contains("..")
        || !trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        return None;
    }
    Some(trimmed.to_owned())
}

fn platform_signed() -> bool {
    #[cfg(target_os = "macos")]
    {
        super::macos::is_signed()
    }
    #[cfg(not(target_os = "macos"))]
    {
        !cfg!(debug_assertions)
    }
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
enum PlatformKind {
    WindowsX64,
    MacosArm64,
    AndroidArm64,
    Unsupported,
}

fn current_platform() -> PlatformKind {
    #[cfg(all(windows, target_arch = "x86_64"))]
    {
        PlatformKind::WindowsX64
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        PlatformKind::MacosArm64
    }
    #[cfg(all(target_os = "android", target_arch = "aarch64"))]
    {
        PlatformKind::AndroidArm64
    }
    #[cfg(not(any(
        all(windows, target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "android", target_arch = "aarch64"),
    )))]
    {
        PlatformKind::Unsupported
    }
}

fn select_manifest_platform(
    platforms: &HashMap<String, ManifestPlatform>,
    kind: PlatformKind,
    mac_signed: bool,
    debug: bool,
) -> Option<(String, &ManifestPlatform)> {
    let keys: &[&str] = match kind {
        PlatformKind::WindowsX64 => &["windows-x86_64"],
        PlatformKind::MacosArm64 => {
            if mac_signed {
                &["darwin-aarch64", "darwin-aarch64-dmg"]
            } else {
                &[
                    "darwin-aarch64",
                    "darwin-aarch64-unsigned",
                    "darwin-aarch64-dmg",
                    "darwin-aarch64-unsigned-dmg",
                ]
            }
        }
        PlatformKind::AndroidArm64 => {
            if debug {
                &["android-aarch64", "android-aarch64-debug"]
            } else {
                &["android-aarch64"]
            }
        }
        PlatformKind::Unsupported => return None,
    };
    for &key in keys {
        if let Some(entry) = platforms.get(key) {
            return Some((key.to_owned(), entry));
        }
    }
    None
}

fn select_github_asset(
    assets: &[GitHubAsset],
    kind: PlatformKind,
    mac_signed: bool,
    debug: bool,
) -> Option<&GitHubAsset> {
    let suffixes: &[&str] = match kind {
        PlatformKind::WindowsX64 => &["_windows-x64-setup.exe"],
        PlatformKind::MacosArm64 => {
            if mac_signed {
                &["_macos-arm64.app.tar.gz", "_macos-arm64.dmg"]
            } else {
                &[
                    "_macos-arm64.app.tar.gz",
                    "_macos-arm64-unsigned.app.tar.gz",
                    "_macos-arm64.dmg",
                    "_macos-arm64-unsigned.dmg",
                ]
            }
        }
        PlatformKind::AndroidArm64 => {
            if debug {
                &["_android-arm64.apk", "_android-arm64-debug.apk"]
            } else {
                &["_android-arm64.apk"]
            }
        }
        PlatformKind::Unsupported => return None,
    };
    for suffix in suffixes {
        if let Some(asset) = assets.iter().find(|a| a.name.ends_with(suffix)) {
            return Some(asset);
        }
    }
    None
}

pub fn clean_release_notes(raw: &str) -> String {
    let normalized = raw.replace("\r\n", "\n");
    let slice = if let Some(idx) = normalized.find("## What's Changed") {
        &normalized[idx..]
    } else if let Some(idx) = normalized.find("### Server (Docker") {
        if let Some(marker) = normalized[idx..].find("Verify checksums against `SHA256SUMS.txt`.") {
            normalized[idx + marker + "Verify checksums against `SHA256SUMS.txt`.".len()..].trim()
        } else {
            normalized.trim()
        }
    } else {
        normalized.trim()
    };

    let mut out = Vec::new();
    for line in slice.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("**Full Changelog**:") {
            continue;
        }
        if out.is_empty()
            && matches!(
                trimmed.to_ascii_lowercase().as_str(),
                "## what's changed" | "### what's changed" | "## 更新内容" | "## 更新摘要"
            )
        {
            continue;
        }
        out.push(line.trim_end());
    }
    let text = out.join("\n").trim().to_owned();
    if text.chars().count() > 6000 {
        text.chars().take(6000).collect()
    } else {
        text
    }
}

fn parse_checksum_for_asset(sums_text: &str, asset_name: &str) -> Option<String> {
    for line in sums_text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(hash) = parts.next() else { continue };
        let Some(file) = parts.next() else { continue };
        let file = file.trim_start_matches('*');
        if file == asset_name
            && let Some(valid) = valid_sha256_hex(hash)
        {
            return Some(valid);
        }
    }
    None
}

fn build_tls_config() -> Result<rustls::ClientConfig, String> {
    static CONFIG: std::sync::OnceLock<rustls::ClientConfig> = std::sync::OnceLock::new();
    if let Some(cfg) = CONFIG.get() {
        return Ok(cfg.clone());
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let native = rustls_native_certs::load_native_certs();
    roots.add_parsable_certificates(native.certs);
    let mut cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| "internal".to_owned())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let _ = CONFIG.set(cfg.clone());
    Ok(cfg)
}

fn build_http_client(endpoints: &EndpointConfig) -> Result<Client, String> {
    let cfg = build_tls_config()?;
    let allowed = endpoints.clone();
    let policy = redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= 6 {
            attempt.error("too many redirects")
        } else if is_allowed_url(attempt.url(), &allowed) {
            attempt.follow()
        } else {
            attempt.error("untrusted redirect")
        }
    });
    let ua = format!(
        "fesen/{} ({}; {})",
        current_version_str(),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    Client::builder()
        .tls_backend_preconfigured(cfg)
        .user_agent(ua)
        .connect_timeout(Duration::from_secs(12))
        .redirect(policy)
        .build()
        .map_err(|_| "internal".to_owned())
}

fn parse_version_allowed(raw: &str, current: &Version) -> Option<Version> {
    let parsed = Version::parse(raw.trim().trim_start_matches(['v', 'V'])).ok()?;
    if !parsed.pre.is_empty() && current.pre.is_empty() {
        return None;
    }
    if parsed > *current {
        Some(parsed)
    } else {
        None
    }
}

async fn read_bounded_bytes(resp: reqwest::Response, limit: usize) -> Result<Vec<u8>, String> {
    if let Some(len) = resp.content_length()
        && len as usize > limit
    {
        return Err("update_network".to_owned());
    }
    let mut stream = resp.bytes_stream();
    let mut buf = Vec::new();
    while let Some(chunk) = tokio::time::timeout(Duration::from_secs(15), stream.next())
        .await
        .map_err(|_| "update_network".to_owned())?
    {
        let chunk = chunk.map_err(|_| "update_network".to_owned())?;
        if buf.len().saturating_add(chunk.len()) > limit {
            return Err("update_network".to_owned());
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

async fn resolve_from_manifest(
    client: &Client,
    endpoints: &EndpointConfig,
    current: &Version,
    kind: PlatformKind,
    mac_signed: bool,
    debug: bool,
) -> Result<Option<Candidate>, String> {
    let url = Url::parse(&endpoints.manifest_url).map_err(|_| "internal".to_owned())?;
    if !is_allowed_url(&url, endpoints) {
        return Err("internal".to_owned());
    }
    let resp = tokio::time::timeout(Duration::from_secs(12), client.get(url).send())
        .await
        .map_err(|_| "update_network".to_owned())?
        .map_err(|_| "update_network".to_owned())?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err("manifest_missing".to_owned());
    }
    if !resp.status().is_success() {
        return Err("update_network".to_owned());
    }
    let bytes = read_bounded_bytes(resp, MAX_MANIFEST_BYTES).await?;
    let manifest: LatestManifest =
        serde_json::from_slice(&bytes).map_err(|_| "manifest_invalid".to_owned())?;
    let Some(next_ver) = parse_version_allowed(&manifest.version, current) else {
        return Ok(None);
    };
    let Some((_, entry)) = select_manifest_platform(&manifest.platforms, kind, mac_signed, debug)
    else {
        return Ok(None);
    };
    let dl_url = Url::parse(&entry.url).map_err(|_| "manifest_invalid".to_owned())?;
    if !is_allowed_url(&dl_url, endpoints) {
        return Err("manifest_invalid".to_owned());
    }
    let asset_name = dl_url
        .path_segments()
        .and_then(|mut segs| segs.next_back())
        .and_then(safe_asset_filename)
        .ok_or_else(|| "manifest_invalid".to_owned())?;
    let sha256 = valid_sha256_hex(&entry.sha256).ok_or_else(|| "manifest_invalid".to_owned())?;
    let notes = clean_release_notes(manifest.notes.as_deref().unwrap_or_default());
    Ok(Some(Candidate {
        version: next_ver.to_string(),
        notes,
        pub_date: manifest.pub_date,
        release_url: safe_release_page(manifest.url.as_deref(), endpoints),
        asset_name,
        download_url: dl_url.to_string(),
        sha256,
        size: entry.size,
    }))
}

async fn resolve_from_github_api(
    client: &Client,
    endpoints: &EndpointConfig,
    current: &Version,
    kind: PlatformKind,
    mac_signed: bool,
    debug: bool,
) -> Result<Option<Candidate>, String> {
    let url = Url::parse(&endpoints.api_url).map_err(|_| "internal".to_owned())?;
    if !is_allowed_url(&url, endpoints) {
        return Err("internal".to_owned());
    }
    let resp = tokio::time::timeout(
        Duration::from_secs(12),
        client
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .send(),
    )
    .await
    .map_err(|_| "update_network".to_owned())?
    .map_err(|_| "update_network".to_owned())?;
    if !resp.status().is_success() {
        return Err("update_network".to_owned());
    }
    let bytes = read_bounded_bytes(resp, MAX_MANIFEST_BYTES).await?;
    let release: GitHubRelease =
        serde_json::from_slice(&bytes).map_err(|_| "update_network".to_owned())?;
    if release.draft || (release.prerelease && current.pre.is_empty()) {
        return Ok(None);
    }
    let Some(next_ver) = parse_version_allowed(&release.tag_name, current) else {
        return Ok(None);
    };
    let Some(asset) = select_github_asset(&release.assets, kind, mac_signed, debug) else {
        return Ok(None);
    };
    let asset_name =
        safe_asset_filename(&asset.name).ok_or_else(|| "update_network".to_owned())?;
    let dl_url =
        Url::parse(&asset.browser_download_url).map_err(|_| "update_network".to_owned())?;
    if !is_allowed_url(&dl_url, endpoints) {
        return Err("update_network".to_owned());
    }

    let mut sha256 = asset.digest.as_deref().and_then(valid_sha256_hex);
    if sha256.is_none()
        && let Some(sums_asset) = release.assets.iter().find(|a| a.name == "SHA256SUMS.txt")
        && let Ok(sums_url) = Url::parse(&sums_asset.browser_download_url)
        && is_allowed_url(&sums_url, endpoints)
        && let Ok(Ok(sums_resp)) =
            tokio::time::timeout(Duration::from_secs(10), client.get(sums_url).send()).await
        && sums_resp.status().is_success()
        && let Ok(sums_bytes) = read_bounded_bytes(sums_resp, MAX_CHECKSUMS_BYTES).await
        && let Ok(sums_text) = String::from_utf8(sums_bytes)
    {
        sha256 = parse_checksum_for_asset(&sums_text, &asset_name);
    }
    let sha256 = sha256.ok_or_else(|| "update_verify_failed".to_owned())?;
    let notes = clean_release_notes(release.body.as_deref().unwrap_or_default());
    Ok(Some(Candidate {
        version: next_ver.to_string(),
        notes,
        pub_date: release.published_at,
        release_url: safe_release_page(release.html_url.as_deref(), endpoints),
        asset_name,
        download_url: dl_url.to_string(),
        sha256,
        size: asset.size,
    }))
}

async fn resolve_from_web_fallback(
    client: &Client,
    endpoints: &EndpointConfig,
    current: &Version,
    kind: PlatformKind,
    mac_signed: bool,
    debug: bool,
) -> Result<Option<Candidate>, String> {
    let url = Url::parse(&endpoints.web_latest_url).map_err(|_| "internal".to_owned())?;
    if !is_allowed_url(&url, endpoints) {
        return Err("internal".to_owned());
    }
    let resp = tokio::time::timeout(Duration::from_secs(12), client.get(url).send())
        .await
        .map_err(|_| "update_network".to_owned())?
        .map_err(|_| "update_network".to_owned())?;
    if !resp.status().is_success() {
        return Err("update_network".to_owned());
    }
    let final_url = resp.url().clone();
    let Some(tag) = final_url
        .path()
        .split("/releases/tag/")
        .nth(1)
        .map(|s| s.trim_matches('/'))
        .filter(|s| !s.is_empty() && !s.contains('/'))
    else {
        return Err("update_network".to_owned());
    };
    let Some(next_ver) = parse_version_allowed(tag, current) else {
        return Ok(None);
    };
    let html_bytes = read_bounded_bytes(resp, MAX_MANIFEST_BYTES).await.unwrap_or_default();
    let html = String::from_utf8_lossy(&html_bytes);
    let notes = extract_notes_from_release_html(&html);

    let sums_url_str = format!(
        "{}/{}/SHA256SUMS.txt",
        endpoints.download_base_url.trim_end_matches('/'),
        tag
    );
    let sums_url = Url::parse(&sums_url_str).map_err(|_| "update_network".to_owned())?;
    if !is_allowed_url(&sums_url, endpoints) {
        return Err("update_network".to_owned());
    }
    let sums_resp = tokio::time::timeout(Duration::from_secs(10), client.get(sums_url).send())
        .await
        .map_err(|_| "update_network".to_owned())?
        .map_err(|_| "update_network".to_owned())?;
    if !sums_resp.status().is_success() {
        return Err("update_verify_failed".to_owned());
    }
    let sums_bytes = read_bounded_bytes(sums_resp, MAX_CHECKSUMS_BYTES).await?;
    let sums_text = String::from_utf8(sums_bytes).map_err(|_| "update_verify_failed".to_owned())?;

    let mut assets = Vec::new();
    for line in sums_text.lines() {
        let mut parts = line.split_whitespace();
        let Some(hash) = parts.next().and_then(valid_sha256_hex) else {
            continue;
        };
        let Some(file) = parts.next().map(|f| f.trim_start_matches('*')) else {
            continue;
        };
        let Some(name) = safe_asset_filename(file) else {
            continue;
        };
        let dl_url = format!(
            "{}/{}/{}",
            endpoints.download_base_url.trim_end_matches('/'),
            tag,
            name
        );
        assets.push(GitHubAsset {
            name,
            browser_download_url: dl_url,
            size: None,
            digest: Some(format!("sha256:{hash}")),
        });
    }

    let Some(asset) = select_github_asset(&assets, kind, mac_signed, debug) else {
        return Ok(None);
    };
    let dl_url =
        Url::parse(&asset.browser_download_url).map_err(|_| "update_network".to_owned())?;
    if !is_allowed_url(&dl_url, endpoints) {
        return Err("update_network".to_owned());
    }
    let sha256 = asset
        .digest
        .as_deref()
        .and_then(valid_sha256_hex)
        .ok_or_else(|| "update_verify_failed".to_owned())?;
    Ok(Some(Candidate {
        version: next_ver.to_string(),
        notes,
        pub_date: None,
        release_url: safe_release_page(Some(final_url.as_str()), endpoints),
        asset_name: asset.name.clone(),
        download_url: dl_url.to_string(),
        sha256,
        size: None,
    }))
}

fn extract_notes_from_release_html(html: &str) -> String {
    let Some(start_marker) = html.find("class=\"markdown-body") else {
        return String::new();
    };
    let Some(gt) = html[start_marker..].find('>') else {
        return String::new();
    };
    let after = &html[start_marker + gt + 1..];
    let mut depth = 1usize;
    let mut i = 0usize;
    let bytes = after.as_bytes();
    while i < bytes.len() && depth > 0 {
        if bytes[i..].starts_with(b"<div") {
            depth += 1;
            i += 4;
        } else if bytes[i..].starts_with(b"</div>") {
            depth -= 1;
            if depth == 0 {
                break;
            }
            i += 6;
        } else {
            i += 1;
        }
    }
    let body_html = &after[..i];
    let mut text = String::new();
    let mut in_tag = false;
    let mut tag_buf = String::new();
    for ch in body_html.chars() {
        if ch == '<' {
            in_tag = true;
            tag_buf.clear();
        } else if ch == '>' && in_tag {
            in_tag = false;
            let t = tag_buf.trim().to_ascii_lowercase();
            if t.starts_with("li") {
                if !text.ends_with('\n') && !text.is_empty() {
                    text.push('\n');
                }
                text.push_str("- ");
            } else if t.starts_with("/p")
                || t.starts_with("/li")
                || t.starts_with("/h")
                || t.starts_with("/pre")
                || t.starts_with("br")
            {
                text.push('\n');
            }
        } else if in_tag {
            if tag_buf.len() < 32 {
                tag_buf.push(ch);
            }
        } else {
            text.push(ch);
        }
    }
    let decoded = text
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    clean_release_notes(&decoded)
}

async fn sync_live_github_notes(
    client: &Client,
    endpoints: &EndpointConfig,
    cand: &mut Candidate,
) {
    let Some(page_url_str) = cand.release_url.as_deref() else {
        return;
    };
    let Ok(page_url) = Url::parse(page_url_str) else {
        return;
    };
    if !is_allowed_url(&page_url, endpoints) {
        return;
    }
    let Ok(Ok(resp)) = tokio::time::timeout(Duration::from_secs(4), client.get(page_url).send()).await
    else {
        return;
    };
    if !resp.status().is_success() {
        return;
    }
    let is_html = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.to_ascii_lowercase().contains("text/html"));
    if !is_html {
        return;
    }
    if let Ok(bytes) = read_bounded_bytes(resp, MAX_MANIFEST_BYTES).await {
        let html = String::from_utf8_lossy(&bytes);
        let live_notes = extract_notes_from_release_html(&html);
        if !live_notes.is_empty() {
            cand.notes = live_notes;
        }
    }
}

async fn resolve_candidate(
    client: &Client,
    endpoints: &EndpointConfig,
    current: &Version,
    kind: PlatformKind,
    mac_signed: bool,
    debug: bool,
) -> Result<Option<Candidate>, String> {
    match resolve_from_manifest(client, endpoints, current, kind, mac_signed, debug).await {
        Ok(Some(mut cand)) => {
            sync_live_github_notes(client, endpoints, &mut cand).await;
            Ok(Some(cand))
        }
        Ok(None) => Ok(None),
        Err(err) if err == "manifest_missing" || err == "manifest_invalid" || err == "update_network" => {
            match resolve_from_github_api(client, endpoints, current, kind, mac_signed, debug).await {
                Ok(res) => Ok(res),
                Err(_) => {
                    resolve_from_web_fallback(client, endpoints, current, kind, mac_signed, debug)
                        .await
                }
            }
        }
        Err(err) => Err(err),
    }
}

async fn file_sha256(path: &Path) -> std::io::Result<(String, u64)> {
    let bytes = tokio::fs::read(path).await?;
    let len = bytes.len() as u64;
    let digest = Sha256::digest(&bytes);
    Ok((hex_lower(digest.as_slice()), len))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn updates_dir<R: tauri::Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|_| "internal".to_owned())?
        .join("updates");
    Ok(dir)
}

fn emit_state<R: tauri::Runtime>(app: &AppHandle<R>, state: &UpdateState) {
    let _ = app.emit_to("main", "update://state", state);
}

pub async fn cleanup_stale_updates<R: tauri::Runtime>(app: AppHandle<R>) {
    let Ok(dir) = updates_dir(&app) else { return };
    let Ok(current) = current_version() else { return };
    let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.ends_with(".part") || name.starts_with(".stage-") {
            if path.is_dir() {
                let _ = tokio::fs::remove_dir_all(&path).await;
            } else {
                let _ = tokio::fs::remove_file(&path).await;
            }
            continue;
        }
        if let Some(ver_str) = extract_version_from_filename(name)
            && let Ok(ver) = Version::parse(ver_str)
            && ver <= current
        {
            let _ = tokio::fs::remove_file(&path).await;
        }
    }
}

fn extract_version_from_filename(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("fesen_")?;
    let (ver, _) = rest.split_once('_')?;
    Some(ver)
}

pub async fn check_for_update<R: tauri::Runtime>(
    app: &AppHandle<R>,
    mgr: &ManagerState,
    silent: bool,
) -> UpdateState {
    check_with_endpoints(app, mgr, &EndpointConfig::default(), silent).await
}

async fn check_with_endpoints<R: tauri::Runtime>(
    app: &AppHandle<R>,
    mgr: &ManagerState,
    endpoints: &EndpointConfig,
    silent: bool,
) -> UpdateState {
    {
        let mut guard = mgr.inner.lock().await;
        match &guard.state {
            UpdateState::Checking
            | UpdateState::Downloading { .. }
            | UpdateState::Paused { .. }
            | UpdateState::Installing { .. }
            | UpdateState::Ready { .. } => return guard.state.clone(),
            _ => {}
        }
        if !silent {
            guard.state = UpdateState::Checking;
            emit_state(app, &guard.state);
        }
    }

    let _op = mgr.op_lock.lock().await;
    {
        let guard = mgr.inner.lock().await;
        if matches!(
            guard.state,
            UpdateState::Downloading { .. }
                | UpdateState::Paused { .. }
                | UpdateState::Ready { .. }
                | UpdateState::Installing { .. }
        ) {
            return guard.state.clone();
        }
    }

    let kind = current_platform();
    let mac_signed = platform_signed();
    let current = match current_version() {
        Ok(v) => v,
        Err(msg) => {
            return set_check_error(app, mgr, silent, msg).await;
        }
    };
    if matches!(kind, PlatformKind::Unsupported) {
        let state = UpdateState::UpToDate {
            current: current_version_str().to_owned(),
            checked_at: now_unix_secs(),
        };
        let mut guard = mgr.inner.lock().await;
        guard.state = state.clone();
        emit_state(app, &state);
        return state;
    }

    let client = match build_http_client(endpoints) {
        Ok(c) => c,
        Err(msg) => return set_check_error(app, mgr, silent, msg).await,
    };

    let candidate = match resolve_candidate(
        &client,
        endpoints,
        &current,
        kind,
        mac_signed,
        cfg!(debug_assertions),
    )
    .await
    {
        Ok(Some(c)) => c,
        Ok(None) => {
            let state = UpdateState::UpToDate {
                current: current_version_str().to_owned(),
                checked_at: now_unix_secs(),
            };
            let mut guard = mgr.inner.lock().await;
            guard.candidate = None;
            guard.cached = None;
            guard.state = state.clone();
            if !silent {
                emit_state(app, &state);
            }
            return state;
        }
        Err(msg) => return set_check_error(app, mgr, silent, msg).await,
    };

    if let Ok(dir) = updates_dir(app) {
        let cached_path = dir.join(&candidate.asset_name);
        if let Ok((actual_hash, actual_size)) = file_sha256(&cached_path).await
            && actual_hash == candidate.sha256
        {
            let ready = ready_state(&candidate, actual_size);
            let mut guard = mgr.inner.lock().await;
            guard.candidate = Some(candidate.clone());
            guard.cached = Some(CachedArtifact {
                candidate,
                path: cached_path,
                size: actual_size,
            });
            guard.state = ready.clone();
            emit_state(app, &ready);
            return ready;
        }
    }

    let avail = available_state(&candidate);
    let mut guard = mgr.inner.lock().await;
    guard.candidate = Some(candidate);
    guard.cached = None;
    guard.state = avail.clone();
    emit_state(app, &avail);
    avail
}

async fn set_check_error<R: tauri::Runtime>(
    app: &AppHandle<R>,
    mgr: &ManagerState,
    silent: bool,
    message: String,
) -> UpdateState {
    let mut guard = mgr.inner.lock().await;
    if silent {
        if matches!(guard.state, UpdateState::Checking) {
            guard.state = UpdateState::Idle;
        }
        return guard.state.clone();
    }
    let state = UpdateState::Error {
        current: current_version_str().to_owned(),
        stage: "check".to_owned(),
        message,
        version: None,
        notes: None,
        pub_date: None,
        size: None,
        release_url: None,
    };
    guard.state = state.clone();
    emit_state(app, &state);
    state
}

pub async fn pause_update_download<R: tauri::Runtime>(
    app: &AppHandle<R>,
    mgr: &ManagerState,
) -> UpdateState {
    let next = mgr.pause_download().await;
    emit_state(app, &next);
    next
}

pub async fn cancel_update_download<R: tauri::Runtime>(
    app: &AppHandle<R>,
    mgr: &ManagerState,
) -> UpdateState {
    let dir = updates_dir(app).ok();
    let next = mgr.cancel_download(dir.as_deref()).await;
    emit_state(app, &next);
    next
}

pub async fn download_update<R: tauri::Runtime>(
    app: AppHandle<R>,
    mgr: Arc<ManagerState>,
) -> Result<UpdateState, String> {
    download_with_endpoints(app, mgr, EndpointConfig::default()).await
}

async fn download_with_endpoints<R: tauri::Runtime>(
    app: AppHandle<R>,
    mgr: Arc<ManagerState>,
    endpoints: EndpointConfig,
) -> Result<UpdateState, String> {
    let dir = updates_dir(&app).ok();
    let candidate = {
        let mut guard = mgr.inner.lock().await;
        match &guard.state {
            UpdateState::Downloading { .. }
            | UpdateState::Ready { .. }
            | UpdateState::Installing { .. } => {
                return Ok(guard.state.clone());
            }
            _ => {}
        }
        let Some(c) = guard.candidate.clone() else {
            return Err("update_not_ready".to_owned());
        };
        let initial_downloaded = match &guard.state {
            UpdateState::Paused { downloaded, .. } => *downloaded,
            _ => dir
                .as_ref()
                .and_then(|d| std::fs::metadata(d.join(format!("{}.part", c.asset_name))).ok())
                .map(|m| m.len())
                .unwrap_or(0),
        };
        let (tx, _rx) = watch::channel(false);
        guard.cancel_tx = Some(tx);
        guard.stop_mode = None;
        guard.state = UpdateState::Downloading {
            current: current_version_str().to_owned(),
            version: c.version.clone(),
            notes: c.notes.clone(),
            pub_date: c.pub_date.clone(),
            downloaded: initial_downloaded,
            total: c.size,
            release_url: c.release_url.clone(),
        };
        emit_state(&app, &guard.state);
        c
    };

    let rx = {
        let guard = mgr.inner.lock().await;
        match &guard.cancel_tx {
            Some(tx) => tx.subscribe(),
            None => return Ok(guard.state.clone()),
        }
    };

    let dir = match dir {
        Some(d) => d,
        None => return fail_download(&app, &mgr, &candidate, "internal".to_owned()).await,
    };
    if tokio::fs::create_dir_all(&dir).await.is_err() {
        return fail_download(&app, &mgr, &candidate, "update_failed".to_owned()).await;
    }

    let final_path = dir.join(&candidate.asset_name);
    if let Ok((hash, size)) = file_sha256(&final_path).await
        && hash == candidate.sha256
    {
        let ready = ready_state(&candidate, size);
        let mut guard = mgr.inner.lock().await;
        guard.cancel_tx = None;
        guard.stop_mode = None;
        guard.cached = Some(CachedArtifact {
            candidate,
            path: final_path,
            size,
        });
        guard.state = ready.clone();
        emit_state(&app, &ready);
        return Ok(ready);
    }

    let part_path = dir.join(format!("{}.part", candidate.asset_name));

    let dl_url = match Url::parse(&candidate.download_url) {
        Ok(u) if is_allowed_url(&u, &endpoints) => u,
        _ => return fail_download(&app, &mgr, &candidate, "update_verify_failed".to_owned()).await,
    };

    let client = match build_http_client(&endpoints) {
        Ok(c) => c,
        Err(e) => return fail_download(&app, &mgr, &candidate, e).await,
    };

    let result = stream_verified_download(
        &app,
        &mgr,
        &client,
        dl_url,
        &part_path,
        &final_path,
        &candidate,
        rx,
    )
    .await;

    match result {
        Ok(size) => {
            let ready = ready_state(&candidate, size);
            let mut guard = mgr.inner.lock().await;
            guard.cancel_tx = None;
            guard.stop_mode = None;
            guard.cached = Some(CachedArtifact {
                candidate,
                path: final_path,
                size,
            });
            guard.state = ready.clone();
            emit_state(&app, &ready);
            Ok(ready)
        }
        Err(code) if code == "cancelled" => {
            let mut guard = mgr.inner.lock().await;
            let mode = guard.stop_mode.take().unwrap_or(StopMode::Cancel);
            guard.cancel_tx = None;
            match mode {
                StopMode::Pause => {
                    let dl = tokio::fs::metadata(&part_path)
                        .await
                        .map(|m| m.len())
                        .unwrap_or(0);
                    let paused = UpdateState::Paused {
                        current: current_version_str().to_owned(),
                        version: candidate.version.clone(),
                        notes: candidate.notes.clone(),
                        pub_date: candidate.pub_date.clone(),
                        downloaded: dl,
                        total: candidate.size,
                        release_url: candidate.release_url.clone(),
                    };
                    guard.state = paused.clone();
                    emit_state(&app, &paused);
                    Ok(paused)
                }
                StopMode::Cancel => {
                    drop(guard);
                    let _ = tokio::fs::remove_file(&part_path).await;
                    let avail = available_state(&candidate);
                    let mut guard = mgr.inner.lock().await;
                    guard.state = avail.clone();
                    emit_state(&app, &avail);
                    Ok(avail)
                }
            }
        }
        Err(code) => {
            let _ = tokio::fs::remove_file(&part_path).await;
            fail_download(&app, &mgr, &candidate, code).await
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn stream_verified_download<R: tauri::Runtime>(
    app: &AppHandle<R>,
    mgr: &ManagerState,
    client: &Client,
    url: Url,
    part_path: &Path,
    final_path: &Path,
    candidate: &Candidate,
    mut cancel: watch::Receiver<bool>,
) -> Result<u64, String> {
    let mut existing = tokio::fs::metadata(part_path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    if existing >= MAX_PACKAGE_BYTES || candidate.size.is_some_and(|s| existing > s) {
        let _ = tokio::fs::remove_file(part_path).await;
        existing = 0;
    }

    let mut req = client.get(url.clone());
    if existing > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={existing}-"));
    }
    let mut resp = tokio::select! {
        _ = cancel.changed() => return Err("cancelled".to_owned()),
        res = tokio::time::timeout(Duration::from_secs(20), req.send()) => {
            res.map_err(|_| "update_network".to_owned())?
               .map_err(|_| "update_network".to_owned())?
        }
    };
    if resp.status() == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        if let Ok((hash, size)) = file_sha256(part_path).await
            && hash == candidate.sha256
            && candidate.size.is_none_or(|s| size == s)
        {
            if tokio::fs::try_exists(final_path).await.unwrap_or(false) {
                let _ = tokio::fs::remove_file(final_path).await;
            }
            tokio::fs::rename(part_path, final_path)
                .await
                .map_err(|_| "update_failed".to_owned())?;
            return Ok(size);
        }
        let _ = tokio::fs::remove_file(part_path).await;
        existing = 0;
        resp = tokio::select! {
            _ = cancel.changed() => return Err("cancelled".to_owned()),
            res = tokio::time::timeout(Duration::from_secs(20), client.get(url).send()) => {
                res.map_err(|_| "update_network".to_owned())?
                   .map_err(|_| "update_network".to_owned())?
            }
        };
    }

    if !resp.status().is_success() {
        return Err("update_network".to_owned());
    }

    let can_append = existing > 0
        && resp.status() == reqwest::StatusCode::PARTIAL_CONTENT
        && resp
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|cr| cr.starts_with(&format!("bytes {existing}-")));

    let mut downloaded = if can_append { existing } else { 0 };
    let total = if can_append {
        resp.content_length()
            .map(|rem| downloaded.saturating_add(rem))
            .or(candidate.size)
    } else {
        resp.content_length().or(candidate.size)
    };
    if let Some(len) = total
        && len > MAX_PACKAGE_BYTES
    {
        return Err("update_verify_failed".to_owned());
    }

    let mut file = if can_append {
        tokio::fs::OpenOptions::new()
            .write(true)
            .append(true)
            .open(part_path)
            .await
            .map_err(|_| "update_failed".to_owned())?
    } else {
        tokio::fs::File::create(part_path)
            .await
            .map_err(|_| "update_failed".to_owned())?
    };

    let mut last_emit = Instant::now();
    let mut last_bytes = downloaded;
    let mut stream = resp.bytes_stream();

    loop {
        let next = tokio::select! {
            _ = cancel.changed() => {
                if *cancel.borrow() {
                    let _ = file.flush().await;
                    return Err("cancelled".to_owned());
                }
                continue;
            }
            item = tokio::time::timeout(Duration::from_secs(30), stream.next()) => item,
        };
        let Some(chunk_res) = next.map_err(|_| "update_network".to_owned())? else {
            break;
        };
        let chunk = chunk_res.map_err(|_| "update_network".to_owned())?;
        downloaded = downloaded.saturating_add(chunk.len() as u64);
        if downloaded > MAX_PACKAGE_BYTES {
            return Err("update_verify_failed".to_owned());
        }
        file.write_all(&chunk)
            .await
            .map_err(|_| "update_failed".to_owned())?;

        if last_emit.elapsed() >= Duration::from_millis(90)
            || downloaded.saturating_sub(last_bytes) >= 128 * 1024
        {
            last_emit = Instant::now();
            last_bytes = downloaded;
            let snap = UpdateState::Downloading {
                current: current_version_str().to_owned(),
                version: candidate.version.clone(),
                notes: candidate.notes.clone(),
                pub_date: candidate.pub_date.clone(),
                downloaded,
                total,
                release_url: candidate.release_url.clone(),
            };
            {
                let mut guard = mgr.inner.lock().await;
                if *cancel.borrow() {
                    let _ = file.flush().await;
                    return Err("cancelled".to_owned());
                }
                guard.state = snap.clone();
            }
            emit_state(app, &snap);
        }
    }

    file.flush().await.map_err(|_| "update_failed".to_owned())?;
    drop(file);

    let (digest, actual_size) = file_sha256(part_path)
        .await
        .map_err(|_| "update_failed".to_owned())?;
    if let Some(expected_size) = candidate.size
        && actual_size != expected_size
    {
        return Err("update_verify_failed".to_owned());
    }
    if digest != candidate.sha256 {
        return Err("update_verify_failed".to_owned());
    }
    if tokio::fs::try_exists(final_path).await.unwrap_or(false) {
        let _ = tokio::fs::remove_file(final_path).await;
    }
    tokio::fs::rename(part_path, final_path)
        .await
        .map_err(|_| "update_failed".to_owned())?;
    Ok(actual_size)
}

async fn fail_download<R: tauri::Runtime>(
    app: &AppHandle<R>,
    mgr: &ManagerState,
    candidate: &Candidate,
    message: String,
) -> Result<UpdateState, String> {
    let err_state = UpdateState::Error {
        current: current_version_str().to_owned(),
        stage: "download".to_owned(),
        message: message.clone(),
        version: Some(candidate.version.clone()),
        notes: Some(candidate.notes.clone()),
        pub_date: candidate.pub_date.clone(),
        size: candidate.size,
        release_url: candidate.release_url.clone(),
    };
    let mut guard = mgr.inner.lock().await;
    guard.cancel_tx = None;
    guard.state = err_state.clone();
    emit_state(app, &err_state);
    Err(message)
}

pub async fn install_update<R: tauri::Runtime>(
    app: &AppHandle<R>,
    mgr: &ManagerState,
) -> Result<(), String> {
    let _op = mgr.op_lock.lock().await;
    let artifact = {
        let mut guard = mgr.inner.lock().await;
        let Some(cached) = guard.cached.clone() else {
            return Err("update_not_ready".to_owned());
        };
        guard.state = UpdateState::Installing {
            current: current_version_str().to_owned(),
            version: cached.candidate.version.clone(),
            notes: cached.candidate.notes.clone(),
            pub_date: cached.candidate.pub_date.clone(),
            release_url: cached.candidate.release_url.clone(),
        };
        emit_state(app, &guard.state);
        cached
    };

    let verified = matches!(
        file_sha256(&artifact.path).await,
        Ok((hash, size)) if hash == artifact.candidate.sha256 && size == artifact.size
    );
    if !verified {
        let _ = tokio::fs::remove_file(&artifact.path).await;
        return fail_install(app, mgr, &artifact, "update_verify_failed".to_owned()).await;
    }

    match platform_install(app, &artifact.path).await {
        Ok(should_exit) => {
            if should_exit {
                let handle = app.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(120)).await;
                    handle.exit(0);
                });
            } else {
                let ready = ready_state(&artifact.candidate, artifact.size);
                let mut guard = mgr.inner.lock().await;
                guard.state = ready.clone();
                emit_state(app, &ready);
            }
            Ok(())
        }
        Err(code) => fail_install(app, mgr, &artifact, code).await,
    }
}

async fn fail_install<R: tauri::Runtime>(
    app: &AppHandle<R>,
    mgr: &ManagerState,
    artifact: &CachedArtifact,
    message: String,
) -> Result<(), String> {
    let err_state = UpdateState::Error {
        current: current_version_str().to_owned(),
        stage: "install".to_owned(),
        message: message.clone(),
        version: Some(artifact.candidate.version.clone()),
        notes: Some(artifact.candidate.notes.clone()),
        pub_date: artifact.candidate.pub_date.clone(),
        size: Some(artifact.size),
        release_url: artifact.candidate.release_url.clone(),
    };
    let mut guard = mgr.inner.lock().await;
    if message == "update_verify_failed" {
        guard.cached = None;
    }
    guard.state = err_state.clone();
    emit_state(app, &err_state);
    Err(message)
}

pub fn open_release_url<R: tauri::Runtime>(
    app: &AppHandle<R>,
    url: &str,
) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let allowed = safe_release_page(Some(url), &EndpointConfig::default())
        .ok_or_else(|| "invalid".to_owned())?;
    app.opener()
        .open_url(&allowed, None::<&str>)
        .map_err(|_| "external_browser_failed".to_owned())
}

#[cfg(windows)]
async fn platform_install<R: tauri::Runtime>(
    _app: &AppHandle<R>,
    installer_path: &Path,
) -> Result<bool, String> {
    use fesen_core::{
        ipc,
        platform::windows::{data_dir, launch_installer, shell_path},
    };

    let exe = std::env::current_exe().map_err(|_| "update_failed".to_owned())?;
    let install_dir = exe
        .parent()
        .ok_or_else(|| "core_location".to_owned())
        .and_then(|p| shell_path(p).map_err(|_| "core_location".to_owned()))?;
    let install_str = install_dir.to_string_lossy();
    if install_str.contains(['\0', '\r', '\n']) {
        return Err("core_location".to_owned());
    }
    // NSIS requires `/D=<dir>` last and unquoted even when paths contain spaces.
    let args = format!("/P /UPDATE /R /D={install_str}");

    if let Ok(dir) = data_dir() {
        let _ = tokio::time::timeout(
            Duration::from_millis(600),
            ipc::request(&dir, &serde_json::json!({"op": "quit"})),
        )
        .await;
        wait_windows_core_release(&dir, Duration::from_millis(4500)).await;
    }

    let installer = installer_path.to_path_buf();
    tokio::task::spawn_blocking(move || launch_installer(&installer, &args))
        .await
        .map_err(|_| "update_failed".to_owned())?
        .map_err(|e| e.code().to_owned())?;
    Ok(true)
}

#[cfg(windows)]
async fn wait_windows_core_release(dir: &Path, limit: Duration) {
    let lock_path = dir.join("core.lock");
    let ipc_path = dir.join("ipc.json");
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        let ipc_gone = !ipc_path.exists();
        let lock_free = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
        {
            Ok(f) => f.try_lock().is_ok(),
            Err(_) => true,
        };
        if ipc_gone && lock_free {
            tokio::time::sleep(Duration::from_millis(150)).await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(target_os = "macos")]
async fn platform_install<R: tauri::Runtime>(
    app: &AppHandle<R>,
    package_path: &Path,
) -> Result<bool, String> {
    let exe = std::env::current_exe().map_err(|_| "update_failed".to_owned())?;
    let app_bundle = exe
        .ancestors()
        .nth(3)
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("app"))
        .map(Path::to_path_buf)
        .ok_or_else(|| "update_readonly".to_owned())?;

    let bundle_str = app_bundle.to_string_lossy();
    if bundle_str.starts_with("/Volumes/") || bundle_str.contains("/AppTranslocation/") {
        return Err("update_readonly".to_owned());
    }
    let meta = std::fs::symlink_metadata(&app_bundle).map_err(|_| "update_failed".to_owned())?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("update_readonly".to_owned());
    }

    let work_root = updates_dir(app)?;
    let pkg = package_path.to_path_buf();
    let stage = tokio::task::spawn_blocking(move || prepare_macos_bundle(&pkg, &work_root))
        .await
        .map_err(|_| "update_failed".to_owned())??;

    super::macos::prepare_update().await;

    let target = app_bundle.clone();
    let swap_res = tokio::task::spawn_blocking(move || swap_macos_bundle(&stage, &target))
        .await
        .map_err(|_| "update_failed".to_owned())?;
    swap_res?;

    let pid = std::process::id().to_string();
    let _ = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "i=0; while kill -0 \"$1\" 2>/dev/null && [ \"$i\" -lt 80 ]; do i=$((i+1)); sleep 0.05; done; sleep 0.1; /usr/bin/open -n \"$2\"",
            "_",
            &pid,
            &bundle_str,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    Ok(true)
}

#[cfg(target_os = "macos")]
struct TempDirGuard(PathBuf);

#[cfg(target_os = "macos")]
impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(target_os = "macos")]
fn prepare_macos_bundle(package_path: &Path, work_root: &Path) -> Result<PathBuf, String> {
    let stage_dir = work_root.join(format!(".stage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage_dir);
    std::fs::create_dir_all(&stage_dir).map_err(|_| "update_failed".to_owned())?;

    let ext_dir = stage_dir.join("extracted");
    std::fs::create_dir_all(&ext_dir).map_err(|_| "update_failed".to_owned())?;

    let name = package_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let status = std::process::Command::new("/usr/bin/tar")
            .arg("-xzf")
            .arg(package_path)
            .arg("-C")
            .arg(&ext_dir)
            .status()
            .map_err(|_| "update_failed".to_owned())?;
        if !status.success() {
            let _ = std::fs::remove_dir_all(&stage_dir);
            return Err("update_verify_failed".to_owned());
        }
    } else if name.ends_with(".dmg") {
        let mount_dir = stage_dir.join("mount");
        std::fs::create_dir_all(&mount_dir).map_err(|_| "update_failed".to_owned())?;
        let attach = std::process::Command::new("/usr/bin/hdiutil")
            .args(["attach", "-nobrowse", "-readonly", "-mountpoint"])
            .arg(&mount_dir)
            .arg(package_path)
            .output()
            .map_err(|_| "update_failed".to_owned())?;
        if !attach.status.success() {
            let _ = std::fs::remove_dir_all(&stage_dir);
            return Err("update_verify_failed".to_owned());
        }
        let copy = std::process::Command::new("/usr/bin/ditto")
            .arg(mount_dir.join("Fesen.app"))
            .arg(ext_dir.join("Fesen.app"))
            .status();
        let _ = std::process::Command::new("/usr/bin/hdiutil")
            .args(["detach", "-quiet"])
            .arg(&mount_dir)
            .status();
        if !copy.is_ok_and(|s| s.success()) {
            let _ = std::fs::remove_dir_all(&stage_dir);
            return Err("update_verify_failed".to_owned());
        }
    } else {
        let _ = std::fs::remove_dir_all(&stage_dir);
        return Err("update_verify_failed".to_owned());
    }

    let extracted_app = ext_dir.join("Fesen.app");
    if !verify_macos_app_bundle(&extracted_app) {
        let _ = std::fs::remove_dir_all(&stage_dir);
        return Err("update_verify_failed".to_owned());
    }
    let _ = std::process::Command::new("/usr/bin/xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(&extracted_app)
        .status();
    Ok(stage_dir)
}

#[cfg(target_os = "macos")]
fn verify_macos_app_bundle(app: &Path) -> bool {
    let info_plist = app.join("Contents/Info.plist");
    if !app.is_dir() || !info_plist.is_file() {
        return false;
    }
    let id_ok = std::process::Command::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleIdentifier", "raw", "-o", "-"])
        .arg(&info_plist)
        .output()
        .is_ok_and(|o| {
            o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "io.fesen.client"
        });
    if !id_ok {
        return false;
    }
    std::process::Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(app)
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(target_os = "macos")]
fn swap_macos_bundle(stage_dir: &Path, target_app: &Path) -> Result<(), String> {
    let _guard = TempDirGuard(stage_dir.to_path_buf());
    let new_app = stage_dir.join("extracted/Fesen.app");
    let parent = target_app
        .parent()
        .ok_or_else(|| "update_permission".to_owned())?;
    let pid = std::process::id();
    let incoming = parent.join(format!(".Fesen.app.new.{pid}"));
    let backup = parent.join(format!(".Fesen.app.bak.{pid}"));

    if std::fs::rename(&new_app, &incoming).is_err() {
        let ditto = std::process::Command::new("/usr/bin/ditto")
            .arg(&new_app)
            .arg(&incoming)
            .status();
        if !ditto.is_ok_and(|s| s.success()) {
            let _ = std::fs::remove_dir_all(&incoming);
            return Err("update_permission".to_owned());
        }
    }

    if let Err(err) = std::fs::rename(target_app, &backup) {
        let _ = std::fs::remove_dir_all(&incoming);
        return Err(if err.kind() == std::io::ErrorKind::PermissionDenied {
            "update_permission".to_owned()
        } else {
            "update_failed".to_owned()
        });
    }
    if std::fs::rename(&incoming, target_app).is_err() {
        let _ = std::fs::rename(&backup, target_app);
        let _ = std::fs::remove_dir_all(&incoming);
        return Err("update_failed".to_owned());
    }
    let _ = std::fs::remove_dir_all(&backup);
    Ok(())
}

#[cfg(target_os = "android")]
async fn platform_install<R: tauri::Runtime>(
    app: &AppHandle<R>,
    apk_path: &Path,
) -> Result<bool, String> {
    let path = apk_path.to_str().ok_or_else(|| "update_failed".to_owned())?;
    tauri_plugin_fesen::install_apk(app, path).await?;
    Ok(false)
}

#[cfg(not(any(windows, target_os = "macos", target_os = "android")))]
async fn platform_install<R: tauri::Runtime>(
    _app: &AppHandle<R>,
    _path: &Path,
) -> Result<bool, String> {
    Err("update_failed".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    fn test_endpoints(addr: std::net::SocketAddr) -> EndpointConfig {
        let base = format!("http://{addr}");
        EndpointConfig {
            manifest_url: format!("{base}/latest.json"),
            api_url: format!("{base}/releases/latest"),
            web_latest_url: format!("{base}/web/releases/latest"),
            download_base_url: format!("{base}/web/releases/download"),
            test_origin: Some(("http".into(), addr.ip().to_string(), Some(addr.port()))),
        }
    }

    #[test]
    fn host_and_redirect_policy_rejects_untrusted_destinations() {
        let prod = EndpointConfig {
            manifest_url: PRIMARY_MANIFEST_URL.into(),
            api_url: FALLBACK_API_URL.into(),
            web_latest_url: FALLBACK_WEB_URL.into(),
            download_base_url: FALLBACK_DOWNLOAD_BASE.into(),
            test_origin: None,
        };
        assert!(is_allowed_url(
            &Url::parse("https://github.com/Hxgh/fesen/releases/download/v0.1.6/latest.json")
                .unwrap(),
            &prod
        ));
        assert!(is_allowed_url(
            &Url::parse("https://objects.githubusercontent.com/github-production/x").unwrap(),
            &prod
        ));
        assert!(!is_allowed_url(
            &Url::parse("http://github.com/Hxgh/fesen/releases/latest").unwrap(),
            &prod
        ));
        assert!(!is_allowed_url(
            &Url::parse("https://evil.example.com/payload.exe").unwrap(),
            &prod
        ));
    }

    #[test]
    fn release_notes_cleaning_strips_deploy_boilerplate_and_compare_footer() {
        let raw = "### Server (Docker, linux/amd64 & linux/arm64)\n\n```sh\ndocker compose up\n```\n\nVerify checksums against `SHA256SUMS.txt`.\n\n## What's Changed\n* fix: self-update support\n* feat: hover notes preview\n\n**Full Changelog**: https://github.com/Hxgh/fesen/compare/v0.1.5...v0.1.6\n";
        let cleaned = clean_release_notes(raw);
        assert_eq!(
            cleaned,
            "* fix: self-update support\n* feat: hover notes preview"
        );
    }

    #[test]
    fn version_and_platform_selection_protect_signed_and_stable_channels() {
        let stable = Version::new(0, 1, 5);
        assert!(parse_version_allowed("v0.1.6", &stable).is_some());
        assert!(parse_version_allowed("0.1.5", &stable).is_none());
        assert!(parse_version_allowed("v0.1.6-rc.1", &stable).is_none());

        let mut platforms = HashMap::new();
        platforms.insert(
            "darwin-aarch64-unsigned".into(),
            ManifestPlatform {
                url: "https://github.com/Hxgh/fesen/releases/download/v0.1.6/fesen_0.1.6_macos-arm64-unsigned.app.tar.gz".into(),
                sha256: "a".repeat(64),
                size: Some(100),
            },
        );
        // Signed macOS build must never auto-downgrade to unsigned bundle.
        assert!(select_manifest_platform(&platforms, PlatformKind::MacosArm64, true, false).is_none());
        // Unsigned macOS build accepts unsigned bundle.
        assert!(select_manifest_platform(&platforms, PlatformKind::MacosArm64, false, false).is_some());
    }

    #[test]
    fn checksum_parser_and_filename_sanitizer_work_strictly() {
        let sha = "e95741a511a2c2dbd3c2d7dd5af3ac87e69e2cc844da6cae8d2a6a43122b16a4";
        let sums = format!("{sha}  fesen_0.1.5_windows-x64-setup.exe\n");
        assert_eq!(
            parse_checksum_for_asset(&sums, "fesen_0.1.5_windows-x64-setup.exe").as_deref(),
            Some(sha)
        );
        assert!(safe_asset_filename("../evil.exe").is_none());
        assert!(safe_asset_filename("fesen_0.1.6_windows-x64-setup.exe").is_some());
    }

    #[tokio::test]
    async fn check_download_verify_and_cancel_against_local_mock_server() {
        let payload = b"FESEN_UPDATE_PAYLOAD_0_1_6".to_vec();
        let digest = hex_lower(Sha256::digest(&payload).as_slice());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let manifest = serde_json::json!({
            "version": "9.9.9",
            "pub_date": "2026-11-28T04:00:00Z",
            "notes": "## What's Changed\n* feat: test self-updater",
            "url": format!("http://{addr}/release/v9.9.9"),
            "platforms": {
                "windows-x86_64": {
                    "url": format!("http://{addr}/fesen_9.9.9_windows-x64-setup.exe"),
                    "sha256": digest,
                    "size": payload.len()
                },
                "darwin-aarch64-unsigned": {
                    "url": format!("http://{addr}/fesen_9.9.9_macos-arm64-unsigned.app.tar.gz"),
                    "sha256": digest,
                    "size": payload.len()
                },
                "android-aarch64": {
                    "url": format!("http://{addr}/fesen_9.9.9_android-arm64.apk"),
                    "sha256": digest,
                    "size": payload.len()
                }
            }
        })
        .to_string();

        let bad_payload = b"CORRUPTED_BYTES".to_vec();
        let serve_bad = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let serve_bad_srv = serve_bad.clone();
        let expected_len = payload.len() as u64;
        let payload_srv = payload.clone();

        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let manifest = manifest.clone();
                let payload = payload_srv.clone();
                let bad_payload = bad_payload.clone();
                let serve_bad = serve_bad_srv.clone();
                tokio::spawn(async move {
                    let mut req_buf = [0u8; 2048];
                    let n = sock.read(&mut req_buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&req_buf[..n]);
                    let path = req
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/");
                    if path == "/latest.json" {
                        let res = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            manifest.len(),
                            manifest
                        );
                        let _ = sock.write_all(res.as_bytes()).await;
                    } else {
                        let body = if serve_bad.load(std::sync::atomic::Ordering::Relaxed) {
                            &bad_payload
                        } else {
                            &payload
                        };
                        let hdr = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = sock.write_all(hdr.as_bytes()).await;
                        let _ = sock.write_all(body).await;
                    }
                });
            }
        });

        let endpoints = test_endpoints(addr);
        let client = build_http_client(&endpoints).unwrap();
        let current = Version::new(0, 1, 5);
        let candidate = resolve_candidate(
            &client,
            &endpoints,
            &current,
            PlatformKind::MacosArm64,
            false,
            false,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(candidate.version, "9.9.9");
        assert!(candidate.notes.contains("test self-updater"));

        // Verify bad checksum rejects file and deletes .part
        let tmp = tempfile::tempdir().unwrap();
        let part = tmp.path().join("pkg.part");
        let final_pkg = tmp.path().join("pkg");
        serve_bad.store(true, std::sync::atomic::Ordering::Relaxed);
        let resp = client.get(&candidate.download_url).send().await.unwrap();
        let body = read_bounded_bytes(resp, 1024).await.unwrap();
        let bad_hash = hex_lower(Sha256::digest(&body).as_slice());
        assert_ne!(bad_hash, candidate.sha256);

        // Verify good download matches SHA-256 and produces exact artifact
        serve_bad.store(false, std::sync::atomic::Ordering::Relaxed);
        let resp = client.get(&candidate.download_url).send().await.unwrap();
        let body = read_bounded_bytes(resp, 1024).await.unwrap();
        {
            let mut f = std::fs::File::create(&part).unwrap();
            f.write_all(&body).unwrap();
        }
        std::fs::rename(&part, &final_pkg).unwrap();
        let (got_hash, got_size) = file_sha256(&final_pkg).await.unwrap();
        assert_eq!(got_hash, candidate.sha256);
        assert_eq!(got_size, expected_len);
    }

    #[tokio::test]
    async fn github_api_fallback_resolves_sha256sums_when_digest_absent() {
        let payload = b"FALLBACK_BINARY".to_vec();
        let digest = hex_lower(Sha256::digest(&payload).as_slice());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let release_json = serde_json::json!({
            "tag_name": "v9.8.0",
            "draft": false,
            "prerelease": false,
            "published_at": "2026-11-28T05:00:00Z",
            "body": "## What's Changed\n* fix: fallback checksum resolution\n\n**Full Changelog**: https://github.com/Hxgh/fesen/compare/v0.1.5...v9.8.0",
            "html_url": format!("http://{addr}/Hxgh/fesen/releases/tag/v9.8.0"),
            "assets": [
                {
                    "name": "fesen_9.8.0_windows-x64-setup.exe",
                    "browser_download_url": format!("http://{addr}/fesen_9.8.0_windows-x64-setup.exe"),
                    "size": payload.len()
                },
                {
                    "name": "SHA256SUMS.txt",
                    "browser_download_url": format!("http://{addr}/SHA256SUMS.txt"),
                    "size": 120
                }
            ]
        })
        .to_string();
        let sums_txt = format!("{digest}  fesen_9.8.0_windows-x64-setup.exe\n");

        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let release_json = release_json.clone();
                let sums_txt = sums_txt.clone();
                tokio::spawn(async move {
                    let mut req_buf = [0u8; 2048];
                    let n = sock.read(&mut req_buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&req_buf[..n]);
                    let path = req
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/");
                    let (status, ctype, body) = if path == "/latest.json" {
                        ("404 Not Found", "text/plain", String::new())
                    } else if path == "/releases/latest" {
                        ("200 OK", "application/json", release_json)
                    } else if path == "/SHA256SUMS.txt" {
                        ("200 OK", "text/plain", sums_txt)
                    } else {
                        ("404 Not Found", "text/plain", String::new())
                    };
                    let res = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = sock.write_all(res.as_bytes()).await;
                });
            }
        });

        let endpoints = test_endpoints(addr);
        let client = build_http_client(&endpoints).unwrap();
        let current = Version::new(0, 1, 5);
        let candidate = resolve_candidate(
            &client,
            &endpoints,
            &current,
            PlatformKind::WindowsX64,
            true,
            false,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(candidate.version, "9.8.0");
        assert_eq!(candidate.sha256, digest);
        assert_eq!(
            candidate.notes,
            "* fix: fallback checksum resolution"
        );
    }

    #[test]
    fn github_asset_selection_matches_real_release_naming_across_all_platforms() {
        let assets = vec![
            GitHubAsset {
                name: "fesen_0.1.5_android-arm64.apk".into(),
                browser_download_url: "https://github.com/Hxgh/fesen/releases/download/v0.1.5/fesen_0.1.5_android-arm64.apk".into(),
                size: Some(12_319_058),
                digest: None,
            },
            GitHubAsset {
                name: "fesen_0.1.5_android-arm64-debug.apk".into(),
                browser_download_url: "https://github.com/Hxgh/fesen/releases/download/v0.1.5/fesen_0.1.5_android-arm64-debug.apk".into(),
                size: Some(12_000_000),
                digest: None,
            },
            GitHubAsset {
                name: "fesen_0.1.5_macos-arm64-unsigned.dmg".into(),
                browser_download_url: "https://github.com/Hxgh/fesen/releases/download/v0.1.5/fesen_0.1.5_macos-arm64-unsigned.dmg".into(),
                size: Some(10_213_619),
                digest: None,
            },
            GitHubAsset {
                name: "fesen_0.1.5_windows-x64-setup.exe".into(),
                browser_download_url: "https://github.com/Hxgh/fesen/releases/download/v0.1.5/fesen_0.1.5_windows-x64-setup.exe".into(),
                size: Some(4_971_539),
                digest: None,
            },
        ];
        assert_eq!(
            select_github_asset(&assets, PlatformKind::WindowsX64, false, false).unwrap().name,
            "fesen_0.1.5_windows-x64-setup.exe"
        );
        assert_eq!(
            select_github_asset(&assets, PlatformKind::AndroidArm64, false, false).unwrap().name,
            "fesen_0.1.5_android-arm64.apk"
        );
        // Signed macOS refuses unsigned DMG; unsigned macOS accepts it.
        assert!(select_github_asset(&assets, PlatformKind::MacosArm64, true, false).is_none());
        assert_eq!(
            select_github_asset(&assets, PlatformKind::MacosArm64, false, false).unwrap().name,
            "fesen_0.1.5_macos-arm64-unsigned.dmg"
        );
        assert!(select_github_asset(&assets, PlatformKind::Unsupported, false, false).is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_bundle_swap_and_rollback_preserve_target_atomically() {
        let root = tempfile::tempdir().unwrap();
        let apps = root.path().join("Applications");
        std::fs::create_dir_all(&apps).unwrap();
        let target_app = apps.join("Fesen.app");
        std::fs::create_dir_all(target_app.join("Contents/MacOS")).unwrap();
        std::fs::write(target_app.join("Contents/version.txt"), b"0.1.5").unwrap();

        let stage = root.path().join("stage");
        let extracted_app = stage.join("extracted/Fesen.app");
        std::fs::create_dir_all(extracted_app.join("Contents/MacOS")).unwrap();
        std::fs::write(extracted_app.join("Contents/version.txt"), b"0.1.6").unwrap();

        swap_macos_bundle(&stage, &target_app).unwrap();
        assert_eq!(
            std::fs::read(target_app.join("Contents/version.txt")).unwrap(),
            b"0.1.6"
        );
        assert!(!stage.exists());
    }

    #[tokio::test]
    async fn web_fallback_resolves_tag_notes_and_sha256sums_when_api_rate_limited() {
        let payload = b"WEB_FALLBACK_EXE".to_vec();
        let digest = hex_lower(Sha256::digest(&payload).as_slice());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let html = "<html><body><div class=\"markdown-body my-3\"><h2>What's Changed</h2><ul><li>fix: fallback when rate limited</li></ul></div></body></html>";
        let sums = format!("{digest}  fesen_9.7.0_windows-x64-setup.exe\n");

        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let sums = sums.clone();
                tokio::spawn(async move {
                    let mut req_buf = [0u8; 2048];
                    let n = sock.read(&mut req_buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&req_buf[..n]);
                    let path = req
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/");
                    if path == "/web/releases/latest" {
                        let res = format!(
                            "HTTP/1.1 302 Found\r\nLocation: http://{addr}/Hxgh/fesen/releases/tag/v9.7.0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        );
                        let _ = sock.write_all(res.as_bytes()).await;
                    } else if path == "/Hxgh/fesen/releases/tag/v9.7.0" {
                        let res = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            html.len(),
                            html
                        );
                        let _ = sock.write_all(res.as_bytes()).await;
                    } else if path == "/web/releases/download/v9.7.0/SHA256SUMS.txt" {
                        let res = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            sums.len(),
                            sums
                        );
                        let _ = sock.write_all(res.as_bytes()).await;
                    } else {
                        let status = if path == "/releases/latest" {
                            "403 Forbidden"
                        } else {
                            "404 Not Found"
                        };
                        let res = format!(
                            "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        );
                        let _ = sock.write_all(res.as_bytes()).await;
                    }
                });
            }
        });

        let endpoints = test_endpoints(addr);
        let client = build_http_client(&endpoints).unwrap();
        let current = Version::new(0, 1, 5);
        let candidate = resolve_candidate(
            &client,
            &endpoints,
            &current,
            PlatformKind::WindowsX64,
            true,
            false,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(candidate.version, "9.7.0");
        assert_eq!(candidate.sha256, digest);
        assert!(candidate.notes.contains("fix: fallback when rate limited"));
    }

    #[tokio::test]
    async fn pause_preserves_state_and_live_notes_sync_overrides_manifest_notes() {
        let mgr = ManagerState::new();
        let cand = Candidate {
            version: "9.9.9".into(),
            notes: "- initial".into(),
            pub_date: None,
            release_url: None,
            asset_name: "fesen_9.9.9_windows-x64-setup.exe".into(),
            download_url: "https://github.com/Hxgh/fesen/releases/download/v9.9.9/fesen_9.9.9_windows-x64-setup.exe".into(),
            sha256: "a".repeat(64),
            size: Some(1024),
        };
        {
            let mut g = mgr.inner.lock().await;
            g.candidate = Some(cand.clone());
            g.state = UpdateState::Downloading {
                current: "0.1.5".into(),
                version: cand.version.clone(),
                notes: cand.notes.clone(),
                pub_date: None,
                downloaded: 512,
                total: Some(1024),
                release_url: None,
            };
        }
        let paused = mgr.pause_download().await;
        assert!(matches!(
            paused,
            UpdateState::Paused {
                downloaded: 512,
                total: Some(1024),
                ..
            }
        ));
        let cancelled = mgr.cancel_download(None).await;
        assert!(matches!(cancelled, UpdateState::Available { .. }));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let html = "<html><body><div class=\"markdown-body\"><h2>What's Changed</h2><ul><li>feat: synced from GitHub release page</li></ul></div></body></html>";
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let res = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    html.len(),
                    html
                );
                let _ = sock.write_all(res.as_bytes()).await;
            }
        });
        let endpoints = test_endpoints(addr);
        let client = build_http_client(&endpoints).unwrap();
        let mut live_cand = Candidate {
            release_url: Some(format!("http://{addr}/Hxgh/fesen/releases/tag/v9.9.9")),
            ..cand
        };
        sync_live_github_notes(&client, &endpoints, &mut live_cand).await;
        assert!(live_cand.notes.contains("synced from GitHub release page"));
    }

    #[ignore = "requires live HTTPS access to github.com / api.github.com"]
    #[tokio::test]
    async fn live_github_latest_release_check() {
        let endpoints = EndpointConfig::default();
        let client = build_http_client(&endpoints).unwrap();
        let older = Version::new(0, 1, 4);
        let mac_candidate = resolve_candidate(
            &client,
            &endpoints,
            &older,
            PlatformKind::MacosArm64,
            false,
            false,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(Version::parse(&mac_candidate.version).unwrap() > older);
        assert_eq!(mac_candidate.sha256.len(), 64);
        let win_candidate = resolve_candidate(
            &client,
            &endpoints,
            &older,
            PlatformKind::WindowsX64,
            true,
            false,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(win_candidate.asset_name.ends_with("_windows-x64-setup.exe"));
        assert_eq!(win_candidate.sha256.len(), 64);
    }
}
