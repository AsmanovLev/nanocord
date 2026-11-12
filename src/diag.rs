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

/// Сохраняет RGBA-кадр в BMP рядом с exe. Нужно, чтобы увидеть, что реально
/// уходит в энкодер и что реально приходит из декодера, а не догадываться.
pub fn dump_frame(name: &str, width: u32, height: u32, rgba: &[u8]) {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let path = dir.join(format!("{}.bmp", name));

    let row = ((width * 3 + 3) / 4 * 4) as usize;
    let size = 54 + row * height as usize;
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(-(height as i32)).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0u8; 24]);

    for y in 0..height as usize {
        for x in 0..width as usize {
            let i = (y * width as usize + x) * 4;
            if i + 2 < rgba.len() {
                out.push(rgba[i + 2]);
                out.push(rgba[i + 1]);
                out.push(rgba[i]);
            } else {
                out.push(0);
                out.push(0);
                out.push(0);
            }
        }
        for _ in 0..(row - width as usize * 3) {
            out.push(0);
        }
    }

    let _ = std::fs::write(&path, out);
    log(&format!(
        "dump: {} {}x{} -> {}",
        name,
        width,
        height,
        path.display()
    ));
}
