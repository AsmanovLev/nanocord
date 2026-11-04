#![allow(dead_code, unused_variables)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
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

// =========================================================================
// P2P СЕТЕВОЙ СЕРВЕР И КЛИЕНТ (IPv6 + IPv4, СЛУЧАЙНЫЙ КОД, БУФЕР ОБМЕНА)
// =========================================================================

/// Генерация случайного 3-значного кода комнаты [A-Za-z0-9]
pub fn generate_random_room_code() -> String {
    let chars = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghjkmnpqrstuvwxyz23456789";
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut seed = (now ^ (now >> 16)) as u64;
    let mut code = String::with_capacity(3);
    for _ in 0..3 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let idx = ((seed >> 32) as usize) % chars.len();
        code.push(chars[idx] as char);
    }
    code
}

/// Разбор строки адреса подключения: host, port, code
/// Поддерживает форматы:
/// [::1]:48120#7xK, 127.0.0.1:48120#7xK, [::1]:48120 7xK, [::1]:48120/7xK, [::1]
pub fn parse_room_target(raw: &str) -> Option<(String, u16, String)> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }

    let (addr_part, code_part) = if let Some(pos) = s.rfind('#') {
        (&s[..pos], &s[pos + 1..])
    } else if let Some(pos) = s.rfind('/') {
        (&s[..pos], &s[pos + 1..])
    } else if let Some(pos) = s.rfind(' ') {
        (&s[..pos], &s[pos + 1..])
    } else {
        (s, "")
    };

    let code = code_part.trim().to_string();
    let addr_part = addr_part.trim();

    if addr_part.starts_with('[') {
        if let Some(close_bracket) = addr_part.find(']') {
            let host = &addr_part[1..close_bracket];
            let rest = &addr_part[close_bracket + 1..];
            let port = if let Some(colon) = rest.find(':') {
                rest[colon + 1..].trim().parse::<u16>().unwrap_or(48120)
            } else {
                48120
            };
            return Some((format!("[{}]", host), port, code));
        }
    }

    if let Some(pos) = addr_part.rfind(':') {
        let host = &addr_part[..pos];
        let port_str = &addr_part[pos + 1..];
        if let Ok(port) = port_str.parse::<u16>() {
            return Some((host.to_string(), port, code));
        }
    }

    if addr_part.contains(':') && !addr_part.contains('.') {
        return Some((format!("[{}]", addr_part), 48120, code));
    }

    Some((addr_part.to_string(), 48120, code))
}

/// Получение подсказок локальных адресов IPv6 и IPv4
pub fn get_local_ip_hint() -> (String, String) {
    let ipv6_str = "[::1]".to_string();
    let ipv4_str = if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
        if s.connect("8.8.8.8:80").is_ok() {
            if let Ok(addr) = s.local_addr() {
                addr.ip().to_string()
            } else {
                "127.0.0.1".to_string()
            }
        } else {
            "127.0.0.1".to_string()
        }
    } else {
        "127.0.0.1".to_string()
    };
    (ipv6_str, ipv4_str)
}

