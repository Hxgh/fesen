use crate::routing::Label;
use fesen_proto::{CERT_NAME, Code, Publication};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Default, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct AccountConfig {
    pub shares: Vec<Publication>,
    pub labels: Vec<Label>,
}

#[derive(Default, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Local {
    pub server: String,
    pub identities: BTreeMap<String, Identity>,
    pub session: Option<Login>,
    pub accounts: BTreeMap<String, AccountConfig>,
    // Legacy flat fields retained solely for automatic one-time migration from older device.json.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shares: Vec<Publication>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<Label>,
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

    pub fn clear_group(&mut self) {
        self.identities.clear();
        self.accounts.clear();
        self.shares.clear();
        self.labels.clear();
    }

    pub fn save(&self, dir: &Path) -> Result<(), Code> {
        let bytes = serde_json::to_vec(self).map_err(|_| Code::Internal)?;
        save(&dir.join("device.json"), &bytes).map_err(|_| Code::Internal)
    }
}

const LOG_MAX: u64 = 10 * 1024 * 1024;
const LOG_AGE: u64 = 7 * 24 * 60 * 60;

pub struct Log {
    path: PathBuf,
    lock: Mutex<()>,
}

impl Log {
    pub fn new(dir: &Path) -> Self {
        Self {
            path: dir.join("events.log"),
            lock: Mutex::new(()),
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

    pub fn event(&self, code: Code) {
        let _lock = self.lock.lock().unwrap();
        let line = format!("{} {code:?}\n", Self::now());
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
        assert!(contents.ends_with(" TargetTimeout\n"));
        assert!(contents.len() < LOG_MAX as usize);
    }
}
