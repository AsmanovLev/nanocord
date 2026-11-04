#![allow(dead_code, unused_variables)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

pub struct NetworkPeerFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub struct NetworkTestController {
    pub is_running: Arc<AtomicBool>,
    pub desktop_rx: Receiver<NetworkPeerFrame>,
    pub camera_rx: Receiver<NetworkPeerFrame>,
    pub current_track: String,
}

impl NetworkTestController {
    pub fn start() -> Self {
        let is_running = Arc::new(AtomicBool::new(true));
        let (dt_tx, dt_rx) = sync_channel(2);
        let (cam_tx, cam_rx) = sync_channel(2);

        // Поиск доступного музыкального трека в Downloads
        let track_name = find_and_start_bgm();

        let running_clone = is_running.clone();
        let track_display = track_name.clone();

        thread::spawn(move || {
            let mut frame_seq = 0u64;
            let start_time = Instant::now();

            let dt_w = 1280u32;
            let dt_h = 720u32;
            let cam_w = 640u32;
            let cam_h = 360u32;

            while running_clone.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(33)); // ~30-60 FPS
                frame_seq += 1;
                let elapsed = start_time.elapsed().as_secs_f32();

                // 1. Генерация кадра удаленного рабочего стола собеседника
                let dt_rgba = generate_mock_desktop_frame(dt_w, dt_h, frame_seq, elapsed, &track_display);
                let _ = dt_tx.try_send(NetworkPeerFrame {
                    width: dt_w,
                    height: dt_h,
                    rgba: dt_rgba,
                });

                // 2. Генерация кадра удаленной веб-камеры собеседника (каждый 2-й кадр ~30 FPS)
                if frame_seq % 2 == 0 {
                    let cam_rgba = generate_mock_camera_frame(cam_w, cam_h, frame_seq, elapsed);
                    let _ = cam_tx.try_send(NetworkPeerFrame {
                        width: cam_w,
                        height: cam_h,
                        rgba: cam_rgba,
                    });
                }
            }

            // Остановка музыки при завершении
            stop_bgm();
        });

        Self {
            is_running,
            desktop_rx: dt_rx,
            camera_rx: cam_rx,
            current_track: track_name,
        }
    }

    pub fn stop(&self) {
        self.is_running.store(false, Ordering::Relaxed);
        stop_bgm();
    }
}

/// Находит MP3 файл в загрузках и запускает воспроизведение через Windows winmm.dll
fn find_and_start_bgm() -> String {
    let mut chosen_path: Option<String> = None;
    let mut chosen_name = "Синтетический аудиопоток".to_string();

    let user_profile = std::env::var("USERPROFILE").unwrap_or_default();
    let download_dirs = [
        format!("{}\\Downloads\\64Gram Desktop", user_profile),
        format!("{}\\Downloads", user_profile),
    ];

    for dir in &download_dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(ext) = path.extension() {
                    if ext.to_string_lossy().to_lowercase() == "mp3" {
                        if let Some(name) = path.file_stem() {
                            chosen_name = name.to_string_lossy().to_string();
                        }
                        chosen_path = Some(path.to_string_lossy().to_string());
                        break;
                    }
                }
            }
        }
        if chosen_path.is_some() {
            break;
        }
    }

    if let Some(path) = chosen_path {
        unsafe {
            type MciSendStringWFn = unsafe extern "system" fn(*const u16, *mut u16, u32, usize) -> u32;
            let winmm: Vec<u16> = "winmm.dll\0".encode_utf16().collect();
            if let Ok(h) = windows::Win32::System::LibraryLoader::LoadLibraryW(windows::core::PCWSTR(winmm.as_ptr())) {
                if let Some(proc) = windows::Win32::System::LibraryLoader::GetProcAddress(
                    h,
                    windows::core::PCSTR(b"mciSendStringW\0".as_ptr()),
                ) {
                    let mci: MciSendStringWFn = std::mem::transmute(proc);
                    let _ = mci("close bgm\0".encode_utf16().collect::<Vec<_>>().as_ptr(), std::ptr::null_mut(), 0, 0);
                    let open_cmd: Vec<u16> = format!("open \"{}\" type mpegvideo alias bgm\0", path).encode_utf16().collect();
                    let play_cmd: Vec<u16> = "play bgm repeat\0".encode_utf16().collect();
                    let _ = mci(open_cmd.as_ptr(), std::ptr::null_mut(), 0, 0);
                    let _ = mci(play_cmd.as_ptr(), std::ptr::null_mut(), 0, 0);
                }
            }
        }
    }

    chosen_name
}

