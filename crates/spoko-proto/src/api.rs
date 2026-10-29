use serde::{Deserialize, Serialize};
use spoko_crypto::ObjectId;
use uuid::Uuid;

use crate::serde_bytes;

/// Login, step 1: the public key (`ssh-ed25519 AAAA…`) the device will sign with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChallengeRequest {
    pub public_key: String,
}

/// A single-use nonce, valid for a short time and only for the key it was issued to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Challenge {
    pub challenge_id: Uuid,
    #[serde(with = "serde_bytes")]
    pub nonce: Vec<u8>,
}

/// Login, step 2: the nonce signed with the SSH key (armored sshsig, namespace `spoko`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginRequest {
    pub challenge_id: Uuid,
    pub signature: String,
    pub device_name: String,
    /// Logging in again from a known device keeps its id.
    #[serde(default)]
    pub device_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginResponse {
    pub account_id: Uuid,
    pub device_id: Uuid,
    pub token: String,
    /// `None` until the first device stores one.
    pub keyring: Option<AccountKeyring>,
}

/// The account keyring, wrapped with the SSH key and with the recovery key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountKeyring {
    #[serde(with = "serde_bytes")]
    pub ssh_wrapped: Vec<u8>,
    /// Fingerprint of the SSH key `ssh_wrapped` was made with (server-filled on reads).
    #[serde(default)]
    pub wrapped_for: String,
    #[serde(with = "serde_bytes::opt", default)]
    pub recovery_wrapped: Option<Vec<u8>>,
}

/// Moves the account to a new SSH key: `signature` answers a challenge issued for the new key
/// (proof of holding it); the keyring comes re-wrapped for it, with a new recovery copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RotateKey {
    pub challenge_id: Uuid,
    pub signature: String,
    #[serde(with = "serde_bytes")]
    pub ssh_wrapped: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub recovery_wrapped: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: Uuid,
    pub name: String,
    pub revoked: bool,
    pub last_seen: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectCreate {
    #[serde(with = "serde_bytes")]
    pub encrypted_meta: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub id: Uuid,
    #[serde(with = "serde_bytes")]
    pub encrypted_meta: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingRequest {
    pub ids: Vec<ObjectId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingResponse {
    pub ids: Vec<ObjectId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pointer {
    pub version: u64,
    #[serde(with = "serde_bytes")]
    pub value: Vec<u8>,
}

/// `expected_version`: `None` = unconditional write, `Some(0)` = must not exist yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointerPut {
    pub expected_version: Option<u64>,
    #[serde(with = "serde_bytes")]
    pub value: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessagePost {
    #[serde(with = "serde_bytes")]
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presence {
    pub online_devices: Vec<Uuid>,
}
