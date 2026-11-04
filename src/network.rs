#![allow(dead_code, unused_variables)]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile,
    FILE_MAP_ALL_ACCESS, FILE_MAP_READ, PAGE_READWRITE,
};

pub const SHM_HEADER_SIZE: usize = 256;
pub const DT_WIDTH: u32 = 1280;
pub const DT_HEIGHT: u32 = 720;
pub const DT_SIZE: usize = (DT_WIDTH * DT_HEIGHT * 4) as usize; // 3,686,400 bytes

pub const CAM_WIDTH: u32 = 640;
pub const CAM_HEIGHT: u32 = 360;
pub const CAM_SIZE: usize = (CAM_WIDTH * CAM_HEIGHT * 4) as usize; // 921,600 bytes

pub const SHM_TOTAL_SIZE: usize = SHM_HEADER_SIZE + DT_SIZE + CAM_SIZE; // ~4.61 MB
pub const SHM_MAGIC: u32 = 0x4E433031; // "NC01"

#[repr(C)]
pub struct ShmHeader {
    pub magic: u32,
    pub is_active: u32,
    pub dt_seq: u64,
    pub dt_w: u32,
    pub dt_h: u32,
    pub dt_offset: u32,
    pub dt_size: u32,
    pub cam_seq: u64,
    pub cam_w: u32,
    pub cam_h: u32,
    pub cam_offset: u32,
    pub cam_size: u32,
    pub fps: f32,
    pub bitrate_mbps: f32,
    pub latency_ms: f32,
    pub track_name: [u8; 128],
}

#[derive(Clone)]
pub struct NetworkPeerFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct P2PTelemetry {
    pub is_connected: bool,
    pub peer_addr: String,
    pub fps: f32,
    pub bitrate_mbps: f32,
    pub latency_ms: f32,
    pub current_track: String,
    pub total_frames: u64,
    pub total_bytes: u64,
}

/// Серверный аппаратный приёмник входящего P2P-потока через Windows Shared Memory (0% сетевых фризов)
pub struct P2PReceiver {
    is_running: Arc<AtomicBool>,
    pub is_connected: Arc<AtomicBool>,
    pub desktop_rx: Receiver<NetworkPeerFrame>,
    pub camera_rx: Receiver<NetworkPeerFrame>,
    pub telemetry: Arc<RwLock<P2PTelemetry>>,
}