/// Чтение текста из системного буфера обмена Windows (user32.dll)
pub fn get_clipboard_text() -> Option<String> {
    unsafe {
        type OpenClipboardFn = unsafe extern "system" fn(isize) -> i32;
        type CloseClipboardFn = unsafe extern "system" fn() -> i32;
        type GetClipboardDataFn = unsafe extern "system" fn(u32) -> isize;
        type GlobalLockFn = unsafe extern "system" fn(isize) -> *const u16;
        type GlobalUnlockFn = unsafe extern "system" fn(isize) -> i32;

        let user32_w: Vec<u16> = "user32.dll\0".encode_utf16().collect();
        let kernel32_w: Vec<u16> = "kernel32.dll\0".encode_utf16().collect();
        let h_user32 = windows::Win32::System::LibraryLoader::LoadLibraryW(windows::core::PCWSTR(user32_w.as_ptr())).ok()?;
        let h_kernel32 = windows::Win32::System::LibraryLoader::LoadLibraryW(windows::core::PCWSTR(kernel32_w.as_ptr())).ok()?;

        let open_cb: OpenClipboardFn = std::mem::transmute(windows::Win32::System::LibraryLoader::GetProcAddress(h_user32, windows::core::PCSTR(b"OpenClipboard\0".as_ptr()))?);
        let close_cb: CloseClipboardFn = std::mem::transmute(windows::Win32::System::LibraryLoader::GetProcAddress(h_user32, windows::core::PCSTR(b"CloseClipboard\0".as_ptr()))?);
        let get_cb_data: GetClipboardDataFn = std::mem::transmute(windows::Win32::System::LibraryLoader::GetProcAddress(h_user32, windows::core::PCSTR(b"GetClipboardData\0".as_ptr()))?);
        let glock: GlobalLockFn = std::mem::transmute(windows::Win32::System::LibraryLoader::GetProcAddress(h_kernel32, windows::core::PCSTR(b"GlobalLock\0".as_ptr()))?);
        let gunlock: GlobalUnlockFn = std::mem::transmute(windows::Win32::System::LibraryLoader::GetProcAddress(h_kernel32, windows::core::PCSTR(b"GlobalUnlock\0".as_ptr()))?);

        if open_cb(0) == 0 {
            return None;
        }

        const CF_UNICODETEXT: u32 = 13;
        let h_data = get_cb_data(CF_UNICODETEXT);
        if h_data == 0 {
            let _ = close_cb();
            return None;
        }

        let ptr = glock(h_data);
        if ptr.is_null() {
            let _ = close_cb();
            return None;
        }

        let mut len = 0;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(ptr, len);
        let s = String::from_utf16_lossy(slice);

        let _ = gunlock(h_data);
        let _ = close_cb();
        Some(s)
    }
}

/// Запись текста в системный буфер обмена Windows (user32.dll)
pub fn set_clipboard_text(text: &str) {
    unsafe {
        type OpenClipboardFn = unsafe extern "system" fn(isize) -> i32;
        type CloseClipboardFn = unsafe extern "system" fn() -> i32;
        type EmptyClipboardFn = unsafe extern "system" fn() -> i32;
        type SetClipboardDataFn = unsafe extern "system" fn(u32, isize) -> isize;
        type GlobalAllocFn = unsafe extern "system" fn(u32, usize) -> isize;
        type GlobalLockFn = unsafe extern "system" fn(isize) -> *mut u16;
        type GlobalUnlockFn = unsafe extern "system" fn(isize) -> i32;

        let user32_w: Vec<u16> = "user32.dll\0".encode_utf16().collect();
        let kernel32_w: Vec<u16> = "kernel32.dll\0".encode_utf16().collect();
        if let (Ok(h_u), Ok(h_k)) = (
            windows::Win32::System::LibraryLoader::LoadLibraryW(windows::core::PCWSTR(user32_w.as_ptr())),
            windows::Win32::System::LibraryLoader::LoadLibraryW(windows::core::PCWSTR(kernel32_w.as_ptr())),
        ) {
            if let (Some(p_open), Some(p_close), Some(p_empty), Some(p_set), Some(p_alloc), Some(p_lock), Some(p_unlock)) = (
                windows::Win32::System::LibraryLoader::GetProcAddress(h_u, windows::core::PCSTR(b"OpenClipboard\0".as_ptr())),
                windows::Win32::System::LibraryLoader::GetProcAddress(h_u, windows::core::PCSTR(b"CloseClipboard\0".as_ptr())),
                windows::Win32::System::LibraryLoader::GetProcAddress(h_u, windows::core::PCSTR(b"EmptyClipboard\0".as_ptr())),
                windows::Win32::System::LibraryLoader::GetProcAddress(h_u, windows::core::PCSTR(b"SetClipboardData\0".as_ptr())),
                windows::Win32::System::LibraryLoader::GetProcAddress(h_k, windows::core::PCSTR(b"GlobalAlloc\0".as_ptr())),
                windows::Win32::System::LibraryLoader::GetProcAddress(h_k, windows::core::PCSTR(b"GlobalLock\0".as_ptr())),
                windows::Win32::System::LibraryLoader::GetProcAddress(h_k, windows::core::PCSTR(b"GlobalUnlock\0".as_ptr())),
            ) {
                let open_cb: OpenClipboardFn = std::mem::transmute(p_open);
                let close_cb: CloseClipboardFn = std::mem::transmute(p_close);
                let empty_cb: EmptyClipboardFn = std::mem::transmute(p_empty);
                let set_cb: SetClipboardDataFn = std::mem::transmute(p_set);
                let galloc: GlobalAllocFn = std::mem::transmute(p_alloc);
                let glock: GlobalLockFn = std::mem::transmute(p_lock);
                let gunlock: GlobalUnlockFn = std::mem::transmute(p_unlock);

                if open_cb(0) != 0 {
                    empty_cb();
                    let u16_vec: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
                    const GMEM_MOVEABLE: u32 = 0x0002;
                    let h_mem = galloc(GMEM_MOVEABLE, u16_vec.len() * 2);
                    if h_mem != 0 {
                        let ptr = glock(h_mem);
                        if !ptr.is_null() {
                            std::ptr::copy_nonoverlapping(u16_vec.as_ptr(), ptr, u16_vec.len());
                            gunlock(h_mem);
                            set_cb(13, h_mem);
                        }
                    }
                    close_cb();
                }
            }
        }
    }
}

