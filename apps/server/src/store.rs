use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use fesen_proto::{
    Code, DeviceOs, DeviceProfile, MemberPref, alias_name, valid_id, valid_label,
    valid_machine_id, valid_password, valid_username,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{collections::HashSet, net::Ipv4Addr, path::Path, time::Duration};

pub fn secret() -> String {
    hex::encode(rand::random::<[u8; 32]>())
}

pub fn hash_secret(s: &str) -> [u8; 32] {
    Sha256::digest(s.as_bytes()).into()
}

pub fn password_hash(password: &str) -> Result<String, Code> {
    if !valid_password(password) {
        return Err(Code::Invalid);
    }
    let salt = SaltString::encode_b64(&rand::random::<[u8; 16]>()).map_err(|_| Code::Internal)?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| Code::Internal)
}

pub fn verify(password: &str, hash: &str) -> bool {
    valid_password(password)
        && PasswordHash::new(hash).is_ok_and(|hash| {
            Argon2::default()
                .verify_password(password.as_bytes(), &hash)
                .is_ok()
        })
}

pub fn os_str(os: DeviceOs) -> &'static str {
    match os {
        DeviceOs::Macos => "macos",
        DeviceOs::Windows => "windows",
        DeviceOs::Android => "android",
        DeviceOs::Linux => "linux",
    }
}

pub fn parse_os(s: &str) -> Option<DeviceOs> {
    match s {
        "macos" => Some(DeviceOs::Macos),
        "windows" => Some(DeviceOs::Windows),
        "android" => Some(DeviceOs::Android),
        "linux" => Some(DeviceOs::Linux),
        _ => None,
    }
}

