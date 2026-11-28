use crate::routing::Label;
use fesen_proto::{CERT_NAME, Code, LocalMapping, Publication};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct AccountConfig {
    pub shares: Vec<Publication>,
    pub labels: Vec<Label>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forwards: Vec<LocalMapping>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_shares: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub share_network: String,
    #[serde(default = "default_true")]
    pub cloud_backup: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dirty_prefs: Vec<String>,
}

impl Default for AccountConfig {
    fn default() -> Self {
        Self {
            shares: Vec::new(),
            labels: Vec::new(),
            forwards: Vec::new(),
            active_shares: Vec::new(),
            share_network: String::new(),
            cloud_backup: true,
            dirty_prefs: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Local {
    pub server: String,
    pub identities: BTreeMap<String, Identity>,
    pub session: Option<Login>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    #[serde(default = "default_true")]
    pub auto_connect: bool,
    #[serde(default = "default_true")]
    pub auto_share: bool,
    pub accounts: BTreeMap<String, AccountConfig>,
    // Legacy flat fields retained solely for automatic one-time migration from older device.json.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shares: Vec<Publication>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<Label>,
}

impl Default for Local {
    fn default() -> Self {
        Self {
            server: String::new(),
            identities: BTreeMap::new(),
            session: None,
            device_name: None,
            auto_connect: true,
            auto_share: true,
            accounts: BTreeMap::new(),
            shares: Vec::new(),
            labels: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Login {
    pub member: String,
    pub token: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Identity {
    pub cert: Vec<u8>,
    key: Vec<u8>,
}

impl Identity {
    pub fn new() -> Result<Self, Code> {
        let cert = rcgen::generate_simple_self_signed(vec![CERT_NAME.to_owned()])
            .map_err(|_| Code::Internal)?;
        Ok(Self {
            cert: cert.cert.der().to_vec(),
            key: cert.signing_key.serialize_der(),
        })
    }

    pub fn chain(&self) -> Vec<CertificateDer<'static>> {
        vec![CertificateDer::from(self.cert.clone())]
    }

    pub fn key(&self) -> PrivateKeyDer<'static> {
        PrivatePkcs8KeyDer::from(self.key.clone()).into()
    }
}

pub fn private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

pub fn private_file() -> fs::OpenOptions {
    let mut options = fs::OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options
}

fn vetted(file: fs::File) -> io::Result<fs::File> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT, GetFileInformationByHandle,
        };
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 || info.nNumberOfLinks != 1 {
            return Err(io::Error::other("unexpected local state link"));
        }
    }
    Ok(file)
}

pub(crate) fn save(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", hex::encode(rand::random::<[u8; 8]>())));
    let result = (|| {
        let mut f = private_file().create_new(true).write(true).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}

impl Local {
    pub fn load(dir: &Path) -> io::Result<Self> {
        use io::Read;
        match private_file()
            .read(true)
            .open(dir.join("device.json"))
            .and_then(vetted)
            .and_then(|mut f| {
                let mut bytes = Vec::new();
                f.read_to_end(&mut bytes)?;
                Ok(bytes)
            }) {
            Ok(bytes) => {
                let mut local: Self = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                local.migrate_legacy();
                Ok(local)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    pub fn migrate_legacy(&mut self) {
        if self.shares.is_empty() && self.labels.is_empty() {
            return;
        }
        let target = self.session.as_ref().map(|s| s.member.clone()).or_else(|| {
            (self.identities.len() == 1)
                .then(|| self.identities.keys().next().cloned())
                .flatten()
        });
        if let Some(member) = target {
            self.migrate_to_member(&member);
        }
    }

    pub fn migrate_to_member(&mut self, member: &str) {
        if self.shares.is_empty() && self.labels.is_empty() {
            return;
        }
        let entry = self.accounts.entry(member.to_owned()).or_default();
        if entry.shares.is_empty() {
            entry.shares = std::mem::take(&mut self.shares);
        } else {
            self.shares.clear();
        }
        if entry.labels.is_empty() {
            entry.labels = std::mem::take(&mut self.labels);
        } else {
            self.labels.clear();
        }
    }

    pub fn active_member(&self) -> Option<&str> {
        self.session.as_ref().map(|s| s.member.as_str())
    }

    pub fn shares(&self) -> &[Publication] {
        self.active_member()
            .and_then(|m| self.accounts.get(m))
            .map_or(&[], |a| a.shares.as_slice())
    }

    pub fn shares_mut(&mut self) -> Result<&mut Vec<Publication>, Code> {
        let member = self
            .active_member()
            .ok_or(Code::Unauthorized)?
            .to_owned();
        Ok(&mut self.accounts.entry(member).or_default().shares)
    }

    pub fn labels(&self) -> &[Label] {
        self.active_member()
            .and_then(|m| self.accounts.get(m))
            .map_or(&[], |a| a.labels.as_slice())
    }

    pub fn labels_mut(&mut self) -> Result<&mut Vec<Label>, Code> {
        let member = self
            .active_member()
            .ok_or(Code::Unauthorized)?
            .to_owned();
        Ok(&mut self.accounts.entry(member).or_default().labels)
    }

    pub fn forwards(&self) -> &[LocalMapping] {
        self.active_member()
            .and_then(|m| self.accounts.get(m))
            .map_or(&[], |a| a.forwards.as_slice())
    }

    pub fn forwards_mut(&mut self) -> Result<&mut Vec<LocalMapping>, Code> {
        let member = self
            .active_member()
            .ok_or(Code::Unauthorized)?
            .to_owned();
        Ok(&mut self.accounts.entry(member).or_default().forwards)
    }

    pub fn cloud_backup(&self) -> bool {
        self.active_member()
            .and_then(|m| self.accounts.get(m))
            .is_none_or(|a| a.cloud_backup)
    }

    pub fn set_cloud_backup(&mut self, enabled: bool) -> Result<(), Code> {
        let member = self
            .active_member()
            .ok_or(Code::Unauthorized)?
            .to_owned();
        self.accounts.entry(member).or_default().cloud_backup = enabled;
        Ok(())
    }

    pub fn dirty_prefs(&self) -> &[String] {
        self.active_member()
            .and_then(|m| self.accounts.get(m))
            .map_or(&[], |a| a.dirty_prefs.as_slice())
    }

    pub fn mark_pref_dirty(&mut self, service: &str) {
        let Some(member) = self.active_member().map(str::to_owned) else {
            return;
        };
        let entry = self.accounts.entry(member).or_default();
        if !entry.dirty_prefs.iter().any(|s| s == service) && entry.dirty_prefs.len() < 256 {
            entry.dirty_prefs.push(service.to_owned());
        }
    }

    pub fn clear_dirty_prefs(&mut self) {
        if let Some(member) = self.active_member().map(str::to_owned)
            && let Some(entry) = self.accounts.get_mut(&member)
        {
            entry.dirty_prefs.clear();
        }
    }

    pub fn clear_group(&mut self) {
        self.identities.clear();
        self.accounts.clear();
        self.shares.clear();
        self.labels.clear();
    }

    pub fn active_shares(&self) -> (&[String], &str) {
        self.active_member()
            .and_then(|m| self.accounts.get(m))
            .map_or((&[], ""), |a| {
                (a.active_shares.as_slice(), a.share_network.as_str())
            })
    }

    pub fn record_share_active(&mut self, id: &str, enabled: bool, marker: &str) {
        let Some(member) = self.active_member().map(str::to_owned) else {
            return;
        };
        let entry = self.accounts.entry(member).or_default();
        if enabled {
            if !entry.active_shares.iter().any(|s| s == id) && entry.active_shares.len() < 32 {
                entry.active_shares.push(id.to_owned());
            }
            if !marker.is_empty() {
                entry.share_network = marker.to_owned();
            }
        } else {
            entry.active_shares.retain(|s| s != id);
            if entry.active_shares.is_empty() {
                entry.share_network.clear();
            }
        }
    }

    pub fn clear_active_shares(&mut self) {
        if let Some(member) = self.active_member().map(str::to_owned)
            && let Some(entry) = self.accounts.get_mut(&member)
        {
            entry.active_shares.clear();
            entry.share_network.clear();
        }
    }

    pub fn save(&self, dir: &Path) -> Result<(), Code> {
        let bytes = serde_json::to_vec(self).map_err(|_| Code::Internal)?;
        save(&dir.join("device.json"), &bytes).map_err(|_| Code::Internal)
    }
}

const LOG_MAX: u64 = 10 * 1024 * 1024;
const LOG_AGE: u64 = 7 * 24 * 60 * 60;
const LOG_MSG_MAX: usize = 320;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub ts: u64,
    pub level: String,
    pub scope: String,
    pub message: String,
}

pub struct Log {
    path: PathBuf,
    lock: Mutex<()>,
    verbose: AtomicBool,
}

fn sanitize_log_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(LOG_MSG_MAX));
    let mut prev_space = false;
    for ch in raw.trim().chars() {
        let mapped = if ch.is_control() { ' ' } else { ch };
        if mapped.is_whitespace() {
            if !prev_space && !out.is_empty() {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(mapped);
            prev_space = false;
        }
        if out.chars().count() >= LOG_MSG_MAX {
            break;
        }
    }
    out.trim().to_owned()
}

fn code_level(code: Code) -> &'static str {
    match code {
        Code::Conflict
        | Code::DeviceRevoked
        | Code::Unauthorized
        | Code::ProtocolMismatch
        | Code::Internal => "ERR",
        _ => "WARN",
    }
}

impl Log {
    pub fn new(dir: &Path) -> Self {
        Self {
            path: dir.join("events.log"),
            lock: Mutex::new(()),
            verbose: AtomicBool::new(false),
        }
    }

    pub fn is_verbose(&self) -> bool {
        self.verbose.load(Ordering::Relaxed)
    }

    pub fn set_verbose(&self, enabled: bool) {
        self.verbose.store(enabled, Ordering::Relaxed);
    }

    pub fn record_info(&self, scope: &str, build: impl FnOnce() -> String) {
        if self.is_verbose() {
            self.record("INFO", scope, &build());
        }
    }

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    pub fn prune(&self) {
        use io::{BufRead, BufReader};
        let _lock = self.lock.lock().unwrap();
        let Ok(file) = private_file().read(true).open(&self.path).and_then(vetted) else {
            return;
        };
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        let cutoff = Self::now().saturating_sub(LOG_AGE - 60);
        let valid = |line: &str| {
            line.split_once(' ')
                .and_then(|(t, _)| t.parse::<u64>().ok())
                .is_some_and(|t| t > cutoff && t <= Self::now())
        };
        if reader
            .read_line(&mut line)
            .ok()
            .filter(|n| *n > 0)
            .is_some()
            && !valid(&line)
        {
            let recent = reader
                .lines()
                .map_while(Result::ok)
                .filter(|l| valid(l))
                .map(|l| format!("{l}\n"))
                .collect::<String>();
            let _ = save(&self.path, recent.as_bytes());
        }
    }

    fn append_line(&self, line: &str) {
        let _lock = self.lock.lock().unwrap();
        let rotate = fs::metadata(&self.path).is_ok_and(|m| m.len() + line.len() as u64 > LOG_MAX);
        let mut options = private_file();
        options.create(true);
        if rotate {
            options.write(true);
        } else {
            options.append(true);
        }
        if let Ok(mut f) = options.open(&self.path).and_then(vetted)
            && (!rotate || f.set_len(0).is_ok())
        {
            let _ = f.write_all(line.as_bytes());
        }
    }

    pub fn event(&self, code: Code) {
        let level = code_level(code);
        let line = format!("{} {level} core {code:?} ({code})\n", Self::now());
        self.append_line(&line);
    }

    pub fn record(&self, level: &str, scope: &str, message: &str) {
        let lvl = match level {
            "ERR" | "WARN" | "INFO" => level,
            _ => "INFO",
        };
        if lvl == "INFO" && !self.is_verbose() {
            return;
        }
        let clean = sanitize_log_text(message);
        if clean.is_empty() {
            return;
        }
        let sc = scope
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .take(16)
            .collect::<String>();
        let sc = if sc.is_empty() { "core" } else { sc.as_str() };
        let line = format!("{} {lvl} {sc} {clean}\n", Self::now());
        self.append_line(&line);
    }

    pub fn tail(&self, limit: usize) -> Vec<LogEntry> {
        use io::{BufRead, BufReader};
        let cap = limit.clamp(1, 500);
        let _lock = self.lock.lock().unwrap();
        let Ok(file) = private_file().read(true).open(&self.path).and_then(vetted) else {
            return Vec::new();
        };
        let mut ring = std::collections::VecDeque::with_capacity(cap);
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Some((ts_str, rest)) = trimmed.split_once(' ') else {
                continue;
            };
            let Ok(ts) = ts_str.parse::<u64>() else {
                continue;
            };
            let entry = if let Some((lvl, after_lvl)) = rest.split_once(' ')
                && matches!(lvl, "ERR" | "WARN" | "INFO")
                && let Some((scope, msg)) = after_lvl.split_once(' ')
            {
                LogEntry {
                    ts,
                    level: lvl.to_owned(),
                    scope: scope.to_owned(),
                    message: msg.to_owned(),
                }
            } else {
                LogEntry {
                    ts,
                    level: "WARN".to_owned(),
                    scope: "core".to_owned(),
                    message: rest.to_owned(),
                }
            };
            if ring.len() == cap {
                ring.pop_front();
            }
            ring.push_back(entry);
        }
        ring.into_iter().collect()
    }

    pub fn clear(&self) -> io::Result<()> {
        let _lock = self.lock.lock().unwrap();
        if self.path.exists() {
            save(&self.path, b"")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_drop_expired_entries_and_stay_within_quota() {
        let dir = tempfile::tempdir().unwrap();
        let log = Log::new(dir.path());
        let now = Log::now();
        let fresh = format!("{now} Offline\n");
        let expired = now.saturating_sub(LOG_AGE + 1);
        save(
            &log.path,
            format!("{expired} Unauthorized\n{fresh}").as_bytes(),
        )
        .unwrap();
        log.prune();
        assert_eq!(fs::read_to_string(&log.path).unwrap(), fresh);

        private_file()
            .write(true)
            .open(&log.path)
            .unwrap()
            .set_len(LOG_MAX - 1)
            .unwrap();
        log.event(Code::TargetTimeout);
        let contents = fs::read_to_string(&log.path).unwrap();
        assert_eq!(contents.lines().count(), 1);
        assert!(contents.contains("TargetTimeout"));
        assert!(contents.len() < LOG_MAX as usize);

        log.record("INFO", "tun", "suppressed while non-verbose");
        assert_eq!(log.tail(10).len(), 1);

        log.set_verbose(true);
        log.record("INFO", "tun", "ready addr=198.18.0.0\nline2");
        let entries = log.tail(10);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].level, "INFO");
        assert_eq!(entries[1].scope, "tun");
        assert_eq!(entries[1].message, "ready addr=198.18.0.0 line2");

        log.clear().unwrap();
        assert!(log.tail(10).is_empty());
    }
}