impl P2PReceiver {
    pub fn start() -> Self {
        let is_running = Arc::new(AtomicBool::new(true));
        let is_connected = Arc::new(AtomicBool::new(false));
        let telemetry = Arc::new(RwLock::new(P2PTelemetry::default()));

        // Канал с буфером на 2 кадра
        let (dt_tx, dt_rx) = sync_channel(2);
        let (cam_tx, cam_rx) = sync_channel(2);

        let running_clone = is_running.clone();
        let connected_clone = is_connected.clone();
        let telem_clone = telemetry.clone();

        thread::spawn(move || {
            let shm_name_utf16: Vec<u16> = "Local\\Nanocord_P2P_SharedMem_v2\0".encode_utf16().collect();
            let mut last_dt_seq = 0u64;
            let mut last_cam_seq = 0u64;
            let mut dt_frame_buf = vec![0u8; DT_SIZE];
            let mut cam_frame_buf = vec![0u8; CAM_SIZE];

            while running_clone.load(Ordering::Relaxed) {
                // Попытка открыть именованную разделяемую память от mock-клиента
                let h_map = unsafe {
                    OpenFileMappingW(
                        FILE_MAP_READ.0,
                        false,
                        PCWSTR(shm_name_utf16.as_ptr()),
                    )
                };

                match h_map {
                    Ok(handle) if !handle.is_invalid() => {
                        let view_ptr = unsafe {
                            MapViewOfFile(
                                handle,
                                FILE_MAP_READ,
                                0,
                                0,
                                SHM_TOTAL_SIZE,
                            )
                        };

                        if !view_ptr.Value.is_null() {
                            println!("[P2P Server] Attached to Windows Shared Memory pipeline!");
                            connected_clone.store(true, Ordering::Relaxed);

                            let header_ptr = view_ptr.Value as *const ShmHeader;
                            let dt_ptr = unsafe { (view_ptr.Value as *const u8).add(SHM_HEADER_SIZE) };
                            let cam_ptr = unsafe { (view_ptr.Value as *const u8).add(SHM_HEADER_SIZE + DT_SIZE) };

                            while running_clone.load(Ordering::Relaxed) {
                                let header = unsafe { &*header_ptr };
                                if header.magic != SHM_MAGIC || header.is_active == 0 {
                                    break;
                                }

                                // 1. Чтение нового кадра рабочего стола
                                if header.dt_seq != last_dt_seq {
                                    last_dt_seq = header.dt_seq;
                                    unsafe {
                                        std::ptr::copy_nonoverlapping(dt_ptr, dt_frame_buf.as_mut_ptr(), DT_SIZE);
                                    }
                                    let _ = dt_tx.try_send(NetworkPeerFrame {
                                        width: header.dt_w,
                                        height: header.dt_h,
                                        rgba: dt_frame_buf.clone(),
                                    });
                                }

                                // 2. Чтение нового кадра камеры
                                if header.cam_seq != last_cam_seq {
                                    last_cam_seq = header.cam_seq;
                                    unsafe {
                                        std::ptr::copy_nonoverlapping(cam_ptr, cam_frame_buf.as_mut_ptr(), CAM_SIZE);
                                    }
                                    let _ = cam_tx.try_send(NetworkPeerFrame {
                                        width: header.cam_w,
                                        height: header.cam_h,
                                        rgba: cam_frame_buf.clone(),
                                    });
                                }

                                // 3. Обновление телеметрии
                                let track_end = header.track_name.iter().position(|&b| b == 0).unwrap_or(header.track_name.len());
                                let track_str = String::from_utf8_lossy(&header.track_name[..track_end]).to_string();
                                {
                                    let mut t = telem_clone.write().unwrap();
                                    t.is_connected = true;
                                    t.fps = header.fps;
                                    t.bitrate_mbps = header.bitrate_mbps;
                                    t.latency_ms = header.latency_ms;
                                    t.current_track = track_str;
                                    t.total_frames = header.dt_seq;
                                    t.total_bytes = header.dt_seq * (DT_SIZE as u64);
                                }

                                thread::sleep(Duration::from_millis(8)); // ~120 Hz проверка обновления
                            }

                            unsafe {
                                let _ = UnmapViewOfFile(view_ptr);
                            }
                        }

                        unsafe {
                            let _ = CloseHandle(handle);
                        }
                    }
                    _ => {
                        connected_clone.store(false, Ordering::Relaxed);
                        {
                            let mut t = telem_clone.write().unwrap();
                            t.is_connected = false;
                        }
                        thread::sleep(Duration::from_millis(250));
                    }
                }
            }
        });

        Self {
            is_running,
            is_connected,
            desktop_rx: dt_rx,
            camera_rx: cam_rx,
            telemetry,
        }
    }

    pub fn stop(&self) {
        self.is_running.store(false, Ordering::Relaxed);
    }
}

/// Встроенный в Nanocord генератор мок-источника (без внешних процессов и сокетов, 60 FPS + BGM)
pub struct InternalMockSource {
    pub is_running: Arc<AtomicBool>,
    pub desktop_rx: Receiver<NetworkPeerFrame>,
    pub camera_rx: Receiver<NetworkPeerFrame>,
    pub bgm_player: Arc<Mutex<BgmPlayer>>,
}

