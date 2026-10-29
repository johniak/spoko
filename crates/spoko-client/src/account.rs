//! This device's identity: a session on the server and the account keyring, unlocked with
//! the user's SSH key (one key shared by all of the user's machines).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use spoko_crypto::{Keyring, RecoveryKey, SshKey, read_private_file, write_private_file};
use spoko_proto::{AccountKeyring, LoginRequest};
use uuid::Uuid;

use crate::api::ApiClient;
use crate::{ClientError, Result};

pub const IDENTITY_FILE: &str = "identity.bin";

pub struct Identity {
    pub device_id: Uuid,
    pub device_name: String,
    pub server_url: String,
    pub token: String,
    pub key_path: PathBuf,
    /// `SHA256:…` of the SSH key.
    pub fingerprint: String,
    pub keyring: Keyring,
    /// The keyring as the server keeps it: wrapped with the SSH key, so it is no secret on disk.
    wrapped_keyring: Vec<u8>,
}

/// What `identity.bin` holds: nothing that opens data without the SSH key.
#[derive(Serialize, Deserialize)]
struct IdentityFile {
    device_id: Uuid,
    device_name: String,
    server_url: String,
    token: String,
    key_path: PathBuf,
    fingerprint: String,
    wrapped_keyring: Vec<u8>,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("device_id", &self.device_id)
            .field("device_name", &self.device_name)
            .field("server_url", &self.server_url)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

fn read_file(home: &Path) -> Result<IdentityFile> {
    let bytes = read_private_file(&home.join(IDENTITY_FILE))?;
    bincode::deserialize(&bytes).map_err(|e| ClientError::Other(format!("corrupt identity: {e}")))
}

impl Identity {
    /// Opens the keyring with the SSH key; a key with a passphrase needs `passphrase`
    /// (`CryptoError::Encrypted` without one).
    pub fn load(home: &Path, passphrase: Option<&str>) -> Result<Identity> {
        let f = read_file(home)?;
        let key = SshKey::load(&f.key_path, passphrase)?;
        if key.fingerprint() != f.fingerprint {
            return Err(ClientError::Other(format!(
                "{} is not the SSH key this device logged in with ({}); run spoko login again",
                f.key_path.display(),
                f.fingerprint
            )));
        }
        let keyring = key.unwrap_keyring(&f.wrapped_keyring)?;
        Ok(Identity {
            device_id: f.device_id,
            device_name: f.device_name,
            server_url: f.server_url,
            token: f.token,
            key_path: f.key_path,
            fingerprint: f.fingerprint,
            keyring,
            wrapped_keyring: f.wrapped_keyring,
        })
    }

    /// The SSH key file of a logged-in device, without opening it.
    pub fn key_path(home: &Path) -> Result<PathBuf> {
        Ok(read_file(home)?.key_path)
    }

    pub fn save(&self, home: &Path) -> Result<()> {
        let f = IdentityFile {
            device_id: self.device_id,
            device_name: self.device_name.clone(),
            server_url: self.server_url.clone(),
            token: self.token.clone(),
            key_path: self.key_path.clone(),
            fingerprint: self.fingerprint.clone(),
            wrapped_keyring: self.wrapped_keyring.clone(),
        };
        write_private_file(&home.join(IDENTITY_FILE), &bincode::serialize(&f).expect("identity serializes"))?;
        Ok(())
    }

    pub fn api(&self) -> ApiClient {
        ApiClient::new(&self.server_url, Some(self.token.clone()))
    }
}

/// Logs this device in with the SSH key at `key_path`. The account's first device creates the
/// keyring and returns a new recovery key to show the user. After the account's key was
/// replaced (`spoko-server account set-key`), `recovery` opens the old keyring.
pub fn login(
    home: &Path,
    server_url: &str,
    key_path: &Path,
    device_name: &str,
    recovery: Option<&RecoveryKey>,
    passphrase: Option<&str>,
) -> Result<(Identity, Option<RecoveryKey>)> {
    let key = SshKey::load(key_path, passphrase)?;
    let anon = ApiClient::new(server_url, None);
    let challenge = anon.challenge(&key.public_openssh())?;
    // the same device logging in again keeps its id
    let device_id = read_file(home).ok().filter(|f| f.server_url == server_url).map(|f| f.device_id);
    let resp = anon.login(&LoginRequest {
        challenge_id: challenge.challenge_id,
        signature: key.sign(&challenge.nonce)?,
        device_name: device_name.to_owned(),
        device_id,
    })?;
    let api = anon.with_token(&resp.token);
    let (keyring, new_rk) = account_keyring(&api, &key, resp.keyring, recovery)?;
    let id = Identity {
        device_id: resp.device_id,
        device_name: device_name.to_owned(),
        server_url: server_url.to_owned(),
        token: resp.token,
        key_path: std::path::absolute(key_path)?,
        fingerprint: key.fingerprint(),
        wrapped_keyring: key.wrap_keyring(&keyring),
        keyring,
    };
    id.save(home)?;
    Ok((id, new_rk))
}

fn account_keyring(api: &ApiClient, key: &SshKey, stored: Option<AccountKeyring>, recovery: Option<&RecoveryKey>) -> Result<(Keyring, Option<RecoveryKey>)> {
    let Some(stored) = stored else {
        let keyring = Keyring::generate();
        let rk = RecoveryKey::generate();
        let fresh = AccountKeyring { ssh_wrapped: key.wrap_keyring(&keyring), wrapped_for: String::new(), recovery_wrapped: Some(rk.wrap(&keyring.to_bytes())) };
        if api.store_keyring(&fresh)? {
            return Ok((keyring, Some(rk)));
        }
        // another machine created it at the same moment: use that one
        let theirs = api.keyring()?.ok_or_else(|| ClientError::Server("keyring missing".into()))?;
        return account_keyring(api, key, Some(theirs), recovery);
    };
    if stored.wrapped_for == key.fingerprint() {
        return Ok((key.unwrap_keyring(&stored.ssh_wrapped)?, None));
    }
    // the account's key was replaced: only the recovery key opens the keyring now
    let rk = recovery.ok_or_else(|| {
        ClientError::Other("the account's SSH key was replaced; log in with --recovery-key to unlock the existing data".into())
    })?;
    let wrapped = stored.recovery_wrapped.ok_or_else(|| ClientError::Other("this account has no recovery key".into()))?;
    let keyring = Keyring::from_bytes(&rk.unwrap(&wrapped)?)?;
    api.store_keyring(&AccountKeyring { ssh_wrapped: key.wrap_keyring(&keyring), wrapped_for: String::new(), recovery_wrapped: None })?;
    Ok((keyring, None))
}

/// Logs a device of this account out (its session token stops working). Not a security
/// boundary: every machine with the SSH key can log in again.
pub fn remove_device(id: &Identity, device: Uuid) -> Result<()> {
    id.api().revoke(device)
}
