//! User-facing notifications (native in the daemon, recorded in tests).

use std::sync::Mutex;

use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notification {
    Conflict {
        project: Uuid,
        path: String,
        from_device: String,
    },
    BinaryOverwritten {
        project: Uuid,
        path: String,
        from_device: String,
    },
    HookPending {
        project: Uuid,
        command: String,
    },
    VolumePrompt {
        project: Uuid,
        volume: String,
        from_device: String,
        at: i64,
    },
    MassDelete {
        project: Uuid,
        deleted: usize,
        total: usize,
    },
    MissingRoot {
        project: Uuid,
    },
    PlatformSkip {
        project: Uuid,
        path: String,
        reason: String,
    },
    GitBusy {
        project: Uuid,
    },
}

pub trait Notifier: Send + Sync {
    fn notify(&self, n: Notification);
}

#[derive(Default)]
pub struct NullNotifier;

impl Notifier for NullNotifier {
    fn notify(&self, _n: Notification) {}
}

/// Keeps every notification (tests, `spoko status` history).
#[derive(Default)]
pub struct RecordingNotifier {
    pub seen: Mutex<Vec<Notification>>,
}

impl RecordingNotifier {
    pub fn take(&self) -> Vec<Notification> {
        std::mem::take(&mut *self.seen.lock().expect("notifier"))
    }
}

impl Notifier for RecordingNotifier {
    fn notify(&self, n: Notification) {
        self.seen.lock().expect("notifier").push(n);
    }
}