impl InternalMockSource {
    pub fn start() -> Self {
        let is_running = Arc::new(AtomicBool::new(true));
        let (dt_tx, dt_rx) = sync_channel(2);
        let (cam_tx, cam_rx) = sync_channel(2);

        let mut player = BgmPlayer::new();
        player.start_auto();
        let bgm_player = Arc::new(Mutex::new(player));

        let running_clone = is_running.clone();
        let bgm_clone = bgm_player.clone();

        thread::spawn(move || {
            let mut dt_seq = 0u64;
            let mut cam_seq = 0u64;
            let start_time = Instant::now();
            let frame_dur = Duration::from_millis(16); // 60 FPS

            while running_clone.load(Ordering::Relaxed) {
                let loop_start = Instant::now();
                dt_seq += 1;
                let elapsed = start_time.elapsed().as_secs_f32();
                let track = bgm_clone.lock().unwrap().current_track_name();

                // 1. Генерация 60 FPS экрана (1280x720)
                let dt_rgba = generate_mock_desktop_frame(DT_WIDTH, DT_HEIGHT, dt_seq, elapsed, &track);
                let _ = dt_tx.try_send(NetworkPeerFrame {
                    width: DT_WIDTH,
                    height: DT_HEIGHT,
                    rgba: dt_rgba,
                });

                // 2. Генерация 30 FPS камеры (640x360)
                if dt_seq % 2 == 0 {
                    cam_seq += 1;
                    let cam_rgba = generate_mock_camera_frame(CAM_WIDTH, CAM_HEIGHT, cam_seq, elapsed);
                    let _ = cam_tx.try_send(NetworkPeerFrame {
                        width: CAM_WIDTH,
                        height: CAM_HEIGHT,
                        rgba: cam_rgba,
                    });
                }

                let elapsed_frame = loop_start.elapsed();
                if frame_dur > elapsed_frame {
                    thread::sleep(frame_dur - elapsed_frame);
                }
            }

            bgm_clone.lock().unwrap().stop();
        });

        Self {
            is_running,
            desktop_rx: dt_rx,
            camera_rx: cam_rx,
            bgm_player,
        }
    }

    pub fn stop(&self) {
        self.is_running.store(false, Ordering::Relaxed);
        self.bgm_player.lock().unwrap().stop();
    }
}

/// Клиентский передатчик P2P потока для отдельного nanocord_mock.exe через Windows Shared Memory
pub struct P2PSender {
    pub is_running: Arc<AtomicBool>,
    pub is_connected: Arc<AtomicBool>,
    pub target_fps: Arc<AtomicU32>,
    pub bgm_player: Arc<Mutex<BgmPlayer>>,
    pub preview_dt_rx: Receiver<NetworkPeerFrame>,
    pub preview_cam_rx: Receiver<NetworkPeerFrame>,
    pub telemetry: Arc<RwLock<P2PTelemetry>>,
}

