use std::path::Path;
use std::sync::Mutex;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::Rng;
use rusqlite::{Connection, OptionalExtension, params};
use spoko_proto::{AccountKeyring, Challenge, DeviceInfo, Pointer, ProjectInfo};
use uuid::Uuid;

pub type DbResult<T> = Result<T, rusqlite::Error>;

const CHALLENGE_TTL_SECS: i64 = 120;

const MIGRATIONS: &[&str] = &[r"
CREATE TABLE accounts (id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, recovery_wrapped BLOB);
CREATE TABLE bootstrap_tokens (hash BLOB PRIMARY KEY, account_id TEXT NOT NULL, used INTEGER NOT NULL DEFAULT 0);
CREATE TABLE devices (id TEXT PRIMARY KEY, account_id TEXT NOT NULL, name TEXT NOT NULL, pubkey BLOB NOT NULL,
  token_hash BLOB NOT NULL UNIQUE, wrapped_keyring BLOB NOT NULL, revoked INTEGER NOT NULL DEFAULT 0, last_seen INTEGER);
CREATE TABLE pairings (id TEXT PRIMARY KEY, secret_hash BLOB NOT NULL, code TEXT NOT NULL UNIQUE, pubkey BLOB NOT NULL,
  name TEXT NOT NULL, status TEXT NOT NULL, device_id TEXT, pending_token TEXT, wrapped_keyring BLOB, expires_at INTEGER NOT NULL);
CREATE TABLE projects (id TEXT PRIMARY KEY, account_id TEXT NOT NULL, encrypted_meta BLOB NOT NULL);
CREATE TABLE pointers (project_id TEXT NOT NULL, name TEXT NOT NULL, version INTEGER NOT NULL, value BLOB NOT NULL,
  PRIMARY KEY (project_id, name));
", r"
DROP TABLE pairings;
DROP TABLE bootstrap_tokens;
DROP TABLE devices;
CREATE TABLE devices (id TEXT PRIMARY KEY, account_id TEXT NOT NULL, name TEXT NOT NULL,
  token_hash BLOB NOT NULL UNIQUE, revoked INTEGER NOT NULL DEFAULT 0, last_seen INTEGER);
ALTER TABLE accounts ADD COLUMN ssh_key TEXT;
ALTER TABLE accounts ADD COLUMN ssh_fingerprint TEXT;
ALTER TABLE accounts ADD COLUMN ssh_wrapped BLOB;
ALTER TABLE accounts ADD COLUMN wrapped_for TEXT;
CREATE UNIQUE INDEX accounts_ssh_key ON accounts (ssh_key);
CREATE TABLE challenges (id TEXT PRIMARY KEY, ssh_key TEXT NOT NULL, nonce BLOB NOT NULL, expires_at INTEGER NOT NULL);
"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    pub id: Uuid,
    pub account_id: Uuid,
    pub name: String,
}

/// A device token, shown once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceLogin {
    pub device_id: Uuid,
    pub token: String,
}

pub struct Db {
    conn: Mutex<Connection>,
}

fn hash(token: &str) -> Vec<u8> {
    blake3::hash(token.as_bytes()).as_bytes().to_vec()
}