pub fn db(path: &Path) -> anyhow::Result<Connection> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?;
    let db = Connection::open(path)?;
    db.busy_timeout(Duration::from_secs(3))?;
    db.execute_batch(
        "PRAGMA foreign_keys=ON;
         PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA temp_store=MEMORY;
         PRAGMA cache_size=-8000;
         CREATE TABLE IF NOT EXISTS users(
           name TEXT PRIMARY KEY, password TEXT NOT NULL,
           admin INTEGER NOT NULL, disabled INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS devices(
           id TEXT PRIMARY KEY, member TEXT NOT NULL REFERENCES users(name),
           label TEXT NOT NULL, cert BLOB NOT NULL UNIQUE,
           token BLOB, revoked INTEGER NOT NULL DEFAULT 0, seen INTEGER NOT NULL,
           machine_id TEXT, os TEXT
         );
         CREATE UNIQUE INDEX IF NOT EXISTS device_token ON devices(token);
         CREATE INDEX IF NOT EXISTS devices_member ON devices(member);
         CREATE TABLE IF NOT EXISTS admin_sessions(
           token BLOB PRIMARY KEY, member TEXT NOT NULL REFERENCES users(name), expires INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS admin_sessions_member ON admin_sessions(member);
         CREATE INDEX IF NOT EXISTS admin_sessions_expires ON admin_sessions(expires);
         CREATE TABLE IF NOT EXISTS member_prefs(
           member TEXT NOT NULL REFERENCES users(name),
           service TEXT NOT NULL,
           remark TEXT NOT NULL DEFAULT '',
           alias TEXT,
           ip TEXT,
           updated_at INTEGER NOT NULL,
           PRIMARY KEY(member, service)
         );
         CREATE TABLE IF NOT EXISTS device_profiles(
           member TEXT NOT NULL REFERENCES users(name),
           machine_id TEXT NOT NULL,
           name TEXT NOT NULL,
           os TEXT NOT NULL,
           shares TEXT NOT NULL DEFAULT '[]',
           forwards TEXT NOT NULL DEFAULT '[]',
           updated_at INTEGER NOT NULL,
           PRIMARY KEY(member, machine_id)
         );",
    )?;
    let cols: Vec<String> = db
        .prepare("PRAGMA table_info(devices)")?
        .query_map([], |r| r.get(1))?
        .collect::<Result<_, _>>()?;
    if !cols.iter().any(|c| c == "machine_id") {
        db.execute("ALTER TABLE devices ADD COLUMN machine_id TEXT", [])?;
    }
    if !cols.iter().any(|c| c == "os") {
        db.execute("ALTER TABLE devices ADD COLUMN os TEXT", [])?;
    }
    db.execute(
        "CREATE INDEX IF NOT EXISTS device_member_machine ON devices(member, machine_id)",
        [],
    )?;
    Ok(db)
}

pub fn create(db: &Connection, name: &str, hash: &str, admin: bool) -> Result<(), Code> {
    if !valid_username(name) {
        return Err(Code::Invalid);
    }
    match db.execute(
        "INSERT INTO users(name,password,admin) VALUES(?1,?2,?3) ON CONFLICT(name) DO NOTHING",
        params![name, hash, admin],
    ) {
        Ok(1) => Ok(()),
        Ok(_) => Err(Code::Conflict),
        Err(_) => Err(Code::Internal),
    }
}

pub fn login_hash(db: &Connection, name: &str, admin: bool) -> Option<String> {
    db.prepare_cached(
        "SELECT password FROM users WHERE name=?1 AND disabled=0 AND (admin=1 OR ?2=0)",
    )
    .ok()?
    .query_row(params![name, admin], |r| r.get(0))
    .ok()
}

pub fn is_admin(db: &Connection, name: &str) -> bool {
    db.prepare_cached("SELECT 1 FROM users WHERE name=?1 AND admin=1 AND disabled=0")
        .and_then(|mut stmt| stmt.query_row([name], |_| Ok(())))
        .is_ok()
}

pub fn authorize_device(
    db: &Connection,
    cert: &[u8],
    new_login: Option<(&str, &str)>,
    machine_id: Option<&str>,
    os: Option<DeviceOs>,
    token: &str,
) -> Result<(String, String, String, Option<DeviceOs>), Code> {
    if let Some(mid) = machine_id
        && !valid_machine_id(mid)
    {
        return Err(Code::Invalid);
    }
    let os_text = os.map(os_str);
    let read_row = |r: &rusqlite::Row<'_>| -> rusqlite::Result<(String, String, String, Option<DeviceOs>)> {
        let os_raw: Option<String> = r.get(3)?;
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            os_raw.as_deref().and_then(parse_os),
        ))
    };
    if let Some((member, label)) = new_login {
        let existing_cert: Option<(String, String, bool)> = db
            .query_row(
                "SELECT id,member,revoked FROM devices WHERE cert=?1",
                [cert],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(sql_code)?;
        if let Some((existing_id, existing_member, revoked)) = existing_cert {
            if revoked || existing_member != member {
                return Err(Code::Unauthorized);
            }
            return db
                .query_row(
                    "UPDATE devices SET token=?2,label=?3,machine_id=coalesce(?4,machine_id),os=coalesce(?5,os),seen=unixepoch()
                     WHERE id=?1 AND revoked=0
                     RETURNING id,member,label,os",
                    params![existing_id, hash_secret(token), label, machine_id, os_text],
                    read_row,
                )
                .map_err(sql_code);
        }
        if let Some(mid) = machine_id {
            let existing_machine: Option<String> = db
                .query_row(
                    "SELECT id FROM devices WHERE member=?1 AND machine_id=?2 AND revoked=0 ORDER BY seen DESC LIMIT 1",
                    params![member, mid],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql_code)?;
            if let Some(existing_id) = existing_machine {
                return db
                    .query_row(
                        "UPDATE devices SET cert=?2,token=?3,label=?4,os=coalesce(?5,os),seen=unixepoch()
                         WHERE id=?1 AND revoked=0
                         RETURNING id,member,label,os",
                        params![existing_id, cert, hash_secret(token), label, os_text],
                        read_row,
                    )
                    .map_err(sql_code);
            }
        }
        let id = hex::encode(rand::random::<[u8; 16]>());
        db.query_row(
            "INSERT INTO devices(id,member,label,cert,token,seen,machine_id,os)
             VALUES(?1,?2,?3,?4,?5,unixepoch(),?6,?7)
             RETURNING id,member,label,os",
            params![
                id,
                member,
                label,
                cert,
                hash_secret(token),
                machine_id,
                os_text
            ],
            read_row,
        )
        .map_err(sql_code)
    } else {
        db.query_row(
            "UPDATE devices SET seen=unixepoch(),machine_id=coalesce(machine_id,?3),os=coalesce(?4,os)
             WHERE token=?1 AND cert=?2 AND revoked=0
             AND EXISTS(SELECT 1 FROM users WHERE name=devices.member AND disabled=0)
             RETURNING id,member,label,os",
            params![hash_secret(token), cert, machine_id, os_text],
            read_row,
        )
        .map_err(sql_code)
    }
}

