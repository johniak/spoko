use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::serde_bytes;

/// Pushed by the server over the account's WebSocket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    PointerChanged { project: Uuid, name: String, version: u64 },
    Message {
        project: Uuid,
        from_device: Uuid,
        #[serde(with = "serde_bytes")]
        payload: Vec<u8>,
    },
    PresenceChanged { project: Uuid, online_devices: Vec<Uuid> },
}
