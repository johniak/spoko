use crate::notifier::{Notification, Notifier};

/// Desktop notifications (macOS Notification Center, freedesktop, Windows toasts).
#[derive(Default)]
pub struct NativeNotifier;

impl Notifier for NativeNotifier {
    fn notify(&self, n: Notification) {
        let _ = notify_rust::Notification::new().summary("spoko").body(&n.text()).show();
    }
}