fn sql_code(e: rusqlite::Error) -> Code {
    if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
        Code::Unauthorized
    } else {
        Code::Internal
    }
}

pub fn admin_cookie(db: &Connection, member: &str, token: &str) -> Result<(), Code> {
    db.execute(
        "DELETE FROM admin_sessions WHERE expires <= unixepoch()",
        [],
    )
    .map_err(sql_code)?;
    db.execute(
        "INSERT INTO admin_sessions VALUES(?1,?2,unixepoch()+43200)",
        params![hash_secret(token), member],
    )
    .map_err(sql_code)?;
    Ok(())
}

pub fn admin(db: &Connection, token: &str) -> Option<String> {
    db.prepare_cached(
        "SELECT member FROM admin_sessions s JOIN users u ON u.name=s.member
         WHERE s.token=?1 AND s.expires>unixepoch() AND u.admin=1 AND u.disabled=0",
    )
    .ok()?
    .query_row([hash_secret(token)], |r| r.get(0))
    .ok()
}

#[derive(Serialize)]
pub struct User {
    name: String,
    admin: bool,
    disabled: bool,
}

#[derive(Serialize)]
pub struct Device {
    pub id: String,
    member: String,
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    machine_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    os: Option<DeviceOs>,
    revoked: bool,
    seen: i64,
    pub online: bool,
}

pub fn list(db: &Connection) -> Result<(Vec<User>, Vec<Device>), Code> {
    let users = db
        .prepare_cached("SELECT name,admin,disabled FROM users ORDER BY name")
        .map_err(sql_code)?
        .query_map([], |r| {
            Ok(User {
                name: r.get(0)?,
                admin: r.get(1)?,
                disabled: r.get(2)?,
            })
        })
        .map_err(sql_code)?
        .collect::<Result<_, _>>()
        .map_err(sql_code)?;
    let devices = db
        .prepare_cached("SELECT id,member,label,machine_id,os,revoked,seen FROM devices ORDER BY seen DESC")
        .map_err(sql_code)?
        .query_map([], |r| {
            let os_raw: Option<String> = r.get(4)?;
            Ok(Device {
                id: r.get(0)?,
                member: r.get(1)?,
                label: r.get(2)?,
                machine_id: r.get(3)?,
                os: os_raw.as_deref().and_then(parse_os),
                revoked: r.get(5)?,
                seen: r.get(6)?,
                online: false,
            })
        })
        .map_err(sql_code)?
        .collect::<Result<_, _>>()
        .map_err(sql_code)?;
    Ok((users, devices))
}

pub fn change_user(
    db: &mut Connection,
    member: &str,
    hash: Option<&str>,
    disabled: Option<bool>,
) -> Result<(), Code> {
    let tx = db.transaction().map_err(sql_code)?;
    let n = tx
        .execute(
            "UPDATE users SET password=coalesce(?2,password),disabled=coalesce(?3,disabled)
         WHERE name=?1 AND admin=0",
            params![member, hash, disabled],
        )
        .map_err(sql_code)?;
    if n != 1 {
        return Err(Code::Invalid);
    }
    tx.execute("UPDATE devices SET token=NULL WHERE member=?1", [member])
        .map_err(sql_code)?;
    tx.commit().map_err(sql_code)
}