impl P2PSender {
    pub fn start() -> Self {
        let is_running = Arc::new(AtomicBool::new(true));
        let is_connected = Arc::new(AtomicBool::new(false));
        let target_fps = Arc::new(AtomicU32::new(60));
        let telemetry = Arc::new(RwLock::new(P2PTelemetry::default()));

        let mut player = BgmPlayer::new();
        player.start_auto();
        let bgm_player = Arc::new(Mutex::new(player));

        let (prev_dt_tx, prev_dt_rx) = sync_channel(2);
        let (prev_cam_tx, prev_cam_rx) = sync_channel(2);

        let running_clone = is_running.clone();
        let connected_clone = is_connected.clone();
        let fps_clone = target_fps.clone();
        let bgm_clone = bgm_player.clone();
        let telem_clone = telemetry.clone();

        thread::spawn(move || {
            let shm_name_utf16: Vec<u16> = "Local\\Nanocord_P2P_SharedMem_v2\0".encode_utf16().collect();
            let mut dt_seq = 0u64;
            let mut cam_seq = 0u64;
            let start_time = Instant::now();

            let h_map: windows::core::Result<HANDLE> = unsafe {
                CreateFileMappingW(
                    INVALID_HANDLE_VALUE,
                    None::<*const windows::Win32::Security::SECURITY_ATTRIBUTES>,
                    PAGE_READWRITE,
                    0,
                    SHM_TOTAL_SIZE as u32,
                    PCWSTR(shm_name_utf16.as_ptr()),
                )
            };

            let handle = match h_map {
                Ok(h) if !h.is_invalid() => h,
                _ => {
                    eprintln!("[Mock Client] Failed to create Windows Shared Memory");
                    return;
                }
            };

            let view_ptr = unsafe {
                MapViewOfFile(
                    handle,
                    FILE_MAP_ALL_ACCESS,
                    0,
                    0,
                    SHM_TOTAL_SIZE,
                )
            };

            if view_ptr.Value.is_null() {
                eprintln!("[Mock Client] Failed to map view of Windows Shared Memory");
                unsafe { let _ = CloseHandle(handle); }
                return;
            }

            println!("[Mock Client] Windows Shared Memory pipeline ready (4.6 MB zero-copy RAM)");
            connected_clone.store(true, Ordering::Relaxed);

            let header_ptr = view_ptr.Value as *mut ShmHeader;
            let dt_ptr = unsafe { (view_ptr.Value as *mut u8).add(SHM_HEADER_SIZE) };
            let cam_ptr = unsafe { (view_ptr.Value as *mut u8).add(SHM_HEADER_SIZE + DT_SIZE) };

            let mut frames_in_sec = 0u32;
            let mut sec_timer = Instant::now();

            while running_clone.load(Ordering::Relaxed) {
                let cur_fps = fps_clone.load(Ordering::Relaxed).clamp(15, 144);
                let frame_interval = Duration::from_secs_f32(1.0 / cur_fps as f32);
                let loop_start = Instant::now();

                dt_seq += 1;
                let elapsed = start_time.elapsed().as_secs_f32();
                let cur_track = bgm_clone.lock().unwrap().current_track_name();

                // 1. Быстрая генерация и запись экрана в разделяемую память (1280x720)
                let dt_rgba = generate_mock_desktop_frame(DT_WIDTH, DT_HEIGHT, dt_seq, elapsed, &cur_track);
                unsafe {
                    std::ptr::copy_nonoverlapping(dt_rgba.as_ptr(), dt_ptr, DT_SIZE);
                }
                let _ = prev_dt_tx.try_send(NetworkPeerFrame {
                    width: DT_WIDTH,
                    height: DT_HEIGHT,
                    rgba: dt_rgba,
                });

                // 2. Генерация и запись камеры в разделяемую память (640x360)
                if dt_seq % 2 == 0 {
                    cam_seq += 1;
                    let cam_rgba = generate_mock_camera_frame(CAM_WIDTH, CAM_HEIGHT, cam_seq, elapsed);
                    unsafe {
                        std::ptr::copy_nonoverlapping(cam_rgba.as_ptr(), cam_ptr, CAM_SIZE);
                    }
                    let _ = prev_cam_tx.try_send(NetworkPeerFrame {
                        width: CAM_WIDTH,
                        height: CAM_HEIGHT,
                        rgba: cam_rgba,
                    });
                }

                frames_in_sec += 1;
                let now_fps = if sec_timer.elapsed() >= Duration::from_secs(1) {
                    let f = frames_in_sec as f32 / sec_timer.elapsed().as_secs_f32();
                    frames_in_sec = 0;
                    sec_timer = Instant::now();
                    f
                } else {
                    cur_fps as f32
                };

                // 3. Обновление заголовка в Shared Memory
                let mut track_bytes = [0u8; 128];
                let tb = cur_track.as_bytes();
                let clen = tb.len().min(127);
                track_bytes[..clen].copy_from_slice(&tb[..clen]);

                let bitrate = (now_fps * (DT_SIZE + CAM_SIZE / 2) as f32 * 8.0) / 1_000_000.0;

                unsafe {
                    *header_ptr = ShmHeader {
                        magic: SHM_MAGIC,
                        is_active: 1,
                        dt_seq,
                        dt_w: DT_WIDTH,
                        dt_h: DT_HEIGHT,
                        dt_offset: SHM_HEADER_SIZE as u32,
                        dt_size: DT_SIZE as u32,
                        cam_seq,
                        cam_w: CAM_WIDTH,
                        cam_h: CAM_HEIGHT,
                        cam_offset: (SHM_HEADER_SIZE + DT_SIZE) as u32,
                        cam_size: CAM_SIZE as u32,
                        fps: now_fps,
                        bitrate_mbps: bitrate,
                        latency_ms: 0.1, // Нулевая задержка памяти
                        track_name: track_bytes,
                    };
                }

                {
                    let mut t = telem_clone.write().unwrap();
                    t.is_connected = true;
                    t.fps = now_fps;
                    t.bitrate_mbps = bitrate;
                    t.latency_ms = 0.1;
                    t.current_track = cur_track;
                    t.total_frames = dt_seq;
                    t.total_bytes = dt_seq * (DT_SIZE as u64);
                }

                let elapsed_frame = loop_start.elapsed();
                if frame_interval > elapsed_frame {
                    thread::sleep(frame_interval - elapsed_frame);
                }
            }

            // Завершение работы
            unsafe {
                (*header_ptr).is_active = 0;
                let _ = UnmapViewOfFile(view_ptr);
                let _ = CloseHandle(handle);
            }
        });

        Self {
            is_running,
            is_connected,
            target_fps,
            bgm_player,
            preview_dt_rx: prev_dt_rx,
            preview_cam_rx: prev_cam_rx,
            telemetry,
        }
    }

