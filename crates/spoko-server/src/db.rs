use std::path::Path;
use std::sync::Mutex;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::Rng;
use rusqlite::{Connection, OptionalExtension, params};
use spoko_proto::{DeviceCredentials, DeviceInfo, PairingInfo, PairingStarted, PairingStatus, Pointer, ProjectInfo};
use uuid::Uuid;

pub type DbResult<T> = Result<T, rusqlite::Error>;

const PAIRING_TTL_SECS: i64 = 600;
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

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
"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    pub id: Uuid,
    pub account_id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    Status(PairingStatus),
    /// Credentials were already handed out once.
    Gone,
    NotFound,
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

fn key32(v: Vec<u8>) -> [u8; 32] {
    v.try_into().expect("32-byte key column")
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

    pub fn create_account(&self, name: &str) -> DbResult<Uuid> {
        let id = Uuid::new_v4();
        self.conn().execute("INSERT INTO accounts (id, name) VALUES (?1, ?2)", params![id.to_string(), name])?;
        Ok(id)
    }

    pub fn account_by_name(&self, name: &str) -> DbResult<Option<Uuid>> {
        self.conn()
            .query_row("SELECT id FROM accounts WHERE name = ?1", [name], |r| r.get(0))
            .optional()
            .map(|o| o.map(uuid))
    }

    pub fn issue_bootstrap_token(&self, account: Uuid) -> DbResult<String> {
        let token = new_token();
        self.conn().execute(
            "INSERT INTO bootstrap_tokens (hash, account_id) VALUES (?1, ?2)",
            params![hash(&token), account.to_string()],
        )?;
        Ok(token)
    }

    pub fn peek_bootstrap_token(&self, token: &str) -> DbResult<Option<Uuid>> {
        self.conn()
            .query_row("SELECT account_id FROM bootstrap_tokens WHERE hash = ?1 AND used = 0", [hash(token)], |r| r.get(0))
            .optional()
            .map(|o| o.map(uuid))
    }

    pub fn consume_bootstrap_token(&self, token: &str) -> DbResult<Option<Uuid>> {
        let conn = self.conn();
        let acc: Option<String> = conn
            .query_row(
                "UPDATE bootstrap_tokens SET used = 1 WHERE hash = ?1 AND used = 0 RETURNING account_id",
                [hash(token)],
                |r| r.get(0),
            )
            .optional()?;
        Ok(acc.map(uuid))
    }

    pub fn set_recovery(&self, account: Uuid, blob: &[u8]) -> DbResult<()> {
        self.conn().execute("UPDATE accounts SET recovery_wrapped = ?1 WHERE id = ?2", params![blob, account.to_string()])?;
        Ok(())
    }

    pub fn get_recovery(&self, account: Uuid) -> DbResult<Option<Vec<u8>>> {
        Ok(self
            .conn()
            .query_row("SELECT recovery_wrapped FROM accounts WHERE id = ?1", [account.to_string()], |r| r.get(0))
            .optional()?
            .flatten())
    }

    pub fn create_device(&self, account: Uuid, name: &str, pubkey: &[u8; 32], wrapped: &[u8]) -> DbResult<DeviceCredentials> {
        Self::insert_device(&self.conn(), account, name, pubkey, wrapped)
    }

    fn insert_device(conn: &Connection, account: Uuid, name: &str, pubkey: &[u8; 32], wrapped: &[u8]) -> DbResult<DeviceCredentials> {
        let id = Uuid::new_v4();
        let token = new_token();
        conn.execute(
            "INSERT INTO devices (id, account_id, name, pubkey, token_hash, wrapped_keyring) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id.to_string(), account.to_string(), name, pubkey.as_slice(), hash(&token), wrapped],
        )?;
        Ok(DeviceCredentials { device_id: id, token })
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
        let mut st = conn.prepare("SELECT id, name, pubkey, revoked, last_seen FROM devices WHERE account_id = ?1 ORDER BY name")?;
        st.query_map([account.to_string()], |r| {
            Ok(DeviceInfo {
                id: uuid(r.get(0)?),
                name: r.get(1)?,
                pubkey: key32(r.get(2)?),
                revoked: r.get(3)?,
                last_seen: r.get(4)?,
            })
        })?
        .collect()
    }

    pub fn revoke_device(&self, account: Uuid, device: Uuid) -> DbResult<bool> {
        let n = self.conn().execute(
            "UPDATE devices SET revoked = 1 WHERE id = ?1 AND account_id = ?2",
            params![device.to_string(), account.to_string()],
        )?;
        Ok(n == 1)
    }

    pub fn device_keyring(&self, device: Uuid) -> DbResult<Option<Vec<u8>>> {
        self.conn()
            .query_row("SELECT wrapped_keyring FROM devices WHERE id = ?1", [device.to_string()], |r| r.get(0))
            .optional()
    }

    pub fn update_keyrings(&self, account: Uuid, per_device: &[(Uuid, Vec<u8>)], recovery: Option<&[u8]>) -> DbResult<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for (dev, wrapped) in per_device {
            tx.execute(
                "UPDATE devices SET wrapped_keyring = ?1 WHERE id = ?2 AND account_id = ?3",
                params![wrapped, dev.to_string(), account.to_string()],
            )?;
        }
        if let Some(r) = recovery {
            tx.execute("UPDATE accounts SET recovery_wrapped = ?1 WHERE id = ?2", params![r, account.to_string()])?;
        }
        tx.commit()
    }

    pub fn create_pairing(&self, name: &str, pubkey: &[u8; 32], now: i64) -> DbResult<PairingStarted> {
        let id = Uuid::new_v4();
        let secret = new_token();
        let mut rng = rand::thread_rng();
        let code: String = (0..8).map(|_| CODE_ALPHABET[rng.gen_range(0..CODE_ALPHABET.len())] as char).collect();
        self.conn().execute(
            "INSERT INTO pairings (id, secret_hash, code, pubkey, name, status, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6)",
            params![id.to_string(), hash(&secret), code, pubkey.as_slice(), name, now + PAIRING_TTL_SECS],
        )?;
        Ok(PairingStarted { pairing_id: id, secret, code })
    }

    pub fn pairing_by_code(&self, code: &str, now: i64) -> DbResult<Option<PairingInfo>> {
        self.conn()
            .query_row(
                "SELECT id, name, pubkey FROM pairings WHERE code = ?1 AND status = 'pending' AND expires_at > ?2",
                params![code.to_uppercase(), now],
                |r| Ok(PairingInfo { pairing_id: uuid(r.get(0)?), device_name: r.get(1)?, device_pubkey: key32(r.get(2)?) }),
            )
            .optional()
    }

    /// Creates the device for a pending pairing; `None` if it is not pending or expired.
    pub fn approve_pairing(&self, pairing: Uuid, account: Uuid, wrapped: &[u8], now: i64) -> DbResult<Option<DeviceCredentials>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let row: Option<(String, Vec<u8>)> = tx
            .query_row(
                "SELECT name, pubkey FROM pairings WHERE id = ?1 AND status = 'pending' AND expires_at > ?2",
                params![pairing.to_string(), now],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((name, pubkey)) = row else { return Ok(None) };
        let creds = Self::insert_device(&tx, account, &name, &key32(pubkey), wrapped)?;
        tx.execute(
            "UPDATE pairings SET status = 'approved', device_id = ?1, pending_token = ?2, wrapped_keyring = ?3 WHERE id = ?4",
            params![creds.device_id.to_string(), creds.token, wrapped, pairing.to_string()],
        )?;
        tx.commit()?;
        Ok(Some(creds))
    }

    pub fn poll_pairing(&self, pairing: Uuid, secret: &str, now: i64) -> DbResult<PollOutcome> {
        type Row = (String, i64, Option<String>, Option<String>, Option<Vec<u8>>);
        let conn = self.conn();
        let row: Option<Row> = conn
            .query_row(
                "SELECT status, expires_at, device_id, pending_token, wrapped_keyring FROM pairings WHERE id = ?1 AND secret_hash = ?2",
                params![pairing.to_string(), hash(secret)],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let Some((status, expires_at, device, token, wrapped)) = row else { return Ok(PollOutcome::NotFound) };
        match (status.as_str(), device, token, wrapped) {
            ("approved", Some(device), Some(token), Some(wrapped)) => {
                conn.execute("UPDATE pairings SET pending_token = NULL, status = 'delivered' WHERE id = ?1", [pairing.to_string()])?;
                Ok(PollOutcome::Status(PairingStatus::Approved {
                    credentials: DeviceCredentials { device_id: uuid(device), token },
                    wrapped_keyring: wrapped,
                }))
            }
            ("pending", ..) if expires_at > now => Ok(PollOutcome::Status(PairingStatus::Pending)),
            ("pending", ..) => Ok(PollOutcome::Status(PairingStatus::Expired)),
            _ => Ok(PollOutcome::Gone),
        }
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
