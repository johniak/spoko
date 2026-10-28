use std::collections::HashMap;
use std::sync::Mutex;

use spoko_proto::ServerEvent;
use tokio::sync::broadcast;
use uuid::Uuid;

/// Fan-out of events to the WebSocket connections of one account.
#[derive(Default)]
pub struct Hub {
    inner: Mutex<HubInner>,
}

#[derive(Default)]
struct HubInner {
    channels: HashMap<Uuid, broadcast::Sender<Envelope>>,
    online: HashMap<Uuid, HashMap<Uuid, usize>>,
}

/// An event plus the device that caused it (so it is not echoed back).
#[derive(Debug, Clone)]
pub struct Envelope {
    pub from_device: Option<Uuid>,
    pub event: ServerEvent,
}

impl Hub {
    pub fn subscribe(&self, account: Uuid) -> broadcast::Receiver<Envelope> {
        let mut inner = self.inner.lock().expect("hub mutex");
        inner.channels.entry(account).or_insert_with(|| broadcast::channel(1024).0).subscribe()
    }

    pub fn publish(&self, account: Uuid, from_device: Option<Uuid>, event: ServerEvent) {
        let inner = self.inner.lock().expect("hub mutex");
        if let Some(tx) = inner.channels.get(&account) {
            let _ = tx.send(Envelope { from_device, event });
        }
    }

    /// Marks a device connected (+1) or disconnected (-1); returns the online set afterwards.
    pub fn set_online(&self, account: Uuid, device: Uuid, connected: bool) -> Vec<Uuid> {
        let mut inner = self.inner.lock().expect("hub mutex");
        let devices = inner.online.entry(account).or_default();
        let n = devices.entry(device).or_insert(0);
        if connected {
            *n += 1;
        } else {
            *n = n.saturating_sub(1);
            if *n == 0 {
                devices.remove(&device);
            }
        }
        let mut list: Vec<Uuid> = devices.keys().copied().collect();
        list.sort();
        list
    }

    pub fn online(&self, account: Uuid) -> Vec<Uuid> {
        let inner = self.inner.lock().expect("hub mutex");
        let mut list: Vec<Uuid> = inner.online.get(&account).map(|d| d.keys().copied().collect()).unwrap_or_default();
        list.sort();
        list
    }
}
