//! Автообновление с GitHub Releases.
//!
//! HTTP живёт внутри процесса (ureq + schannel), без curl и без внешних
//! утилит. Windows-бинарник нельзя перезаписать, пока он запущен, поэтому
//! скачанный файл ложится рядом как `nanocord.new.exe`, а батник подменяет
//! exe после выхода процесса и перезапускает приложение.

use std::io::Write;
use std::path::Path;
use std::thread;
use std::time::Duration;

const REPO: &str = "AsmanovLev/nanocord";

fn http_get(url: &str) -> Result<String, String> {
    let resp = ureq::get(url)
        .set("User-Agent", "nanocord")
        .timeout(Duration::from_secs(20))
        .call()
        .map_err(|e| format!("запрос: {}", e))?;
    resp.into_string().map_err(|e| format!("чтение: {}", e))
}

fn http_download(url: &str, path: &Path) -> Result<(), String> {
    let resp = ureq::get(url)
        .set("User-Agent", "nanocord")
        .timeout(Duration::from_secs(120))
        .call()
        .map_err(|e| format!("запрос: {}", e))?;

    let mut reader = resp.into_reader();
    let mut file = std::fs::File::create(path).map_err(|e| format!("файл: {}", e))?;
    std::io::copy(&mut reader, &mut file).map_err(|e| format!("запись: {}", e))?;
    let _ = file.flush();
    Ok(())
}

pub fn check_and_install() {
    thread::spawn(|| {
        let current = env!("CARGO_PKG_VERSION");
        let url = format!("https://api.github.com/repos/{}/releases/latest", REPO);

        let body = match http_get(&url) {
            Ok(b) => b,
            Err(e) => {
                crate::diag::log(&format!("update: {}", e));
                return;
            }
        };

        let tag = match extract(&body, "\"tag_name\":\"") {
            Some(t) => t,
            None => {
                crate::diag::log("update: в ответе нет tag_name (нет релизов?)");
                return;
            }
        };
        let download = match extract(&body, "\"browser_download_url\":\"") {
            Some(u) => u,
            None => {
                crate::diag::log("update: в релизе нет файла");
                return;
            }
        };

        let latest = tag.trim_start_matches('v').to_string();
        crate::diag::log(&format!(
            "update: текущая {} / последняя {}",
            current, latest
        ));
        if !is_newer(&latest, current) {
            return;
        }

        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(_) => return,
        };
        let dir = exe.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let new_exe = dir.join("nanocord.new.exe");

        if let Err(e) = http_download(&download, &new_exe) {
            crate::diag::log(&format!("update: скачать не удалось ({})", e));
            return;
        }

        let bat = dir.join("nanocord_update.bat");
        let script = concat!(
            "@echo off\r\n",
            "timeout /t 2 /nobreak >nul\r\n",
            "move /y \"%~dp0nanocord.exe\" \"%~dp0nanocord.old.exe\"\r\n",
            "move /y \"%~dp0nanocord.new.exe\" \"%~dp0nanocord.exe\"\r\n",
            "start \"\" \"%~dp0nanocord.exe\"\r\n",
            "del \"%~dp0nanocord.old.exe\"\r\n"
        );
        let _ = std::fs::write(&bat, script);
        if std::process::Command::new("cmd")
            .arg("/c")
            .arg(&bat)
            .spawn()
            .is_ok()
        {
            crate::diag::log(&format!("update: v{} скачана, перезапуск", latest));
            thread::sleep(Duration::from_millis(500));
            std::process::exit(0);
        }
    });
}

fn extract(body: &str, key: &str) -> Option<String> {
    let start = body.find(key)? + key.len();
    let rest = &body[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> { v.split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    let a = parse(latest);
    let b = parse(current);
    for i in 0..a.len().max(b.len()) {
        let x = *a.get(i).unwrap_or(&0);
        let y = *b.get(i).unwrap_or(&0);
        if x != y {
            return x > y;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{extract, is_newer};

    #[test]
    fn version_comparison() {
        assert!(is_newer("0.2.2", "0.2.1"));
        assert!(!is_newer("0.2.1", "0.2.1"));
        assert!(!is_newer("0.1.9", "0.2.0"));
        assert!(is_newer("1.0", "0.9.9"));
    }

    #[test]
    fn json_field_extraction() {
        let body = r#"{"tag_name":"v9.9.9","assets":[{"browser_download_url":"https://x/y.exe"}]}"#;
        assert_eq!(extract(body, "\"tag_name\":\""), Some("v9.9.9".to_string()));
        assert_eq!(
            extract(body, "\"browser_download_url\":\""),
            Some("https://x/y.exe".to_string())
        );
    }
}
