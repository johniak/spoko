//! This device's identity, and the account-level key operations.

use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use spoko_crypto::{DeviceKeypair, Keyring, RecoveryKey, fingerprint, read_private_file, seal_to, write_private_file};
use spoko_proto::{BootstrapRequest, DeviceKeyring, KeyringUpdate, PairingStart, PairingStarted, PairingStatus};
use uuid::Uuid;

use crate::api::{ApiClient, PollResult};
use crate::{ClientError, Result};

pub const IDENTITY_FILE: &str = "identity.bin";

pub struct Identity {
    pub device_id: Uuid,
    pub device_name: String,
    pub server_url: String,
    pub token: String,
    pub keypair: DeviceKeypair,
    pub keyring: Keyring,
}

#[derive(Serialize, Deserialize)]
struct IdentityFile {
    device_id: Uuid,
    device_name: String,
    server_url: String,
    token: String,
    secret: [u8; 32],
    keyring: Vec<u8>,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity").field("device_id", &self.device_id).field("device_name", &self.device_name).field("server_url", &self.server_url).finish_non_exhaustive()
    }
}

impl Identity {
    pub fn load(home: &Path) -> Result<Identity> {
        let bytes = read_private_file(&home.join(IDENTITY_FILE))?;
        let f: IdentityFile = bincode::deserialize(&bytes).map_err(|e| ClientError::Other(format!("corrupt identity: {e}")))?;
        Ok(Identity {
            device_id: f.device_id,
            device_name: f.device_name,
            server_url: f.server_url,
            token: f.token,
            keypair: DeviceKeypair::from_secret_bytes(f.secret),
            keyring: Keyring::from_bytes(&f.keyring)?,
        })
    }

    pub fn save(&self, home: &Path) -> Result<()> {
        let f = IdentityFile {
            device_id: self.device_id,
            device_name: self.device_name.clone(),
            server_url: self.server_url.clone(),
            token: self.token.clone(),
            secret: self.keypair.secret_bytes(),
            keyring: self.keyring.to_bytes(),
        };
        write_private_file(&home.join(IDENTITY_FILE), &bincode::serialize(&f).expect("identity serializes"))?;
        Ok(())
    }

    pub fn api(&self) -> ApiClient {
        ApiClient::new(&self.server_url, Some(self.token.clone()))
    }
}

/// First device of an account (no `recovery`), or recovery of the keyring with the recovery key.
/// Returns the new recovery key to show the user when one was created.
pub fn login_bootstrap(home: &Path, server_url: &str, bootstrap_token: &str, device_name: &str, recovery: Option<&RecoveryKey>) -> Result<(Identity, Option<RecoveryKey>)> {
    let api = ApiClient::new(server_url, None);
    let keypair = DeviceKeypair::generate();
    let (keyring, new_rk) = match recovery {
        Some(rk) => (Keyring::from_bytes(&rk.unwrap(&api.recovery(bootstrap_token)?.recovery_wrapped_keyring)?)?, None),
        None => (Keyring::generate(), Some(RecoveryKey::generate())),
    };
    let req = BootstrapRequest {
        device_name: device_name.to_owned(),
        device_pubkey: keypair.public(),
        wrapped_keyring: seal_to(&keypair.public(), &keyring.to_bytes()),
        recovery_wrapped_keyring: new_rk.as_ref().map(|rk| rk.wrap(&keyring.to_bytes())),
    };
    let creds = api.bootstrap(bootstrap_token, &req)?;
    let id = Identity { device_id: creds.device_id, device_name: device_name.to_owned(), server_url: server_url.to_owned(), token: creds.token, keypair, keyring };
    id.save(home)?;
    Ok((id, new_rk))
}

/// A new device waiting for approval from an existing one.
pub struct PendingPairing {
    pub code: String,
    pub fingerprint: String,
    started: PairingStarted,
    keypair: DeviceKeypair,
    device_name: String,
    server_url: String,
}

pub fn pair_start(server_url: &str, device_name: &str) -> Result<PendingPairing> {
    let keypair = DeviceKeypair::generate();
    let started = ApiClient::new(server_url, None).pairing_start(&PairingStart { device_name: device_name.to_owned(), device_pubkey: keypair.public() })?;
    Ok(PendingPairing {
        code: started.code.clone(),
        fingerprint: fingerprint(&keypair.public()),
        started,
        keypair,
        device_name: device_name.to_owned(),
        server_url: server_url.to_owned(),
    })
}

impl PendingPairing {
    pub fn wait(self, home: &Path, timeout: Duration, poll_every: Duration) -> Result<Identity> {
        let api = ApiClient::new(&self.server_url, None);
        let deadline = Instant::now() + timeout;
        loop {
            match api.pairing_poll(self.started.pairing_id, &self.started.secret)? {
                PollResult::Status(PairingStatus::Approved { credentials, wrapped_keyring }) => {
                    let keyring = Keyring::from_bytes(&self.keypair.unseal(&wrapped_keyring)?)?;
                    let id = Identity {
                        device_id: credentials.device_id,
                        device_name: self.device_name,
                        server_url: self.server_url,
                        token: credentials.token,
                        keypair: self.keypair,
                        keyring,
                    };
                    id.save(home)?;
                    return Ok(id);
                }
                PollResult::Status(PairingStatus::Expired) | PollResult::Gone => return Err(ClientError::Other("pairing expired".into())),
                PollResult::Status(PairingStatus::Pending) => {}
            }
            if Instant::now() >= deadline {
                return Err(ClientError::Other("pairing timed out".into()));
            }
            std::thread::sleep(poll_every);
        }
    }
}

/// Approves a pending pairing after `confirm(device_name, fingerprint)` returns true.
pub fn pair_approve(id: &Identity, code: &str, confirm: impl Fn(&str, &str) -> bool) -> Result<bool> {
    let api = id.api();
    let info = api.pairing_by_code(code)?.ok_or_else(|| ClientError::Other(format!("no pending pairing with code {code}")))?;
    if !confirm(&info.device_name, &fingerprint(&info.device_pubkey)) {
        return Ok(false);
    }
    api.pairing_approve(info.pairing_id, seal_to(&info.device_pubkey, &id.keyring.to_bytes()))?;
    Ok(true)
}

/// Revokes a device, rotates the data key and re-wraps it for every remaining device.
/// Returns the new recovery key (the old one no longer unlocks new data).
pub fn revoke_device(id: &mut Identity, home: &Path, device: Uuid) -> Result<RecoveryKey> {
    let api = id.api();
    api.revoke(device)?;
    id.keyring.rotate();
    let bytes = id.keyring.to_bytes();
    let per_device = api
        .devices()?
        .into_iter()
        .filter(|d| !d.revoked)
        .map(|d| DeviceKeyring { device_id: d.id, wrapped_keyring: seal_to(&d.pubkey, &bytes) })
        .collect();
    let rk = RecoveryKey::generate();
    api.update_keyrings(&KeyringUpdate { per_device, recovery_wrapped_keyring: Some(rk.wrap(&bytes)) })?;
    id.save(home)?;
    Ok(rk)
}

/// Fetches this device's (possibly rotated) keyring.
pub fn refresh_keyring(id: &mut Identity, home: &Path) -> Result<()> {
    let wrapped = id.api().my_keyring()?;
    id.keyring = Keyring::from_bytes(&id.keypair.unseal(&wrapped.wrapped_keyring)?)?;
    id.save(home)
}