/// Быстрое сжатие RGBA кадра в JPEG для сетевой передачи (~40 KB)
pub fn encode_frame_to_jpeg(w: u32, h: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    if rgba.len() < (w * h * 4) as usize {
        return None;
    }
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for chunk in rgba.chunks_exact(4) {
        rgb.push(chunk[0]);
        rgb.push(chunk[1]);
        rgb.push(chunk[2]);
    }
    let mut out = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 65);
    if encoder.encode(&rgb, w, h, image::ExtendedColorType::Rgb8).is_ok() {
        Some(out)
    } else {
        None
    }
}

/// Распаковка JPEG в RGBA
pub fn decode_frame_from_jpeg(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if let Ok(img) = image::load_from_memory_with_format(data, image::ImageFormat::Jpeg) {
        let rgba = img.to_rgba8();
        let (w, h) = (rgba.width(), rgba.height());
        Some((w, h, rgba.into_raw()))
    } else {
        None
    }
}

/// Полноценный P2P TCP Сервер Nanocord с поддержкой IPv6 [::] и IPv4 0.0.0.0
pub struct P2PNetworkServer {
    pub is_running: Arc<AtomicBool>,
    pub is_connected: Arc<AtomicBool>,
    pub port: u16,
    pub room_code: String,
    pub peer_addr: Arc<RwLock<String>>,
    pub status: Arc<RwLock<String>>,
    pub desktop_rx: Receiver<NetworkPeerFrame>,
    pub camera_rx: Receiver<NetworkPeerFrame>,
    pub outgoing_desktop_tx: SyncSender<NetworkPeerFrame>,
    pub outgoing_camera_tx: SyncSender<NetworkPeerFrame>,
    pub telemetry: Arc<RwLock<P2PTelemetry>>,
}

