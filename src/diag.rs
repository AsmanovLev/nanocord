use std::fs::OpenOptions;
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

/// Minimal file logger.
///
/// The app is built as a GUI subsystem binary, so `println!` goes nowhere and a
/// crash leaves no evidence. Everything that matters for diagnosis (encoder
/// choice, loopback mode, capture source, recoverable errors) is appended here.
pub fn log(message: &str) {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open("nanocord.log")
    {
        let _ = writeln!(file, "[{}] {}", stamp, message);
    }
}

pub fn log_once(message: &str) {
    static ONCE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !ONCE.swap(true, std::sync::atomic::Ordering::Relaxed) {
        log(message);
    }
}