    pub fn stop(&self) {
        self.is_running.store(false, Ordering::Relaxed);
        self.bgm_player.lock().unwrap().stop();
    }
}

/// Плеер фоновой музыки из Downloads через Windows MCI winmm.dll
pub struct BgmPlayer {
    pub tracks: Vec<(String, String)>,
    pub current_idx: usize,
    pub is_playing: bool,
}

impl BgmPlayer {
    pub fn new() -> Self {
        let mut tracks = Vec::new();
        let user_profile = std::env::var("USERPROFILE").unwrap_or_default();
        let dirs = [
            format!("{}\\Downloads\\64Gram Desktop", user_profile),
            format!("{}\\Downloads", user_profile),
        ];

        for d in &dirs {
            if let Ok(entries) = std::fs::read_dir(d) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if let Some(ext) = p.extension() {
                        if ext.to_string_lossy().to_lowercase() == "mp3" {
                            let name = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Трек".into());
                            let path_str = p.to_string_lossy().to_string();
                            if !tracks.iter().any(|(n, _)| n == &name) {
                                tracks.push((name, path_str));
                            }
                        }
                    }
                }
            }
        }

        Self {
            tracks,
            current_idx: 0,
            is_playing: false,
        }
    }

    pub fn start_auto(&mut self) {
        if !self.tracks.is_empty() {
            self.play_index(0);
        }
    }

    pub fn play_index(&mut self, idx: usize) {
        if self.tracks.is_empty() {
            return;
        }
        self.current_idx = idx % self.tracks.len();
        let (_, ref path) = self.tracks[self.current_idx];

        unsafe {
            type MciFn = unsafe extern "system" fn(*const u16, *mut u16, u32, usize) -> u32;
            let winmm: Vec<u16> = "winmm.dll\0".encode_utf16().collect();
            if let Ok(h) = windows::Win32::System::LibraryLoader::LoadLibraryW(windows::core::PCWSTR(winmm.as_ptr())) {
                if let Some(proc) = windows::Win32::System::LibraryLoader::GetProcAddress(
                    h,
                    windows::core::PCSTR(b"mciSendStringW\0".as_ptr()),
                ) {
                    let mci: MciFn = std::mem::transmute(proc);
                    let _ = mci("close bgm\0".encode_utf16().collect::<Vec<_>>().as_ptr(), std::ptr::null_mut(), 0, 0);
                    let open_cmd: Vec<u16> = format!("open \"{}\" type mpegvideo alias bgm\0", path).encode_utf16().collect();
                    let play_cmd: Vec<u16> = "play bgm repeat\0".encode_utf16().collect();
                    let _ = mci(open_cmd.as_ptr(), std::ptr::null_mut(), 0, 0);
                    let _ = mci(play_cmd.as_ptr(), std::ptr::null_mut(), 0, 0);
                    self.is_playing = true;
                }
            }
        }
    }

    pub fn next(&mut self) {
        if !self.tracks.is_empty() {
            let next_idx = (self.current_idx + 1) % self.tracks.len();
            self.play_index(next_idx);
        }
    }

    pub fn prev(&mut self) {
        if !self.tracks.is_empty() {
            let prev_idx = if self.current_idx == 0 { self.tracks.len() - 1 } else { self.current_idx - 1 };
            self.play_index(prev_idx);
        }
    }

    pub fn toggle_pause(&mut self) {
        unsafe {
            type MciFn = unsafe extern "system" fn(*const u16, *mut u16, u32, usize) -> u32;
            let winmm: Vec<u16> = "winmm.dll\0".encode_utf16().collect();
            if let Ok(h) = windows::Win32::System::LibraryLoader::LoadLibraryW(windows::core::PCWSTR(winmm.as_ptr())) {
                if let Some(proc) = windows::Win32::System::LibraryLoader::GetProcAddress(
                    h,
                    windows::core::PCSTR(b"mciSendStringW\0".as_ptr()),
                ) {
                    let mci: MciFn = std::mem::transmute(proc);
                    if self.is_playing {
                        let _ = mci("pause bgm\0".encode_utf16().collect::<Vec<_>>().as_ptr(), std::ptr::null_mut(), 0, 0);
                        self.is_playing = false;
                    } else {
                        let _ = mci("resume bgm\0".encode_utf16().collect::<Vec<_>>().as_ptr(), std::ptr::null_mut(), 0, 0);
                        self.is_playing = true;
                    }
                }
            }
        }
    }

    pub fn stop(&mut self) {
        unsafe {
            type MciFn = unsafe extern "system" fn(*const u16, *mut u16, u32, usize) -> u32;
            let winmm: Vec<u16> = "winmm.dll\0".encode_utf16().collect();
            if let Ok(h) = windows::Win32::System::LibraryLoader::LoadLibraryW(windows::core::PCWSTR(winmm.as_ptr())) {
                if let Some(proc) = windows::Win32::System::LibraryLoader::GetProcAddress(
                    h,
                    windows::core::PCSTR(b"mciSendStringW\0".as_ptr()),
                ) {
                    let mci: MciFn = std::mem::transmute(proc);
                    let _ = mci("close bgm\0".encode_utf16().collect::<Vec<_>>().as_ptr(), std::ptr::null_mut(), 0, 0);
                    self.is_playing = false;
                }
            }
        }
    }

    pub fn current_track_name(&self) -> String {
        if self.tracks.is_empty() {
            "Синтетический аудиопоток".to_string()
        } else {
            self.tracks[self.current_idx].0.clone()
        }
    }
}