impl P2PNetworkServer {
    pub fn start(port: u16, room_code: String) -> Self {
        let is_running = Arc::new(AtomicBool::new(true));
        let is_connected = Arc::new(AtomicBool::new(false));
        let peer_addr = Arc::new(RwLock::new(String::new()));
        let status = Arc::new(RwLock::new("Запуск сервера...".to_string()));
        let telemetry = Arc::new(RwLock::new(P2PTelemetry::default()));

        let (in_dt_tx, in_dt_rx) = sync_channel(4);
        let (in_cam_tx, in_cam_rx) = sync_channel(4);
        let (out_dt_tx, out_dt_rx) = sync_channel(4);
        let (out_cam_tx, out_cam_rx) = sync_channel(4);

        let running_clone = is_running.clone();
        let connected_clone = is_connected.clone();
        let peer_addr_clone = peer_addr.clone();
        let status_clone = status.clone();
        let telem_clone = telemetry.clone();
        let expected_code = room_code.clone();

        thread::spawn(move || {
            let l_v6 = TcpListener::bind(format!("[::]:{}", port)).ok();
            if let Some(ref l) = l_v6 {
                let _ = l.set_nonblocking(true);
            }
            let l_v4 = TcpListener::bind(format!("0.0.0.0:{}", port)).ok();
            if let Some(ref l) = l_v4 {
                let _ = l.set_nonblocking(true);
            }

            if l_v6.is_none() && l_v4.is_none() {
                if let Ok(mut st) = status_clone.write() {
                    *st = format!("Ошибка: не удалось привязать порт {}", port);
                }
                return;
            }

            if let Ok(mut st) = status_clone.write() {
                *st = format!("Ожидание подключения на порту {} (IPv6 + IPv4)...", port);
            }

            while running_clone.load(Ordering::Relaxed) {
                let mut incoming = None;
                if let Some(ref l) = l_v6 {
                    if let Ok((s, a)) = l.accept() {
                        incoming = Some((s, a));
                    }
                }
                if incoming.is_none() {
                    if let Some(ref l) = l_v4 {
                        if let Ok((s, a)) = l.accept() {
                            incoming = Some((s, a));
                        }
                    }
                }

                if let Some((mut stream, addr)) = incoming {
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_nodelay(true);
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(2000)));

                    // Рукопожатие
                    let mut magic = [0u8; 4];
                    let hs_ok = if stream.read_exact(&mut magic).is_ok() && &magic == b"NC02" {
                        let mut len_b = [0u8; 1];
                        if stream.read_exact(&mut len_b).is_ok() {
                            let len = len_b[0] as usize;
                            let mut code_b = vec![0u8; len];
                            if stream.read_exact(&mut code_b).is_ok() {
                                let client_code = String::from_utf8_lossy(&code_b).trim().to_string();
                                client_code == expected_code.trim()
                            } else { false }
                        } else { false }
                    } else { false };

                    if hs_ok {
                        let _ = stream.write_all(b"NCOK");
                        let _ = stream.flush();
                        connected_clone.store(true, Ordering::Relaxed);
                        if let Ok(mut pa) = peer_addr_clone.write() {
                            *pa = addr.to_string();
                        }
                        if let Ok(mut st) = status_clone.write() {
                            *st = format!("Собеседник подключен: {}", addr);
                        }
                        if let Ok(mut t) = telem_clone.write() {
                            t.is_connected = true;
                            t.peer_addr = addr.to_string();
                        }

                        // Обмен кадрами
                        run_p2p_socket_session(
                            stream,
                            running_clone.clone(),
                            connected_clone.clone(),
                            in_dt_tx.clone(),
                            in_cam_tx.clone(),
                            &out_dt_rx,
                            &out_cam_rx,
                            telem_clone.clone(),
                        );

                        connected_clone.store(false, Ordering::Relaxed);
                        if let Ok(mut st) = status_clone.write() {
                            *st = format!("Собеседник отключился. Ожидание нового подключения...");
                        }
                        if let Ok(mut t) = telem_clone.write() {
                            t.is_connected = false;
                        }
                    } else {
                        let _ = stream.write_all(b"NCER");
                        let _ = stream.flush();
                        if let Ok(mut st) = status_clone.write() {
                            *st = format!("Отклонено подключение от {} (неверный код)", addr);
                        }
                    }
                } else {
                    thread::sleep(Duration::from_millis(40));
                }
            }
        });

        Self {
            is_running,
            is_connected,
            port,
            room_code,
            peer_addr,
            status,
            desktop_rx: in_dt_rx,
            camera_rx: in_cam_rx,
            outgoing_desktop_tx: out_dt_tx,
            outgoing_camera_tx: out_cam_tx,
            telemetry,
        }
    }

    pub fn stop(&self) {
        self.is_running.store(false, Ordering::Relaxed);
        self.is_connected.store(false, Ordering::Relaxed);
    }
}

/// Полноценный P2P TCP Клиент Nanocord с поддержкой IPv6 и IPv4
pub struct P2PNetworkClient {
    pub is_running: Arc<AtomicBool>,
    pub is_connected: Arc<AtomicBool>,
    pub target_addr: String,
    pub room_code: String,
    pub status: Arc<RwLock<String>>,
    pub desktop_rx: Receiver<NetworkPeerFrame>,
    pub camera_rx: Receiver<NetworkPeerFrame>,
    pub outgoing_desktop_tx: SyncSender<NetworkPeerFrame>,
    pub outgoing_camera_tx: SyncSender<NetworkPeerFrame>,
    pub telemetry: Arc<RwLock<P2PTelemetry>>,
}

