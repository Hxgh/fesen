use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use fesen_proto::{Code, valid_password, valid_username};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{path::Path, time::Duration};

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
         CREATE TABLE IF NOT EXISTS users(
           name TEXT PRIMARY KEY, password TEXT NOT NULL,
           admin INTEGER NOT NULL, disabled INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS devices(
           id TEXT PRIMARY KEY, member TEXT NOT NULL REFERENCES users(name),
           label TEXT NOT NULL, cert BLOB NOT NULL UNIQUE,
           token BLOB, revoked INTEGER NOT NULL DEFAULT 0, seen INTEGER NOT NULL
         );
         CREATE UNIQUE INDEX IF NOT EXISTS device_token ON devices(token);
         CREATE TABLE IF NOT EXISTS admin_sessions(
           token BLOB PRIMARY KEY, member TEXT NOT NULL REFERENCES users(name), expires INTEGER NOT NULL
         );",
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
    db.query_row(
        "SELECT password FROM users WHERE name=?1 AND disabled=0 AND (admin=1 OR ?2=0)",
        params![name, admin],
        |r| r.get(0),
    )
    .ok()
}

pub fn is_admin(db: &Connection, name: &str) -> bool {
    db.query_row(
        "SELECT 1 FROM users WHERE name=?1 AND admin=1 AND disabled=0",
        [name],
        |_| Ok(()),
    )
    .is_ok()
}

pub fn authorize_device(
    db: &Connection,
    cert: &[u8],
    new_login: Option<(&str, &str)>,
    token: &str,
) -> Result<(String, String, String), Code> {
    if let Some((member, label)) = new_login {
        let id = hex::encode(rand::random::<[u8; 16]>());
        db.query_row(
            "INSERT INTO devices(id,member,label,cert,token,seen)
             VALUES(?1,?2,?3,?4,?5,unixepoch()) ON CONFLICT(cert) DO UPDATE
             SET token=excluded.token,label=excluded.label,seen=unixepoch()
             WHERE devices.member=excluded.member AND devices.revoked=0
             RETURNING id,member,label",
            params![id, member, label, cert, hash_secret(token)],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(sql_code)
    } else {
        db.query_row(
            "UPDATE devices SET seen=unixepoch()
             WHERE token=?1 AND cert=?2 AND revoked=0
             AND EXISTS(SELECT 1 FROM users WHERE name=devices.member AND disabled=0)
             RETURNING id,member,label",
            params![hash_secret(token), cert],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
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
    db.query_row(
        "SELECT member FROM admin_sessions s JOIN users u ON u.name=s.member
         WHERE s.token=?1 AND s.expires>unixepoch() AND u.admin=1 AND u.disabled=0",
        [hash_secret(token)],
        |r| r.get(0),
    )
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
    revoked: bool,
    seen: i64,
    pub online: bool,
}

pub fn list(db: &Connection) -> Result<(Vec<User>, Vec<Device>), Code> {
    let users = db
        .prepare("SELECT name,admin,disabled FROM users ORDER BY name")
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
        .prepare("SELECT id,member,label,revoked,seen FROM devices ORDER BY seen DESC")
        .map_err(sql_code)?
        .query_map([], |r| {
            Ok(Device {
                id: r.get(0)?,
                member: r.get(1)?,
                label: r.get(2)?,
                revoked: r.get(3)?,
                seen: r.get(4)?,
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
