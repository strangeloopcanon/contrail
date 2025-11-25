use notify_rust::Notification;

pub struct Notifier;

impl Notifier {
    pub fn new() -> Self {
        Self
    }

    pub fn send_notification(&self, title: &str, body: &str) {
        let _ = Notification::new()
            .summary(title)
            .body(body)
            .sound_name("Glass") // Mac sound
            .show();
    }
}