pub fn set_admin(
    db: &mut Connection,
    actor: &str,
    member: &str,
    admin: bool,
) -> Result<(), Code> {
    if !admin && actor == member {
        return Err(Code::Invalid);
    }
    let tx = db.transaction().map_err(sql_code)?;
    let n = tx
        .execute(
            "UPDATE users SET admin=?2 WHERE name=?1 AND disabled=0",
            params![member, admin],
        )
        .map_err(sql_code)?;
    if n != 1 {
        return Err(Code::Invalid);
    }
    if !admin {
        let remaining: i64 = tx
            .query_row(
                "SELECT count(*) FROM users WHERE admin=1 AND disabled=0",
                [],
                |r| r.get(0),
            )
            .map_err(sql_code)?;
        if remaining < 1 {
            return Err(Code::Invalid);
        }
        tx.execute("DELETE FROM admin_sessions WHERE member=?1", [member])
            .map_err(sql_code)?;
    }
    tx.commit().map_err(sql_code)
}

pub fn revoke(db: &Connection, id: &str) -> Result<(), Code> {
    if db
        .execute("UPDATE devices SET revoked=1,token=NULL WHERE id=?1", [id])
        .map_err(sql_code)?
        != 1
    {
        return Err(Code::Invalid);
    }
    Ok(())
}

pub fn device_owner(db: &Connection, cert: &[u8]) -> Result<Option<(String, bool)>, Code> {
    db.query_row(
        "SELECT member,revoked FROM devices WHERE cert=?1",
        [cert],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .map_err(sql_code)
}

pub fn rename_device(db: &Connection, id: &str, label: &str) -> Result<String, Code> {
    let trimmed = label.trim();
    if !valid_label(trimmed) {
        return Err(Code::Invalid);
    }
    if db
        .execute(
            "UPDATE devices SET label=?2 WHERE id=?1 AND revoked=0",
            params![id, trimmed],
        )
        .map_err(sql_code)?
        != 1
    {
        return Err(Code::Unauthorized);
    }
    Ok(trimmed.to_owned())
}

pub fn list_prefs(db: &Connection, member: &str) -> Result<Vec<MemberPref>, Code> {
    db.prepare_cached(
        "SELECT service,remark,alias,ip FROM member_prefs WHERE member=?1 ORDER BY updated_at DESC",
    )
    .map_err(sql_code)?
    .query_map([member], |r| {
        let ip_raw: Option<String> = r.get(3)?;
        Ok(MemberPref {
            service: r.get(0)?,
            remark: r.get(1)?,
            alias: r.get(2)?,
            ip: ip_raw.and_then(|s| s.parse::<Ipv4Addr>().ok()),
        })
    })
    .map_err(sql_code)?
    .collect::<Result<_, _>>()
    .map_err(sql_code)
}

pub fn save_pref(db: &Connection, member: &str, pref: &MemberPref) -> Result<(), Code> {
    if !valid_id(&pref.service) {
        return Err(Code::Invalid);
    }
    let remark = pref.remark.trim();
    if !(remark.is_empty() || valid_label(remark)) {
        return Err(Code::Invalid);
    }
    let alias = pref
        .alias
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(alias_name)
        .transpose()?;
    if remark.is_empty() && alias.is_none() {
        db.execute(
            "DELETE FROM member_prefs WHERE member=?1 AND service=?2",
            params![member, &pref.service],
        )
        .map_err(sql_code)?;
        return Ok(());
    }
    if let Some(ref alias) = alias {
        let taken: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM member_prefs WHERE member=?1 AND service!=?2 AND alias=?3)",
                params![member, &pref.service, alias],
                |r| r.get(0),
            )
            .map_err(sql_code)?;
        if taken {
            return Err(Code::Conflict);
        }
    }
    let count: i64 = db
        .query_row(
            "SELECT count(*) FROM member_prefs WHERE member=?1 AND service!=?2",
            params![member, &pref.service],
            |r| r.get(0),
        )
        .map_err(sql_code)?;
    if count >= 256 {
        return Err(Code::Limit);
    }
    let ip_str = alias.as_ref().and(pref.ip).map(|ip| ip.to_string());
    db.execute(
        "INSERT INTO member_prefs(member,service,remark,alias,ip,updated_at)
         VALUES(?1,?2,?3,?4,?5,unixepoch())
         ON CONFLICT(member,service) DO UPDATE
         SET remark=excluded.remark,alias=excluded.alias,ip=excluded.ip,updated_at=excluded.updated_at",
        params![member, &pref.service, remark, alias, ip_str],
    )
    .map_err(sql_code)?;
    Ok(())
}