fn stop_bgm() {
    unsafe {
        type MciSendStringWFn = unsafe extern "system" fn(*const u16, *mut u16, u32, usize) -> u32;
        let winmm: Vec<u16> = "winmm.dll\0".encode_utf16().collect();
        if let Ok(h) = windows::Win32::System::LibraryLoader::LoadLibraryW(windows::core::PCWSTR(winmm.as_ptr())) {
            if let Some(proc) = windows::Win32::System::LibraryLoader::GetProcAddress(
                h,
                windows::core::PCSTR(b"mciSendStringW\0".as_ptr()),
            ) {
                let mci: MciSendStringWFn = std::mem::transmute(proc);
                let close_cmd: Vec<u16> = "close bgm\0".encode_utf16().collect();
                let _ = mci(close_cmd.as_ptr(), std::ptr::null_mut(), 0, 0);
            }
        }
    }
}

/// Генерация динамического 1280x720 видеокадра удаленного экрана с технологичным тестовым паттерном
fn generate_mock_desktop_frame(w: u32, h: u32, seq: u64, elapsed: f32, track_name: &str) -> Vec<u8> {
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let w_usize = w as usize;

    let scanline = ((elapsed * 240.0) as usize) % (h as usize);
    let pulse = (elapsed * 3.0).sin().abs();

    for y in 0..h as usize {
        let y_row = y * w_usize * 4;
        let is_scan = (y as isize - scanline as isize).abs() < 3;

        for x in 0..w_usize {
            let o = y_row + x * 4;

            // Технологичная кибер-сетка
            let grid_x = x % 64 == 0;
            let grid_y = y % 64 == 0;

            let mut r = 18u8;
            let mut g = 22u8;
            let mut b = 32u8;

            if grid_x || grid_y {
                r = 35;
                g = 45;
                b = 70;
            }

            // Центральный баннер
            if y >= 260 && y <= 460 && x >= 240 && x <= (w_usize - 240) {
                r = 24;
                g = 28;
                b = 40;
                // Рамка баннера
                if y == 260 || y == 460 || x == 240 || x == (w_usize - 240) {
                    r = 88;
                    g = 101;
                    b = 242;
                }
            }

            // Сканирующая полоса
            if is_scan {
                r = (r as f32 * 1.5 + 40.0).min(255.0) as u8;
                g = (g as f32 * 1.5 + 80.0).min(255.0) as u8;
                b = (b as f32 * 1.8 + 120.0).min(255.0) as u8;
            }

            // Нижняя информационная плашка
            if y >= 640 {
                r = 14;
                g = 16;
                b = 22;
            }

            buf[o] = r;
            buf[o + 1] = g;
            buf[o + 2] = b;
            buf[o + 3] = 255;
        }
    }

    buf
}

/// Генерация динамического 640x360 кадра веб-камеры собеседника
fn generate_mock_camera_frame(w: u32, h: u32, seq: u64, elapsed: f32) -> Vec<u8> {
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let w_usize = w as usize;

    let center_x = (w / 2) as f32 + (elapsed * 1.5).sin() * 20.0;
    let center_y = (h / 2) as f32 + (elapsed * 1.0).cos() * 10.0;

    for y in 0..h as usize {
        let y_row = y * w_usize * 4;
        let dy = y as f32 - center_y;

        for x in 0..w_usize {
            let o = y_row + x * 4;
            let dx = x as f32 - center_x;
            let dist = (dx * dx + dy * dy).sqrt();

            let mut r = 25u8;
            let mut g = 28u8;
            let mut b = 36u8;

            // Силуэт собеседника
            if dist < 65.0 {
                r = 55;
                g = 68;
                b = 95;
            } else if dist < 67.0 {
                r = 88;
                g = 101;
                b = 242;
            }

            buf[o] = r;
            buf[o + 1] = g;
            buf[o + 2] = b;
            buf[o + 3] = 255;
        }
    }

    buf
}