/// Сверхбыстрая генерация динамического 1280x720 видеокадра (побитовые операции & 63, < 1ms на кадр)
pub fn generate_mock_desktop_frame(w: u32, h: u32, seq: u64, elapsed: f32, track_name: &str) -> Vec<u8> {
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let w_usize = w as usize;
    let scanline = ((elapsed * 240.0) as usize) % (h as usize);

    for y in 0..h as usize {
        let y_row = y * w_usize * 4;
        let is_scan = (y as isize - scanline as isize).abs() < 3;
        let grid_y = (y & 63) == 0;

        for x in 0..w_usize {
            let o = y_row + x * 4;
            let grid_x = (x & 63) == 0;

            let (mut r, mut g, mut b) = if grid_x || grid_y {
                (35u8, 45u8, 70u8)
            } else {
                (18u8, 22u8, 32u8)
            };

            // Центральный баннер
            if y >= 260 && y <= 460 && x >= 240 && x <= (w_usize - 240) {
                if y == 260 || y == 460 || x == 240 || x == (w_usize - 240) {
                    r = 88; g = 101; b = 242;
                } else {
                    r = 24; g = 28; b = 40;
                }
            }

            // Сканирующая полоса
            if is_scan {
                r = r.saturating_add(45);
                g = g.saturating_add(90);
                b = b.saturating_add(140);
            }

            // Нижняя информационная плашка
            if y >= 640 {
                r = 14; g = 16; b = 22;
            }

            buf[o] = r;
            buf[o + 1] = g;
            buf[o + 2] = b;
            buf[o + 3] = 255;
        }
    }

    buf
}

/// Сверхбыстрая генерация кадра веб-камеры (без sqrt, по квадратам радиусов)
pub fn generate_mock_camera_frame(w: u32, h: u32, seq: u64, elapsed: f32) -> Vec<u8> {
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let w_usize = w as usize;

    let center_x = (w / 2) as f32 + (elapsed * 1.5).sin() * 20.0;
    let center_y = (h / 2) as f32 + (elapsed * 1.0).cos() * 10.0;
    let r_sq_inner = 65.0 * 65.0;
    let r_sq_outer = 68.0 * 68.0;

    for y in 0..h as usize {
        let y_row = y * w_usize * 4;
        let dy = y as f32 - center_y;
        let dy_sq = dy * dy;

        for x in 0..w_usize {
            let o = y_row + x * 4;
            let dx = x as f32 - center_x;
            let dist_sq = dx * dx + dy_sq;

            let (r, g, b) = if dist_sq < r_sq_inner {
                (55u8, 68u8, 95u8)
            } else if dist_sq < r_sq_outer {
                (88u8, 101u8, 242u8)
            } else {
                (25u8, 28u8, 36u8)
            };

            buf[o] = r;
            buf[o + 1] = g;
            buf[o + 2] = b;
            buf[o + 3] = 255;
        }
    }

    buf
}