fn new_token() -> String {
    URL_SAFE_NO_PAD.encode(rand::thread_rng().r#gen::<[u8; 32]>())
}

fn uuid(s: String) -> Uuid {
    Uuid::parse_str(&s).expect("uuid column")
}


impl Db {
    pub fn open(path: &Path) -> DbResult<Db> {
        Db::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> DbResult<Db> {
        Db::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> DbResult<Db> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        for (i, m) in MIGRATIONS.iter().enumerate().skip(usize::try_from(version).unwrap_or(0)) {
            conn.execute_batch(m)?;
            conn.pragma_update(None, "user_version", i64::try_from(i + 1).expect("few migrations"))?;
        }
        Ok(Db { conn: Mutex::new(conn) })
    }

    /// Test hook: run raw SQL (used to break the schema and exercise error paths).
    #[cfg(feature = "test-util")]
    pub fn execute_raw(&self, sql: &str) -> DbResult<()> {
        self.conn().execute_batch(sql)
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("db mutex poisoned")
    }

    /// `ssh_key` is the comment-free public key line (see `spoko_crypto::parse_public_key`).
    pub fn create_account(&self, name: &str, ssh_key: &str, fingerprint: &str) -> DbResult<Uuid> {
        let id = Uuid::new_v4();
        self.conn().execute(
            "INSERT INTO accounts (id, name, ssh_key, ssh_fingerprint) VALUES (?1, ?2, ?3, ?4)",
            params![id.to_string(), name, ssh_key, fingerprint],
        )?;
        Ok(id)
    }

    pub fn account_by_name(&self, name: &str) -> DbResult<Option<Uuid>> {
        self.conn()
            .query_row("SELECT id FROM accounts WHERE name = ?1", [name], |r| r.get(0))
            .optional()
            .map(|o| o.map(uuid))
    }

    pub fn account_by_key(&self, ssh_key: &str) -> DbResult<Option<Uuid>> {
        self.conn()
            .query_row("SELECT id FROM accounts WHERE ssh_key = ?1", [ssh_key], |r| r.get(0))
            .optional()
            .map(|o| o.map(uuid))
    }

    /// Replaces the account's SSH key (lost or leaked key) and logs every device out.
    pub fn set_account_key(&self, account: Uuid, ssh_key: &str, fingerprint: &str) -> DbResult<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE accounts SET ssh_key = ?1, ssh_fingerprint = ?2 WHERE id = ?3",
            params![ssh_key, fingerprint, account.to_string()],
        )?;
        tx.execute("UPDATE devices SET revoked = 1 WHERE account_id = ?1", [account.to_string()])?;
        tx.commit()
    }

    pub fn create_challenge(&self, ssh_key: &str, now: i64) -> DbResult<Challenge> {
        let id = Uuid::new_v4();
        let nonce = rand::thread_rng().r#gen::<[u8; 32]>().to_vec();
        let conn = self.conn();
        conn.execute("DELETE FROM challenges WHERE expires_at <= ?1", [now])?;
        conn.execute(
            "INSERT INTO challenges (id, ssh_key, nonce, expires_at) VALUES (?1, ?2, ?3, ?4)",
            params![id.to_string(), ssh_key, nonce, now + CHALLENGE_TTL_SECS],
        )?;
        Ok(Challenge { challenge_id: id, nonce })
    }

    /// Uses up a challenge: `(public key, nonce)` once, `None` afterwards or after it expired.
    pub fn take_challenge(&self, id: Uuid, now: i64) -> DbResult<Option<(String, Vec<u8>)>> {
        self.conn()
            .query_row(
                "DELETE FROM challenges WHERE id = ?1 RETURNING ssh_key, nonce, expires_at",
                [id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)?)),
            )
            .optional()
            .map(|o| o.filter(|(_, _, exp)| *exp > now).map(|(k, n, _)| (k, n)))
    }

    /// A new session token; for `device` of this account the same device id is kept.
    pub fn login_device(&self, account: Uuid, name: &str, device: Option<Uuid>) -> DbResult<DeviceLogin> {
        let token = new_token();
        let conn = self.conn();
        if let Some(id) = device {
            let n = conn.execute(
                "UPDATE devices SET token_hash = ?1, name = ?2, revoked = 0 WHERE id = ?3 AND account_id = ?4",
                params![hash(&token), name, id.to_string(), account.to_string()],
            )?;
            if n == 1 {
                return Ok(DeviceLogin { device_id: id, token });
            }
        }
        let id = Uuid::new_v4();
        conn.execute(
            "INSERT INTO devices (id, account_id, name, token_hash) VALUES (?1, ?2, ?3, ?4)",
            params![id.to_string(), account.to_string(), name, hash(&token)],
        )?;
        Ok(DeviceLogin { device_id: id, token })
    }

    /// `None` for a new account (no keyring and no recovery copy yet).
    pub fn account_keyring(&self, account: Uuid) -> DbResult<Option<AccountKeyring>> {
        type Row = (Option<Vec<u8>>, Option<String>, Option<Vec<u8>>);
        let row: Row = self.conn().query_row(
            "SELECT ssh_wrapped, wrapped_for, recovery_wrapped FROM accounts WHERE id = ?1",
            [account.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok(match row {
            (None, _, None) => None,
            (ssh, wrapped_for, recovery_wrapped) => Some(AccountKeyring {
                ssh_wrapped: ssh.unwrap_or_default(),
                wrapped_for: wrapped_for.unwrap_or_default(),
                recovery_wrapped,
            }),
        })
    }

    /// Stores the keyring wrapped with the account's current SSH key — only while the stored
    /// copy is not already for that key (first device, or after the key was replaced).
    pub fn store_keyring(&self, account: Uuid, ssh_wrapped: &[u8], recovery: Option<&[u8]>) -> DbResult<bool> {
        let n = self.conn().execute(
            "UPDATE accounts SET ssh_wrapped = ?1, wrapped_for = ssh_fingerprint, recovery_wrapped = COALESCE(?2, recovery_wrapped)
             WHERE id = ?3 AND (wrapped_for IS NULL OR wrapped_for != ssh_fingerprint)",
            params![ssh_wrapped, recovery, account.to_string()],
        )?;
        Ok(n == 1)
    }

    pub fn device_by_token(&self, token: &str) -> DbResult<Option<DeviceRow>> {
        self.conn()
            .query_row(
                "SELECT id, account_id, name FROM devices WHERE token_hash = ?1 AND revoked = 0",
                [hash(token)],
                |r| Ok(DeviceRow { id: uuid(r.get(0)?), account_id: uuid(r.get(1)?), name: r.get(2)? }),
            )
            .optional()
    }

    pub fn touch_device(&self, device: Uuid, now: i64) -> DbResult<()> {
        self.conn().execute("UPDATE devices SET last_seen = ?1 WHERE id = ?2", params![now, device.to_string()])?;
        Ok(())
    }

    pub fn list_devices(&self, account: Uuid) -> DbResult<Vec<DeviceInfo>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT id, name, revoked, last_seen FROM devices WHERE account_id = ?1 ORDER BY name")?;
        st.query_map([account.to_string()], |r| Ok(DeviceInfo { id: uuid(r.get(0)?), name: r.get(1)?, revoked: r.get(2)?, last_seen: r.get(3)? }))?
            .collect()
    }

    pub fn revoke_device(&self, account: Uuid, device: Uuid) -> DbResult<bool> {
        let n = self.conn().execute(
            "UPDATE devices SET revoked = 1 WHERE id = ?1 AND account_id = ?2",
            params![device.to_string(), account.to_string()],
        )?;
        Ok(n == 1)
    }

    pub fn create_project(&self, account: Uuid, meta: &[u8]) -> DbResult<Uuid> {
        let id = Uuid::new_v4();
        self.conn().execute(
            "INSERT INTO projects (id, account_id, encrypted_meta) VALUES (?1, ?2, ?3)",
            params![id.to_string(), account.to_string(), meta],
        )?;
        Ok(id)
    }

    pub fn list_projects(&self, account: Uuid) -> DbResult<Vec<ProjectInfo>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT id, encrypted_meta FROM projects WHERE account_id = ?1")?;
        st.query_map([account.to_string()], |r| Ok(ProjectInfo { id: uuid(r.get(0)?), encrypted_meta: r.get(1)? }))?
            .collect()
    }

    pub fn project_account(&self, project: Uuid) -> DbResult<Option<Uuid>> {
        self.conn()
            .query_row("SELECT account_id FROM projects WHERE id = ?1", [project.to_string()], |r| r.get(0))
            .optional()
            .map(|o| o.map(uuid))
    }

    pub fn get_pointer(&self, project: Uuid, name: &str) -> DbResult<Option<Pointer>> {
        Self::read_pointer(&self.conn(), project, name)
    }

    fn read_pointer(conn: &Connection, project: Uuid, name: &str) -> DbResult<Option<Pointer>> {
        conn.query_row(
            "SELECT version, value FROM pointers WHERE project_id = ?1 AND name = ?2",
            params![project.to_string(), name],
            |r| Ok(Pointer { version: r.get::<_, i64>(0)?.cast_unsigned(), value: r.get(1)? }),
        )
        .optional()
    }

    /// Compare-and-swap. `Err(current)` on mismatch (version 0 when the pointer is absent).
    pub fn cas_pointer(&self, project: Uuid, name: &str, expected: Option<u64>, value: &[u8]) -> DbResult<Result<Pointer, Pointer>> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current = Self::read_pointer(&tx, project, name)?;
        let current_version = current.as_ref().map_or(0, |p| p.version);
        if expected.is_some_and(|e| e != current_version) {
            return Ok(Err(current.unwrap_or(Pointer { version: 0, value: vec![] })));
        }
        let next = Pointer { version: current_version + 1, value: value.to_vec() };
        tx.execute(
            "INSERT INTO pointers (project_id, name, version, value) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (project_id, name) DO UPDATE SET version = excluded.version, value = excluded.value",
            params![project.to_string(), name, next.version.cast_signed(), value],
        )?;
        tx.commit()?;
        Ok(Ok(next))
    }
}
