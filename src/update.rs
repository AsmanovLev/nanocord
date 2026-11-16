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

/// Текущее состояние апдейтера — показывается в интерфейсе, чтобы не гадать
/// по логам, работает ли он.
fn status_slot() -> &'static std::sync::Mutex<String> {
    static SLOT: std::sync::OnceLock<std::sync::Mutex<String>> = std::sync::OnceLock::new();
    SLOT.get_or_init(|| std::sync::Mutex::new("проверка…".to_string()))
}

pub fn status() -> String {
    status_slot()
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default()
}

/// Скачано и всего байт — пока идёт загрузка.
fn progress_slot() -> &'static std::sync::Mutex<Option<(u64, u64)>> {
    static SLOT: std::sync::OnceLock<std::sync::Mutex<Option<(u64, u64)>>> =
        std::sync::OnceLock::new();
    SLOT.get_or_init(|| std::sync::Mutex::new(None))
}

/// Прогресс загрузки для интерфейса: `None`, когда ничего не качается.
pub fn progress() -> Option<(u64, u64)> {
    progress_slot().lock().ok().and_then(|p| *p)
}

fn set_progress(value: Option<(u64, u64)>) {
    if let Ok(mut slot) = progress_slot().lock() {
        *slot = value;
    }
}

fn set_status(text: &str) {
    if let Ok(mut s) = status_slot().lock() {
        *s = text.to_string();
    }
    crate::diag::log(&format!("update: {}", text));
}

fn http_get(url: &str) -> Result<String, String> {
    let resp = ureq::get(url)
        .set("User-Agent", "nanocord")
        .timeout(Duration::from_secs(20))
        .call()
        .map_err(|e| format!("запрос: {}", e))?;
    resp.into_string().map_err(|e| format!("чтение: {}", e))
}

fn http_download(url: &str, path: &Path, version: &str) -> Result<(), String> {
    let resp = ureq::get(url)
        .set("User-Agent", "nanocord")
        .timeout(Duration::from_secs(120))
        .call()
        .map_err(|e| format!("запрос: {}", e))?;

    // Размер нужен для процентов: без него пользователь видит «скачиваю…»
    // и не понимает, идёт дело или загрузка встала.
    let total: u64 = resp
        .header("Content-Length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    set_progress(Some((0, total)));

    let mut reader = resp.into_reader();
    let mut file = std::fs::File::create(path).map_err(|e| format!("файл: {}", e))?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut done: u64 = 0;
    let mut last_reported = 0u64;
    loop {
        let read = std::io::Read::read(&mut reader, &mut buf).map_err(|e| format!("чтение: {}", e))?;
        if read == 0 {
            break;
        }
        file.write_all(&buf[..read]).map_err(|e| format!("запись: {}", e))?;
        done += read as u64;
        set_progress(Some((done, total)));
        // В журнал — по десятым долям, чтобы прогресс был виден и без UI.
        if total > 0 && done - last_reported >= total / 10 {
            last_reported = done;
            crate::diag::log(&format!(
                "update: скачивание v{version} {}% ({:.1} из {:.1} МБ)",
                done * 100 / total,
                done as f64 / 1_048_576.0,
                total as f64 / 1_048_576.0
            ));
            set_status(&format!(
                "скачиваю v{version}… {}% ({:.1}/{:.1} МБ)",
                done * 100 / total,
                done as f64 / 1_048_576.0,
                total as f64 / 1_048_576.0
            ));
        }
    }
    let _ = file.flush();
    set_progress(None);
    Ok(())
}

/// Проверка при старте и дальше раз в полчаса: приложение может работать
/// сутками, и «проверили один раз при запуске» означало, что обновление
/// приходит только после ручного перезапуска.
pub fn check_and_install() {
    if std::env::var("NANOCORD_NO_UPDATE").is_ok() {
        crate::diag::log("update: disabled by NANOCORD_NO_UPDATE");
        return;
    }
    thread::spawn(|| loop {
        run(true);
        thread::sleep(Duration::from_secs(30 * 60));
    });
}

/// Принудительная проверка с установкой: `nanocord.exe --update`.
pub fn force_update() -> String {
    run(true);
    status()
}

fn run(install: bool) {
    let current = env!("CARGO_PKG_VERSION");
    let url = format!("https://api.github.com/repos/{}/releases/latest", REPO);

    let body = match http_get(&url) {
        Ok(b) => b,
        Err(e) => {
            set_status(&format!("ошибка: {}", e));
            return;
        }
    };

    let tag = match extract(&body, "\"tag_name\"") {
        Some(t) => t,
        None => {
            crate::diag::log("update: в ответе нет tag_name (нет релизов?)");
            return;
        }
    };
    // Берём именно exe, а не первый попавшийся ассет: к релизу может быть
    // приложен и архив, и тогда обновление скачивало zip вместо программы.
    let download = match pick_exe_asset(&body) {
        Some(u) => u,
        None => {
            crate::diag::log("update: в релизе нет .exe");
            return;
        }
    };

    let latest = tag.trim_start_matches('v').to_string();
    if !is_newer(&latest, current) {
        set_status(&format!("v{} — актуальная", current));
        return;
    }
    if !install {
        set_status(&format!("доступна v{}", latest));
        return;
    }
    set_status(&format!("скачиваю v{}…", latest));

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };
    let dir = exe.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let new_exe = dir.join("nanocord.new.exe");

    if let Err(e) = http_download(&download, &new_exe, &latest) {
        set_progress(None);
        crate::diag::log(&format!("update: скачать не удалось ({})", e));
        return;
    }
    // Недокачанный или пустой файл подменять нельзя: приложение просто исчезнет.
    let size = std::fs::metadata(&new_exe).map(|m| m.len()).unwrap_or(0);
    if size < 1_000_000 {
        crate::diag::log(&format!("update: файл подозрительно мал ({size} байт), отмена"));
        let _ = std::fs::remove_file(&new_exe);
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
        set_status(&format!("v{} скачана, перезапуск", latest));
        thread::sleep(Duration::from_millis(500));
        std::process::exit(0);
    }
}