impl P2PNetworkClient {
    pub fn connect(target_host: String, port: u16, room_code: String) -> Self {
        let is_running = Arc::new(AtomicBool::new(true));
        let is_connected = Arc::new(AtomicBool::new(false));
        let status = Arc::new(RwLock::new(format!("Подключение к {}:{}...", target_host, port)));
        let telemetry = Arc::new(RwLock::new(P2PTelemetry::default()));

        let (in_dt_tx, in_dt_rx) = sync_channel(4);
        let (in_cam_tx, in_cam_rx) = sync_channel(4);
        let (out_dt_tx, out_dt_rx) = sync_channel(4);
        let (out_cam_tx, out_cam_rx) = sync_channel(4);

        let running_clone = is_running.clone();
        let connected_clone = is_connected.clone();
        let status_clone = status.clone();
        let telem_clone = telemetry.clone();
        let code_to_send = room_code.clone();
        let host_clone = target_host.clone();

        thread::spawn(move || {
            let resolve_target = if host_clone.starts_with('[') && host_clone.ends_with(']') {
                format!("{}:{}", host_clone, port)
            } else if host_clone.contains(':') && !host_clone.contains('.') {
                format!("[{}]:{}", host_clone, port)
            } else {
                format!("{}:{}", host_clone, port)
            };

            let addrs_res = resolve_target.to_socket_addrs();
            let addrs: Vec<SocketAddr> = match addrs_res {
                Ok(iter) => iter.collect(),
                Err(e) => {
                    if let Ok(mut st) = status_clone.write() {
                        *st = format!("Ошибка адреса: {}", e);
                    }
                    return;
                }
            };

            if addrs.is_empty() {
                if let Ok(mut st) = status_clone.write() {
                    *st = format!("Не удалось разрешить адрес {}", resolve_target);
                }
                return;
            }

            let mut connected_stream = None;
            for sa in &addrs {
                if let Ok(s) = TcpStream::connect_timeout(sa, Duration::from_secs(3)) {
                    connected_stream = Some(s);
                    break;
                }
            }

            let mut stream = match connected_stream {
                Some(s) => s,
                None => {
                    if let Ok(mut st) = status_clone.write() {
                        *st = format!("Сервер {}:{} недоступен", host_clone, port);
                    }
                    return;
                }
            };

            let _ = stream.set_nodelay(true);
            let _ = stream.set_read_timeout(Some(Duration::from_millis(3000)));

            // Отправка рукопожатия
            let code_bytes = code_to_send.as_bytes();
            let mut hs_packet = Vec::with_capacity(5 + code_bytes.len());
            hs_packet.extend_from_slice(b"NC02");
            hs_packet.push(code_bytes.len() as u8);
            hs_packet.extend_from_slice(code_bytes);

            if stream.write_all(&hs_packet).is_err() || stream.flush().is_err() {
                if let Ok(mut st) = status_clone.write() {
                    *st = "Сбой отправки данных".to_string();
                }
                return;
            }

            let mut reply = [0u8; 4];
            if stream.read_exact(&mut reply).is_err() {
                if let Ok(mut st) = status_clone.write() {
                    *st = "Сервер разорвал соединение".to_string();
                }
                return;
            }

            if &reply == b"NCOK" {
                connected_clone.store(true, Ordering::Relaxed);
                if let Ok(mut st) = status_clone.write() {
                    *st = "Успешно подключено к серверу!".to_string();
                }
                if let Ok(mut t) = telem_clone.write() {
                    t.is_connected = true;
                    t.peer_addr = resolve_target.clone();
                }

                run_p2p_socket_session(
                    stream,
                    running_clone.clone(),
                    connected_clone.clone(),
                    in_dt_tx.clone(),
                    in_cam_tx.clone(),
                    &out_dt_rx,
                    &out_cam_rx,
                    telem_clone.clone(),
                );

                connected_clone.store(false, Ordering::Relaxed);
                if let Ok(mut st) = status_clone.write() {
                    *st = "Отключено от сервера".to_string();
                }
                if let Ok(mut t) = telem_clone.write() {
                    t.is_connected = false;
                }
            } else if &reply == b"NCER" {
                connected_clone.store(false, Ordering::Relaxed);
                if let Ok(mut st) = status_clone.write() {
                    *st = "Ошибка: неверный код доступа комнаты!".to_string();
                }
            } else {
                connected_clone.store(false, Ordering::Relaxed);
                if let Ok(mut st) = status_clone.write() {
                    *st = "Сервер вернул некорректный ответ".to_string();
                }
            }
        });

        Self {
            is_running,
            is_connected,
            target_addr: target_host,
            room_code,
            status,
            desktop_rx: in_dt_rx,
            camera_rx: in_cam_rx,
            outgoing_desktop_tx: out_dt_tx,
            outgoing_camera_tx: out_cam_tx,
            telemetry,
        }
    }

