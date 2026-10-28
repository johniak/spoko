use serde::{Deserialize, Serialize};
use spoko_crypto::ObjectId;
use uuid::Uuid;

use crate::serde_bytes;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapRequest {
    pub device_name: String,
    #[serde(with = "serde_bytes::key32")]
    pub device_pubkey: [u8; 32],
    #[serde(with = "serde_bytes")]
    pub wrapped_keyring: Vec<u8>,
    #[serde(with = "serde_bytes::opt", default)]
    pub recovery_wrapped_keyring: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCredentials {
    pub device_id: Uuid,
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryBlob {
    #[serde(with = "serde_bytes")]
    pub recovery_wrapped_keyring: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingStart {
    pub device_name: String,
    #[serde(with = "serde_bytes::key32")]
    pub device_pubkey: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingStarted {
    pub pairing_id: Uuid,
    pub secret: String,
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PairingStatus {
    Pending,
    Approved {
        credentials: DeviceCredentials,
        #[serde(with = "serde_bytes")]
        wrapped_keyring: Vec<u8>,
    },
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingInfo {
    pub pairing_id: Uuid,
    pub device_name: String,
    #[serde(with = "serde_bytes::key32")]
    pub device_pubkey: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingApprove {
    #[serde(with = "serde_bytes")]
    pub wrapped_keyring: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: Uuid,
    pub name: String,
    #[serde(with = "serde_bytes::key32")]
    pub pubkey: [u8; 32],
    pub revoked: bool,
    pub last_seen: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrappedKeyring {
    #[serde(with = "serde_bytes")]
    pub wrapped_keyring: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceKeyring {
    pub device_id: Uuid,
    #[serde(with = "serde_bytes")]
    pub wrapped_keyring: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyringUpdate {
    pub per_device: Vec<DeviceKeyring>,
    #[serde(with = "serde_bytes::opt", default)]
    pub recovery_wrapped_keyring: Option<Vec<u8>>,
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