/// Ищет в JSON релиза ссылку на ассет с расширением .exe.
fn pick_exe_asset(body: &str) -> Option<String> {
    let mut rest = body;
    while let Some(value) = extract(rest, "\"browser_download_url\"") {
        if value.to_ascii_lowercase().ends_with(".exe") {
            return Some(value);
        }
        let next = rest.find(&value)? + value.len();
        if next >= rest.len() {
            break;
        }
        rest = &rest[next..];
    }
    None
}

/// Достаёт строковое значение поля JSON.
///
/// GitHub отдаёт pretty-printed JSON с пробелом после двоеточия
/// (`"tag_name": "v0.3.5"`), а поиск по точному `"tag_name":"` его не находил:
/// апдейтер считал, что новых релизов нет.
fn extract(body: &str, key: &str) -> Option<String> {
    let mut rest = body;
    while let Some(pos) = rest.find(key) {
        let after_key = &rest[pos + key.len()..];
        let trimmed = after_key.trim_start();
        if let Some(stripped) = trimmed.strip_prefix(':') {
            let value = stripped.trim_start();
            if let Some(stripped) = value.strip_prefix('"') {
                if let Some(end) = stripped.find('"') {
                    return Some(stripped[..end].to_string());
                }
            }
        }
        rest = after_key;
    }
    None
}

/// Сравнение версий по компонентам: 0.3.10 новее 0.3.9.
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
    use super::{extract, is_newer, pick_exe_asset};

    #[test]
    fn version_comparison() {
        assert!(is_newer("0.2.2", "0.2.1"));
        assert!(!is_newer("0.2.1", "0.2.1"));
        assert!(!is_newer("0.1.9", "0.2.0"));
        assert!(is_newer("1.0", "0.9.9"));
    }

    #[test]
    fn json_field_extraction() {
        let compact = r#"{"tag_name":"v9.9.9","assets":[{"browser_download_url":"https://x/y.exe"}]}"#;
        assert_eq!(extract(compact, "\"tag_name\""), Some("v9.9.9".to_string()));
        assert_eq!(pick_exe_asset(compact), Some("https://x/y.exe".to_string()));

        // Именно так отвечает api.github.com — с пробелом после двоеточия.
        let pretty = r#"{
  "tag_name": "v0.3.5",
  "assets": [
    { "name": "nanocord-0.3.5.exe", "browser_download_url": "https://x/a.zip" },
    { "name": "nanocord-0.3.5.exe", "browser_download_url": "https://x/b.exe" }
  ]
}"#;
        assert_eq!(extract(pretty, "\"tag_name\""), Some("v0.3.5".to_string()));
        assert_eq!(pick_exe_asset(pretty), Some("https://x/b.exe".to_string()));
    }
}