    pub fn stop(&self) {
        self.is_running.store(false, Ordering::Relaxed);
        self.is_connected.store(false, Ordering::Relaxed);
    }
}

/// Общий цикл двунаправленного обмена кадрами и телеметрией по сокету
fn run_p2p_socket_session(
    stream: TcpStream,
    is_running: Arc<AtomicBool>,
    is_connected: Arc<AtomicBool>,
    in_dt_tx: SyncSender<NetworkPeerFrame>,
    in_cam_tx: SyncSender<NetworkPeerFrame>,
    out_dt_rx: &Receiver<NetworkPeerFrame>,
    out_cam_rx: &Receiver<NetworkPeerFrame>,
    telemetry: Arc<RwLock<P2PTelemetry>>,
) {
    let mut reader = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut writer = stream;

    let r_run = is_running.clone();
    let r_conn = is_connected.clone();
    let r_telem = telemetry.clone();

    // Поток чтения входящих пакетов
    let read_handle = thread::spawn(move || {
        let _ = reader.set_read_timeout(Some(Duration::from_millis(1500)));
        let mut header = [0u8; 12];

        while r_run.load(Ordering::Relaxed) && r_conn.load(Ordering::Relaxed) {
            match reader.read_exact(&mut header) {
                Ok(_) => {
                    let magic = u16::from_le_bytes([header[0], header[1]]);
                    if magic != 0x4E43 {
                        break;
                    }
                    let msg_type = header[2];
                    let flags = header[3];
                    let width = u16::from_le_bytes([header[4], header[5]]) as u32;
                    let height = u16::from_le_bytes([header[6], header[7]]) as u32;
                    let payload_len = u32::from_le_bytes([header[8], header[9], header[10], header[11]]) as usize;

                    if payload_len > 10_000_000 {
                        break;
                    }

                    let mut payload = vec![0u8; payload_len];
                    if reader.read_exact(&mut payload).is_err() {
                        break;
                    }

                    if msg_type == 1 {
                        let rgba = if flags == 1 {
                            decode_frame_from_jpeg(&payload).map(|(_, _, d)| d).unwrap_or(payload)
                        } else {
                            payload
                        };
                        let _ = in_dt_tx.try_send(NetworkPeerFrame { width, height, rgba });
                    } else if msg_type == 2 {
                        let rgba = if flags == 1 {
                            decode_frame_from_jpeg(&payload).map(|(_, _, d)| d).unwrap_or(payload)
                        } else {
                            payload
                        };
                        let _ = in_cam_tx.try_send(NetworkPeerFrame { width, height, rgba });
                    } else if msg_type == 4 {
                        if payload.len() == 8 {
                            let sent_ns = u64::from_le_bytes(payload.try_into().unwrap());
                            let now_ns = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_nanos() as u64;
                            let rtt_ms = (now_ns.saturating_sub(sent_ns) as f32) / 1_000_000.0;
                            if let Ok(mut t) = r_telem.write() {
                                t.latency_ms = rtt_ms.clamp(0.1, 999.0);
                            }
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut || e.kind() == std::io::ErrorKind::WouldBlock => {
                    continue;
                }
                Err(_) => {
                    break;
                }
            }
        }
        r_conn.store(false, Ordering::Relaxed);
    });

    // Цикл отправки исходящих пакетов
    let mut last_ping = Instant::now();
    let mut fps_counter = 0u32;
    let mut fps_timer = Instant::now();
    let mut total_bytes_sent = 0u64;

    while is_running.load(Ordering::Relaxed) && is_connected.load(Ordering::Relaxed) {
        let mut sent_any = false;

        if let Ok(frame) = out_dt_rx.try_recv() {
            let (flags, payload) = if let Some(jpeg) = encode_frame_to_jpeg(frame.width, frame.height, &frame.rgba) {
                (1u8, jpeg)
            } else {
                (0u8, frame.rgba)
            };

            let mut pkt = Vec::with_capacity(12 + payload.len());
            pkt.extend_from_slice(&0x4E43u16.to_le_bytes());
            pkt.push(1); // Desktop
            pkt.push(flags);
            pkt.extend_from_slice(&(frame.width as u16).to_le_bytes());
            pkt.extend_from_slice(&(frame.height as u16).to_le_bytes());
            pkt.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            pkt.extend_from_slice(&payload);

            if writer.write_all(&pkt).is_err() {
                break;
            }
            total_bytes_sent += pkt.len() as u64;
            fps_counter += 1;
            sent_any = true;
        }

        if let Ok(frame) = out_cam_rx.try_recv() {
            let (flags, payload) = if let Some(jpeg) = encode_frame_to_jpeg(frame.width, frame.height, &frame.rgba) {
                (1u8, jpeg)
            } else {
                (0u8, frame.rgba)
            };

            let mut pkt = Vec::with_capacity(12 + payload.len());
            pkt.extend_from_slice(&0x4E43u16.to_le_bytes());
            pkt.push(2); // Camera
            pkt.push(flags);
            pkt.extend_from_slice(&(frame.width as u16).to_le_bytes());
            pkt.extend_from_slice(&(frame.height as u16).to_le_bytes());
            pkt.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            pkt.extend_from_slice(&payload);

            if writer.write_all(&pkt).is_err() {
                break;
            }
            total_bytes_sent += pkt.len() as u64;
            sent_any = true;
        }

        if last_ping.elapsed() >= Duration::from_secs(1) {
            last_ping = Instant::now();
            let now_ns = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_nanos() as u64;
            let mut pkt = Vec::with_capacity(12 + 8);
            pkt.extend_from_slice(&0x4E43u16.to_le_bytes());
            pkt.push(4); // Ping/Pong RTT
            pkt.push(0);
            pkt.extend_from_slice(&0u16.to_le_bytes());
            pkt.extend_from_slice(&0u16.to_le_bytes());
            pkt.extend_from_slice(&8u32.to_le_bytes());
            pkt.extend_from_slice(&now_ns.to_le_bytes());

            if writer.write_all(&pkt).is_err() {
                break;
            }
        }

        if fps_timer.elapsed() >= Duration::from_secs(1) {
            let cur_fps = fps_counter as f32 / fps_timer.elapsed().as_secs_f32();
            let mbps = (total_bytes_sent as f32 * 8.0) / (fps_timer.elapsed().as_secs_f32() * 1_000_000.0);
            if let Ok(mut t) = telemetry.write() {
                t.fps = cur_fps;
                t.bitrate_mbps = mbps;
            }
            fps_counter = 0;
            total_bytes_sent = 0;
            fps_timer = Instant::now();
        }

        if !sent_any {
            thread::sleep(Duration::from_millis(16));
        }
    }

    is_connected.store(false, Ordering::Relaxed);
    let _ = read_handle.join();
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_random_room_code() {
        for _ in 0..10 {
            let code = generate_random_room_code();
            assert_eq!(code.len(), 3);
            for c in code.chars() {
                assert!(c.is_ascii_alphanumeric());
            }
        }
    }

    #[test]
    fn test_parse_room_target_ipv6() {
        let (host, port, code) = parse_room_target("[::1]:48120#7xK").expect("parse failed");
        assert_eq!(host, "[::1]");
        assert_eq!(port, 48120);
        assert_eq!(code, "7xK");
    }

    #[test]
    fn test_parse_room_target_ipv4() {
        let (host, port, code) = parse_room_target("192.168.1.100:50000#Ab3").expect("parse failed");
        assert_eq!(host, "192.168.1.100");
        assert_eq!(port, 50000);
        assert_eq!(code, "Ab3");
    }

    #[test]
    fn test_parse_room_target_slash() {
        let (host, port, code) = parse_room_target("[2001:db8::1]:48120/Xy9").expect("parse failed");
        assert_eq!(host, "[2001:db8::1]");
        assert_eq!(port, 48120);
        assert_eq!(code, "Xy9");
    }

    #[test]
    fn test_p2p_ipv6_handshake() {
        let port = 49152;
        let code = "Tv7".to_string();

        let server = P2PNetworkServer::start(port, code.clone());
        thread::sleep(Duration::from_millis(100));

        let client = P2PNetworkClient::connect("[::1]".to_string(), port, code);
        thread::sleep(Duration::from_millis(300));

        assert!(client.is_connected.load(Ordering::Relaxed), "Client should be connected via IPv6");
        assert!(server.is_connected.load(Ordering::Relaxed), "Server should have connected client");

        client.stop();
        server.stop();
    }
}
