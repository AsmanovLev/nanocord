#![allow(dead_code, unused_variables)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const P2P_DEFAULT_PORT: u16 = 48120;
pub const P2P_MAGIC: [u8; 4] = *b"NC01";
pub const HEADER_SIZE: usize = 93;

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

/// Серверный приёмник входящего P2P-потока в основном приложении nanocord.exe
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

        let (dt_tx, dt_rx) = sync_channel(2);
        let (cam_tx, cam_rx) = sync_channel(2);

        let running_clone = is_running.clone();
        let connected_clone = is_connected.clone();
        let telem_clone = telemetry.clone();

        thread::spawn(move || {
            let addr = format!("127.0.0.1:{}", P2P_DEFAULT_PORT);
            let listener = match TcpListener::bind(&addr) {
                Ok(l) => {
                    let _ = l.set_nonblocking(true);
                    l
                }
                Err(e) => {
                    eprintln!("[P2P Server] Failed to bind to {}: {}", addr, e);
                    return;
                }
            };

            println!("[P2P Server] Listening for mock client on {}", addr);

            let mut header_buf = [0u8; HEADER_SIZE];
            let mut payload_buf = vec![0u8; 1920 * 1080 * 4];

            while running_clone.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, peer_addr)) => {
                        println!("[P2P Server] Mock client connected from {}", peer_addr);
                        let _ = stream.set_nodelay(true);
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                        connected_clone.store(true, Ordering::Relaxed);

                        {
                            let mut t = telem_clone.write().unwrap();
                            t.is_connected = true;
                            t.peer_addr = peer_addr.to_string();
                        }

                        let mut frames_in_second = 0u32;
                        let mut bytes_in_second = 0u64;
                        let mut sec_timer = Instant::now();
                        let mut total_frames = 0u64;
                        let mut total_bytes = 0u64;

                        while running_clone.load(Ordering::Relaxed) {
                            if stream.read_exact(&mut header_buf).is_err() {
                                break;
                            }

                            if &header_buf[0..4] != &P2P_MAGIC {
                                break;
                            }

                            let pkt_type = header_buf[4];
                            let width = u16::from_le_bytes([header_buf[5], header_buf[6]]) as u32;
                            let height = u16::from_le_bytes([header_buf[7], header_buf[8]]) as u32;
                            let seq = u64::from_le_bytes(header_buf[9..17].try_into().unwrap());
                            let timestamp_ms = u64::from_le_bytes(header_buf[17..25].try_into().unwrap());
                            let payload_len = u32::from_le_bytes(header_buf[25..29].try_into().unwrap()) as usize;

                            let track_bytes = &header_buf[29..93];
                            let track_end = track_bytes.iter().position(|&b| b == 0).unwrap_or(track_bytes.len());
                            let track_name = String::from_utf8_lossy(&track_bytes[..track_end]).to_string();

                            if payload_len > payload_buf.len() {
                                payload_buf.resize(payload_len, 0);
                            }

                            if stream.read_exact(&mut payload_buf[..payload_len]).is_err() {
                                break;
                            }

                            frames_in_second += 1;
                            bytes_in_second += (HEADER_SIZE + payload_len) as u64;
                            total_frames += 1;
                            total_bytes += (HEADER_SIZE + payload_len) as u64;

                            let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
                            let latency = if now_ms >= timestamp_ms { (now_ms - timestamp_ms) as f32 } else { 0.5 };

                            if sec_timer.elapsed() >= Duration::from_secs(1) {
                                let fps = frames_in_second as f32 / sec_timer.elapsed().as_secs_f32();
                                let mbps = (bytes_in_second as f32 * 8.0) / (1_000_000.0 * sec_timer.elapsed().as_secs_f32());
                                {
                                    let mut t = telem_clone.write().unwrap();
                                    t.fps = fps;
                                    t.bitrate_mbps = mbps;
                                    t.latency_ms = latency.max(0.4);
                                    t.current_track = track_name.clone();
                                    t.total_frames = total_frames;
                                    t.total_bytes = total_bytes;
                                }
                                frames_in_second = 0;
                                bytes_in_second = 0;
                                sec_timer = Instant::now();
                            }

                            let frame = NetworkPeerFrame {
                                width,
                                height,
                                rgba: payload_buf[..payload_len].to_vec(),
                            };

                            if pkt_type == 1 {
                                let _ = dt_tx.try_send(frame);
                            } else if pkt_type == 2 {
                                let _ = cam_tx.try_send(frame);
                            }
                        }

                        println!("[P2P Server] Mock client disconnected");
                        connected_clone.store(false, Ordering::Relaxed);
                        {
                            let mut t = telem_clone.write().unwrap();
                            t.is_connected = false;
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(15));
                    }
                    Err(e) => {
                        thread::sleep(Duration::from_millis(50));
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

/// Клиентский передатчик P2P потока для отдельного nanocord_mock.exe
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
            let addr = format!("127.0.0.1:{}", P2P_DEFAULT_PORT);
            let mut dt_seq = 0u64;
            let mut cam_seq = 0u64;
            let start_time = Instant::now();

            let dt_w = 1280u32;
            let dt_h = 720u32;
            let cam_w = 640u32;
            let cam_h = 360u32;

            while running_clone.load(Ordering::Relaxed) {
                connected_clone.store(false, Ordering::Relaxed);
                println!("[Mock Client] Connecting to Nanocord at {}...", addr);

                let mut stream = loop {
                    if !running_clone.load(Ordering::Relaxed) {
                        return;
                    }
                    match TcpStream::connect(&addr) {
                        Ok(s) => {
                            let _ = s.set_nodelay(true);
                            println!("[Mock Client] Connected to Nanocord!");
                            connected_clone.store(true, Ordering::Relaxed);
                            break s;
                        }
                        Err(_) => {
                            thread::sleep(Duration::from_millis(400));
                        }
                    }
                };

                let mut frames_in_sec = 0u32;
                let mut bytes_in_sec = 0u64;
                let mut sec_timer = Instant::now();
                let mut total_frames = 0u64;
                let mut total_bytes = 0u64;

                while running_clone.load(Ordering::Relaxed) {
                    let cur_fps = fps_clone.load(Ordering::Relaxed).clamp(15, 144);
                    let frame_interval = Duration::from_secs_f32(1.0 / cur_fps as f32);
                    let loop_start = Instant::now();

                    dt_seq += 1;
                    let elapsed = start_time.elapsed().as_secs_f32();
                    let cur_track = bgm_clone.lock().unwrap().current_track_name();

                    // 1. Генерация и отправка экрана (1280x720)
                    let dt_rgba = generate_mock_desktop_frame(dt_w, dt_h, dt_seq, elapsed, &cur_track);
                    let _ = prev_dt_tx.try_send(NetworkPeerFrame {
                        width: dt_w,
                        height: dt_h,
                        rgba: dt_rgba.clone(),
                    });

                    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
                    let mut header = [0u8; HEADER_SIZE];
                    header[0..4].copy_from_slice(&P2P_MAGIC);
                    header[4] = 1; // 1 = Desktop
                    header[5..7].copy_from_slice(&(dt_w as u16).to_le_bytes());
                    header[7..9].copy_from_slice(&(dt_h as u16).to_le_bytes());
                    header[9..17].copy_from_slice(&dt_seq.to_le_bytes());
                    header[17..25].copy_from_slice(&now_ms.to_le_bytes());
                    header[25..29].copy_from_slice(&(dt_rgba.len() as u32).to_le_bytes());

                    let track_bytes = cur_track.as_bytes();
                    let copy_len = track_bytes.len().min(63);
                    header[29..29 + copy_len].copy_from_slice(&track_bytes[..copy_len]);

                    if stream.write_all(&header).is_err() || stream.write_all(&dt_rgba).is_err() {
                        println!("[Mock Client] Connection to Nanocord lost");
                        break;
                    }

                    frames_in_sec += 1;
                    bytes_in_sec += (HEADER_SIZE + dt_rgba.len()) as u64;
                    total_frames += 1;
                    total_bytes += (HEADER_SIZE + dt_rgba.len()) as u64;

                    // 2. Генерация и отправка камеры (640x360 ~ 30 FPS)
                    if dt_seq % 2 == 0 {
                        cam_seq += 1;
                        let cam_rgba = generate_mock_camera_frame(cam_w, cam_h, cam_seq, elapsed);
                        let _ = prev_cam_tx.try_send(NetworkPeerFrame {
                            width: cam_w,
                            height: cam_h,
                            rgba: cam_rgba.clone(),
                        });

                        let mut cam_header = [0u8; HEADER_SIZE];
                        cam_header[0..4].copy_from_slice(&P2P_MAGIC);
                        cam_header[4] = 2; // 2 = Camera
                        cam_header[5..7].copy_from_slice(&(cam_w as u16).to_le_bytes());
                        cam_header[7..9].copy_from_slice(&(cam_h as u16).to_le_bytes());
                        cam_header[9..17].copy_from_slice(&cam_seq.to_le_bytes());
                        cam_header[17..25].copy_from_slice(&now_ms.to_le_bytes());
                        cam_header[25..29].copy_from_slice(&(cam_rgba.len() as u32).to_le_bytes());
                        cam_header[29..29 + copy_len].copy_from_slice(&track_bytes[..copy_len]);

                        if stream.write_all(&cam_header).is_err() || stream.write_all(&cam_rgba).is_err() {
                            println!("[Mock Client] Connection to Nanocord lost");
                            break;
                        }

                        frames_in_sec += 1;
                        bytes_in_sec += (HEADER_SIZE + cam_rgba.len()) as u64;
                        total_frames += 1;
                        total_bytes += (HEADER_SIZE + cam_rgba.len()) as u64;
                    }

                    if sec_timer.elapsed() >= Duration::from_secs(1) {
                        let fps = frames_in_sec as f32 / sec_timer.elapsed().as_secs_f32();
                        let mbps = (bytes_in_sec as f32 * 8.0) / (1_000_000.0 * sec_timer.elapsed().as_secs_f32());
                        {
                            let mut t = telem_clone.write().unwrap();
                            t.is_connected = true;
                            t.fps = fps;
                            t.bitrate_mbps = mbps;
                            t.current_track = cur_track;
                            t.total_frames = total_frames;
                            t.total_bytes = total_bytes;
                        }
                        frames_in_sec = 0;
                        bytes_in_sec = 0;
                        sec_timer = Instant::now();
                    }

                    let elapsed_frame = loop_start.elapsed();
                    if frame_interval > elapsed_frame {
                        thread::sleep(frame_interval - elapsed_frame);
                    }
                }
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
    pub tracks: Vec<(String, String)>, // (display_name, full_path)
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

/// Генерация динамического 1280x720 видеокадра удаленного экрана с технологичным тестовым паттерном
pub fn generate_mock_desktop_frame(w: u32, h: u32, seq: u64, elapsed: f32, track_name: &str) -> Vec<u8> {
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let w_usize = w as usize;

    let scanline = ((elapsed * 240.0) as usize) % (h as usize);

    for y in 0..h as usize {
        let y_row = y * w_usize * 4;
        let is_scan = (y as isize - scanline as isize).abs() < 3;

        for x in 0..w_usize {
            let o = y_row + x * 4;

            // Кибер-сетка
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
pub fn generate_mock_camera_frame(w: u32, h: u32, seq: u64, elapsed: f32) -> Vec<u8> {
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
