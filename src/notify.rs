//! Notification native de l'OS (D-Bus sous Linux, toast sous Windows), hors du fil de l'interface.
use std::thread;

pub fn show(title: String, body: String, on_error: impl FnOnce(String) + Send + 'static) {
    thread::spawn(move || {
        let sent = notify_rust::Notification::new().appname("MCO Dash").summary(&title).body(&body).show();
        if let Err(e) = sent {
            on_error(format!("notification système impossible : {e}"));
        }
    });
}
