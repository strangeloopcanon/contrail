use notify_rust::Notification;

pub struct Notifier;

impl Notifier {
    pub fn new() -> Self {
        Self
    }

    pub fn send_notification(&self, title: &str, body: &str) {
        if let Err(e) = Notification::new()
            .summary(title)
            .body(body)
            .sound_name("Glass")
            .show()
        {
            tracing::warn!(err = %e, "failed to send desktop notification");
        }
    }
}

impl Default for Notifier {
    fn default() -> Self {
        Self::new()
    }
}