pub fn list_profiles(db: &Connection, member: &str) -> Result<Vec<DeviceProfile>, Code> {
    db.prepare_cached(
        "SELECT machine_id,name,os,shares,forwards,updated_at
         FROM device_profiles WHERE member=?1 ORDER BY updated_at DESC",
    )
    .map_err(sql_code)?
    .query_map([member], |r| {
        let os_raw: String = r.get(2)?;
        let shares_raw: String = r.get(3)?;
        let forwards_raw: String = r.get(4)?;
        Ok(DeviceProfile {
            machine_id: r.get(0)?,
            name: r.get(1)?,
            os: parse_os(&os_raw).unwrap_or(DeviceOs::Linux),
            shares: serde_json::from_str(&shares_raw).unwrap_or_default(),
            forwards: serde_json::from_str(&forwards_raw).unwrap_or_default(),
            updated_at: r.get(5)?,
        })
    })
    .map_err(sql_code)?
    .collect::<Result<_, _>>()
    .map_err(sql_code)
}

pub fn save_profile(
    db: &Connection,
    member: &str,
    profile: &DeviceProfile,
) -> Result<(), Code> {
    if !valid_machine_id(&profile.machine_id) || !valid_label(&profile.name) {
        return Err(Code::Invalid);
    }
    if profile.shares.len() > 32 || profile.forwards.len() > 64 {
        return Err(Code::Limit);
    }
    let mut shares = Vec::with_capacity(profile.shares.len());
    for s in &profile.shares {
        shares.push(s.clone().checked()?);
    }
    let mut seen_ports = HashSet::new();
    for f in &profile.forwards {
        if !valid_id(&f.service) || f.target_port == 0 || f.local_port == 0 {
            return Err(Code::Invalid);
        }
        if !seen_ports.insert(f.local_port) {
            return Err(Code::Conflict);
        }
    }
    let count: i64 = db
        .query_row(
            "SELECT count(*) FROM device_profiles WHERE member=?1 AND machine_id!=?2",
            params![member, &profile.machine_id],
            |r| r.get(0),
        )
        .map_err(sql_code)?;
    if count >= 32 {
        return Err(Code::Limit);
    }
    let shares_json = serde_json::to_string(&shares).map_err(|_| Code::Internal)?;
    let forwards_json = serde_json::to_string(&profile.forwards).map_err(|_| Code::Internal)?;
    if shares_json.len() + forwards_json.len() > 48 * 1024 {
        return Err(Code::Limit);
    }
    db.execute(
        "INSERT INTO device_profiles(member,machine_id,name,os,shares,forwards,updated_at)
         VALUES(?1,?2,?3,?4,?5,?6,unixepoch())
         ON CONFLICT(member,machine_id) DO UPDATE
         SET name=excluded.name,os=excluded.os,shares=excluded.shares,forwards=excluded.forwards,updated_at=excluded.updated_at",
        params![
            member,
            &profile.machine_id,
            profile.name.trim(),
            os_str(profile.os),
            shares_json,
            forwards_json,
        ],
    )
    .map_err(sql_code)?;
    Ok(())
}

pub fn delete_profile(db: &Connection, member: &str, machine_id: &str) -> Result<(), Code> {
    if !valid_machine_id(machine_id) {
        return Err(Code::Invalid);
    }
    db.execute(
        "DELETE FROM device_profiles WHERE member=?1 AND machine_id=?2",
        params![member, machine_id],
    )
    .map_err(sql_code)?;
    Ok(())
}
