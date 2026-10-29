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

fn display_path(p: &str) -> &str {
    p.strip_prefix("work/").unwrap_or(p)
}

impl Notification {
    /// Message shown to the user (Polish, like the rest of the UI).
    pub fn text(&self) -> String {
        match self {
            Notification::Conflict { path, from_device, .. } => {
                format!("Konflikt w {}: zostaje Twoja wersja, wersja z {from_device} jest w historii (spoko resolve).", display_path(path))
            }
            Notification::BinaryOverwritten { path, from_device, .. } => {
                format!("{}: nowsza wersja z {from_device} zastąpiła Twoją (starsza jest w historii).", display_path(path))
            }
            Notification::HookPending { command, .. } => format!("Zmieniły się zależności — uruchom: {command} (spoko hooks run)."),
            Notification::VolumePrompt { volume, from_device, .. } => format!("Wolumen {volume} z {from_device} jest nowszy — zastosować? (spoko volume pull)"),
            Notification::MassDelete { deleted, total, .. } => format!("Synchronizacja wstrzymana: znikło {deleted} z {total} plików. Potwierdź: spoko resume --confirm"),
            Notification::MissingRoot { .. } => "Synchronizacja wstrzymana: katalog projektu zniknął.".into(),
            Notification::PlatformSkip { path, reason, .. } => format!("Pominięto {path}: {reason}"),
            Notification::GitBusy { .. } => "Git jest zajęty — stan repozytorium zsynchronizuje się po zakończeniu operacji.".into(),
        }
    }
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
