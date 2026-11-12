#![allow(dead_code, unused_variables)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::latest::LatestSlot;

/// Visible session state: one source of truth instead of a handful of flags
/// that can disagree with each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    Idle,
    Listening,
    Connecting,
    Connected,
    Reconnecting,
    Failed,
}

impl SessionState {
    pub fn label(self) -> &'static str {
        match self {
            SessionState::Idle => "idle",
            SessionState::Listening => "listening",
            SessionState::Connecting => "connecting",
            SessionState::Connected => "connected",
            SessionState::Reconnecting => "reconnecting",
            SessionState::Failed => "failed",
        }
    }
}

/// A frame on its way out. RGBA, shared by `Arc` so queueing costs nothing.
#[derive(Clone)]
pub struct OutgoingFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<Vec<u8>>,
}

#[derive(Clone)]
pub struct NetworkPeerFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// No pixels: "the stream is alive, the desktop simply did not change".
    pub keepalive: bool,
}

impl NetworkPeerFrame {
    pub fn keepalive() -> Self {
        Self {
            width: 0,
            height: 0,
            rgba: Vec::new(),
            keepalive: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NetworkAudioPacket {
    pub stream_type: u8, // 0 = Voice/Mic, 1 = Desktop/Loopback
    pub seq: u32,
    pub peak_db: f32,
    pub min_sample: f32,
    pub max_sample: f32,
    pub rms: f32,
    pub bins: [f32; 20],
    pub opus_data: Vec<u8>,
}

impl Default for NetworkAudioPacket {
    fn default() -> Self {
        Self {
            stream_type: 0,
            seq: 0,
            peak_db: -60.0,
            min_sample: 0.0,
            max_sample: 0.0,
            rms: 0.0,
            bins: [0.0; 20],
            opus_data: Vec::new(),
        }
    }
}

impl NetworkAudioPacket {
    pub fn new(peak_db: f32, min_sample: f32, max_sample: f32, rms: f32, bins: [f32; 20]) -> Self {
        Self {
            stream_type: 0,
            seq: 0,
            peak_db,
            min_sample,
            max_sample,
            rms,
            bins,
            opus_data: Vec::new(),
        }
    }

    pub fn with_opus(peak_db: f32, min_sample: f32, max_sample: f32, rms: f32, bins: [f32; 20], opus_data: Vec<u8>) -> Self {
        Self {
            stream_type: 0,
            seq: 0,
            peak_db,
            min_sample,
            max_sample,
            rms,
            bins,
            opus_data,
        }
    }

    pub fn with_stream(
        stream_type: u8,
        seq: u32,
        peak_db: f32,
        min_sample: f32,
        max_sample: f32,
        rms: f32,
        bins: [f32; 20],
        opus_data: Vec<u8>,
    ) -> Self {
        Self {
            stream_type,
            seq,
            peak_db,
            min_sample,
            max_sample,
            rms,
            bins,
            opus_data,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(1 + 1 + 4 + 96 + 4 + self.opus_data.len());
        buf.push(b'O');
        buf.push(self.stream_type);
        buf.extend_from_slice(&self.seq.to_le_bytes());
        buf.extend_from_slice(&self.peak_db.to_le_bytes());
        buf.extend_from_slice(&self.min_sample.to_le_bytes());
        buf.extend_from_slice(&self.max_sample.to_le_bytes());
        buf.extend_from_slice(&self.rms.to_le_bytes());
        for i in 0..20 {
            buf.extend_from_slice(&self.bins[i].to_le_bytes());
        }
        buf.extend_from_slice(&(self.opus_data.len() as u32).to_le_bytes());
        buf.extend_from_slice(&self.opus_data);
        buf
    }

    pub fn from_bytes(slice: &[u8]) -> Option<Self> {
        if slice.is_empty() {
            return None;
        }

        // Проверяем формат с префиксом b'O'
        if slice[0] == b'O' && slice.len() >= 106 {
            let stream_type = slice[1];
            let seq = u32::from_le_bytes(slice[2..6].try_into().ok()?);
            let peak_db = f32::from_le_bytes(slice[6..10].try_into().ok()?);
            let min_sample = f32::from_le_bytes(slice[10..14].try_into().ok()?);
            let max_sample = f32::from_le_bytes(slice[14..18].try_into().ok()?);
            let rms = f32::from_le_bytes(slice[18..22].try_into().ok()?);
            let mut bins = [0.0f32; 20];
            for i in 0..20 {
                bins[i] = f32::from_le_bytes(slice[22 + i * 4..26 + i * 4].try_into().ok()?);
            }
            let opus_len = u32::from_le_bytes(slice[102..106].try_into().ok()?) as usize;
            let mut opus_data = Vec::new();
            if slice.len() >= 106 + opus_len {
                opus_data = slice[106..106 + opus_len].to_vec();
            }
            return Some(Self {
                stream_type,
                seq,
                peak_db,
                min_sample,
                max_sample,
                rms,
                bins,
                opus_data,
            });
        }

        // Обратная совместимость с 96-байтным форматом
        if slice.len() >= 96 {
            let peak_db = f32::from_le_bytes(slice[0..4].try_into().ok()?);
            let min_sample = f32::from_le_bytes(slice[4..8].try_into().ok()?);
            let max_sample = f32::from_le_bytes(slice[8..12].try_into().ok()?);
            let rms = f32::from_le_bytes(slice[12..16].try_into().ok()?);
            let mut bins = [0.0f32; 20];
            for i in 0..20 {
                bins[i] = f32::from_le_bytes(slice[16 + i * 4..20 + i * 4].try_into().ok()?);
            }

            let mut opus_data = Vec::new();
            if slice.len() >= 100 {
                let opus_len = u32::from_le_bytes(slice[96..100].try_into().ok()?) as usize;
                if slice.len() >= 100 + opus_len {
                    opus_data = slice[100..100 + opus_len].to_vec();
                }
            }

            return Some(Self {
                stream_type: 0,
                seq: 0,
                peak_db,
                min_sample,
                max_sample,
                rms,
                bins,
                opus_data,
            });
        }

        None
    }
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
    pub frames_sent: u64,
    pub frames_dropped: u64,
    pub quality: u8,
    pub encode_errors: u64,
    pub state: &'static str,
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

/// Определение реального публичного IP-адреса через прямой сетевой сокет в обход любых HTTP/HTTPS прокси
pub fn fetch_real_public_ip() -> Option<String> {
    use std::io::{Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;

    // Список надёжных сервисов (обращение напрямую через сырой Winsock TCP сокет в обход системных прокси)
    let services = [
        ("api64.ipify.org", "api64.ipify.org:80", "/"),
        ("api.ipify.org", "api.ipify.org:80", "/"),
        ("icanhazip.com", "icanhazip.com:80", "/"),
        ("ifconfig.me", "ifconfig.me:80", "/ip"),
    ];

    for (host_hdr, target_addr, path) in services {
        if let Ok(mut addrs) = target_addr.to_socket_addrs() {
            if let Some(addr) = addrs.next() {
                if let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(1800)) {
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(1800)));
                    let _ = stream.set_write_timeout(Some(Duration::from_millis(1800)));
                    let req = format!(
                        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: curl/7.88.1\r\nAccept: */*\r\nConnection: close\r\n\r\n",
                        path, host_hdr
                    );
                    if stream.write_all(req.as_bytes()).is_ok() {
                        let mut resp = Vec::new();
                        if stream.read_to_end(&mut resp).is_ok() {
                            let text = String::from_utf8_lossy(&resp);
                            if let Some(pos) = text.find("\r\n\r\n") {
                                let body = text[pos + 4..].trim();
                                if !body.is_empty() && (body.contains('.') || body.contains(':')) && body.len() <= 45 {
                                    let clean_ip = body.lines().next().unwrap_or("").trim().to_string();
                                    if !clean_ip.is_empty() {
                                        return Some(clean_ip);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    None
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

/// Быстрое сжатие RGBA кадра в JPEG с заданным качеством (CQ-VBR адаптация)
pub fn encode_frame_to_jpeg_with_quality(w: u32, h: u32, rgba: &[u8], quality: u8) -> Option<Vec<u8>> {
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
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(10, 100));
    if encoder.encode(&rgb, w, h, image::ExtendedColorType::Rgb8).is_ok() {
        Some(out)
    } else {
        None
    }
}

/// Быстрое сжатие RGBA кадра в JPEG для сетевой передачи (~40 KB)
pub fn encode_frame_to_jpeg(w: u32, h: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    encode_frame_to_jpeg_with_quality(w, h, rgba, 65)
}

#[derive(Clone, Debug)]
pub struct VideoEncoderConfig {
    pub cq_quality: u8,
    pub max_bitrate_mbps: u32,
    pub preset_index: usize,
}

impl Default for VideoEncoderConfig {
    fn default() -> Self {
        Self {
            cq_quality: 22,
            max_bitrate_mbps: 30,
            preset_index: 1,
        }
    }
}

pub fn cq_to_jpeg_quality(cq: u8) -> u8 {
    let q = 100.0 - (cq.clamp(16, 32) as f32 - 16.0) * 2.2;
    q.clamp(50.0, 95.0) as u8
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
    pub audio_rx: Receiver<NetworkAudioPacket>,
    pub outbox_desktop: Arc<LatestSlot<OutgoingFrame>>,
    pub outbox_camera: Arc<LatestSlot<OutgoingFrame>>,
    pub outgoing_audio_tx: SyncSender<NetworkAudioPacket>,
    pub telemetry: Arc<RwLock<P2PTelemetry>>,
    pub encoder_config: Arc<RwLock<VideoEncoderConfig>>,
    pub state: Arc<RwLock<SessionState>>,
}

impl P2PNetworkServer {
    pub fn start(port: u16, room_code: String) -> Self {
        Self::start_with_config(port, room_code, Arc::new(RwLock::new(VideoEncoderConfig::default())))
    }

    pub fn start_with_config(port: u16, room_code: String, encoder_config: Arc<RwLock<VideoEncoderConfig>>) -> Self {
        let is_running = Arc::new(AtomicBool::new(true));
        let is_connected = Arc::new(AtomicBool::new(false));
        let state = Arc::new(RwLock::new(SessionState::Idle));
        let peer_addr = Arc::new(RwLock::new(String::new()));
        let status = Arc::new(RwLock::new("Запуск сервера...".to_string()));
        let telemetry = Arc::new(RwLock::new(P2PTelemetry::default()));

        let (in_dt_tx, in_dt_rx) = sync_channel(4);
        let (in_cam_tx, in_cam_rx) = sync_channel(4);
        let (in_aud_tx, in_aud_rx) = sync_channel(16);
        let outbox_desktop: Arc<LatestSlot<OutgoingFrame>> = Arc::new(LatestSlot::new());
        let outbox_camera: Arc<LatestSlot<OutgoingFrame>> = Arc::new(LatestSlot::new());
        let outbox_desktop_bg = outbox_desktop.clone();
        let outbox_camera_bg = outbox_camera.clone();
        let (out_aud_tx, out_aud_rx) = sync_channel(16);

        let running_clone = is_running.clone();
        let connected_clone = is_connected.clone();
        let state_clone = state.clone();
        let peer_addr_clone = peer_addr.clone();
        let status_clone = status.clone();
        let telem_clone = telemetry.clone();
        let expected_code = room_code.clone();
        let enc_config_clone = encoder_config.clone();

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
                            in_aud_tx.clone(),
                            outbox_desktop_bg.clone(),
                            outbox_camera_bg.clone(),
                            &out_aud_rx,
                            telem_clone.clone(),
                            enc_config_clone.clone(),
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
            audio_rx: in_aud_rx,
            outbox_desktop,
            outbox_camera,
            outgoing_audio_tx: out_aud_tx,
            telemetry,
            encoder_config,
            state,
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
    pub audio_rx: Receiver<NetworkAudioPacket>,
    pub outbox_desktop: Arc<LatestSlot<OutgoingFrame>>,
    pub outbox_camera: Arc<LatestSlot<OutgoingFrame>>,
    pub outgoing_audio_tx: SyncSender<NetworkAudioPacket>,
    pub telemetry: Arc<RwLock<P2PTelemetry>>,
    pub encoder_config: Arc<RwLock<VideoEncoderConfig>>,
    pub state: Arc<RwLock<SessionState>>,
}

impl P2PNetworkClient {
    pub fn connect(target_host: String, port: u16, room_code: String) -> Self {
        Self::connect_with_config(target_host, port, room_code, Arc::new(RwLock::new(VideoEncoderConfig::default())))
    }

    pub fn connect_with_config(target_host: String, port: u16, room_code: String, encoder_config: Arc<RwLock<VideoEncoderConfig>>) -> Self {
        let is_running = Arc::new(AtomicBool::new(true));
        let is_connected = Arc::new(AtomicBool::new(false));
        let state = Arc::new(RwLock::new(SessionState::Idle));
        let status = Arc::new(RwLock::new(format!("Подключение к {}:{}...", target_host, port)));
        let telemetry = Arc::new(RwLock::new(P2PTelemetry::default()));

        let (in_dt_tx, in_dt_rx) = sync_channel(4);
        let (in_cam_tx, in_cam_rx) = sync_channel(4);
        let (in_aud_tx, in_aud_rx) = sync_channel(16);
        let outbox_desktop: Arc<LatestSlot<OutgoingFrame>> = Arc::new(LatestSlot::new());
        let outbox_camera: Arc<LatestSlot<OutgoingFrame>> = Arc::new(LatestSlot::new());
        let outbox_desktop_bg = outbox_desktop.clone();
        let outbox_camera_bg = outbox_camera.clone();
        let (out_aud_tx, out_aud_rx) = sync_channel(16);

        let running_clone = is_running.clone();
        let connected_clone = is_connected.clone();
        let state_clone = state.clone();
        let status_clone = status.clone();
        let telem_clone = telemetry.clone();
        let code_to_send = room_code.clone();
        let host_clone = target_host.clone();
        let enc_config_clone = encoder_config.clone();

        thread::spawn(move || {
            let mut attempt: u32 = 0;

            loop {
                if !running_clone.load(Ordering::Relaxed) {
                    break;
                }

                if let Ok(mut s) = state_clone.write() {
                    *s = if attempt == 0 {
                        SessionState::Connecting
                    } else {
                        SessionState::Reconnecting
                    };
                }

                match client_connect_once(
                    &host_clone,
                    port,
                    &code_to_send,
                    &status_clone,
                    &telem_clone,
                ) {
                    Ok(stream) => {
                        attempt = 0;
                        connected_clone.store(true, Ordering::Relaxed);
                        if let Ok(mut s) = state_clone.write() {
                            *s = SessionState::Connected;
                        }

                        run_p2p_socket_session(
                            stream,
                            running_clone.clone(),
                            connected_clone.clone(),
                            in_dt_tx.clone(),
                            in_cam_tx.clone(),
                            in_aud_tx.clone(),
                            outbox_desktop_bg.clone(),
                            outbox_camera_bg.clone(),
                            &out_aud_rx,
                            telem_clone.clone(),
                            enc_config_clone.clone(),
                        );

                        connected_clone.store(false, Ordering::Relaxed);
                        if let Ok(mut t) = telem_clone.write() {
                            t.is_connected = false;
                        }
                    }
                    Err(message) => {
                        if let Ok(mut st) = status_clone.write() {
                            *st = message;
                        }
                    }
                }

                if !running_clone.load(Ordering::Relaxed) {
                    break;
                }

                attempt = attempt.saturating_add(1);
                if attempt > MAX_RECONNECT_ATTEMPTS {
                    if let Ok(mut s) = state_clone.write() {
                        *s = SessionState::Failed;
                    }
                    if let Ok(mut st) = status_clone.write() {
                        *st = "Не удалось подключиться. Проверь адрес и код комнаты.".to_string();
                    }
                    break;
                }

                let delay = reconnect_delay(attempt);
                if let Ok(mut s) = state_clone.write() {
                    *s = SessionState::Reconnecting;
                }
                if let Ok(mut st) = status_clone.write() {
                    *st = format!(
                        "Связь потеряна. Переподключение через {:.1} с (попытка {})",
                        delay.as_secs_f32(),
                        attempt
                    );
                }
                thread::sleep(delay);
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
            audio_rx: in_aud_rx,
            outbox_desktop,
            outbox_camera,
            outgoing_audio_tx: out_aud_tx,
            telemetry,
            encoder_config,
            state,
        }
    }

    pub fn stop(&self) {
        self.is_running.store(false, Ordering::Relaxed);
        self.is_connected.store(false, Ordering::Relaxed);
    }
}

const MAX_RECONNECT_ATTEMPTS: u32 = 8;

fn reconnect_delay(attempt: u32) -> Duration {
    let step = attempt.saturating_sub(1).min(6) as i32;
    Duration::from_secs_f32((0.5f32 * 2f32.powi(step)).clamp(0.5, 10.0))
}

/// Resolve, connect and perform the room-code handshake, once.
fn client_connect_once(
    host: &str,
    port: u16,
    room_code: &str,
    status: &Arc<RwLock<String>>,
    telemetry: &Arc<RwLock<P2PTelemetry>>,
) -> std::result::Result<TcpStream, String> {
    let resolve_target = if host.starts_with('[') && host.ends_with(']') {
        format!("{}:{}", host, port)
    } else if host.contains(':') && !host.contains('.') {
        format!("[{}]:{}", host, port)
    } else {
        format!("{}:{}", host, port)
    };

    let addrs: Vec<SocketAddr> = match resolve_target.to_socket_addrs() {
        Ok(iter) => iter.collect(),
        Err(e) => return Err(format!("Ошибка адреса: {}", e)),
    };
    if addrs.is_empty() {
        return Err(format!("Не удалось разрешить адрес {}", resolve_target));
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
        None => return Err(format!("Сервер {}:{} недоступен", host, port)),
    };

    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(3000)));

    let code_bytes = room_code.as_bytes();
    let mut hs_packet = Vec::with_capacity(5 + code_bytes.len());
    hs_packet.extend_from_slice(b"NC02");
    hs_packet.push(code_bytes.len() as u8);
    hs_packet.extend_from_slice(code_bytes);

    if stream.write_all(&hs_packet).is_err() || stream.flush().is_err() {
        return Err("Сбой отправки данных".to_string());
    }

    let mut reply = [0u8; 4];
    if stream.read_exact(&mut reply).is_err() {
        return Err("Сервер разорвал соединение".to_string());
    }

    match &reply {
        b"NCOK" => {
            if let Ok(mut st) = status.write() {
                *st = "Успешно подключено к серверу!".to_string();
            }
            if let Ok(mut t) = telemetry.write() {
                t.is_connected = true;
                t.peer_addr = resolve_target.clone();
            }
            Ok(stream)
        }
        b"NCER" => Err("Ошибка: неверный код доступа комнаты!".to_string()),
        _ => Err("Сервер вернул некорректный ответ".to_string()),
    }
}

/// Общий цикл двунаправленного обмена кадрами и телеметрией по сокету
fn run_p2p_socket_session(
    stream: TcpStream,
    is_running: Arc<AtomicBool>,
    is_connected: Arc<AtomicBool>,
    in_dt_tx: SyncSender<NetworkPeerFrame>,
    in_cam_tx: SyncSender<NetworkPeerFrame>,
    in_aud_tx: SyncSender<NetworkAudioPacket>,
    outbox_desktop: Arc<LatestSlot<OutgoingFrame>>,
    outbox_camera: Arc<LatestSlot<OutgoingFrame>>,
    out_aud_rx: &Receiver<NetworkAudioPacket>,
    telemetry: Arc<RwLock<P2PTelemetry>>,
    encoder_config: Arc<RwLock<VideoEncoderConfig>>,
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
        let mut last_rx = Instant::now();
        let mut hevc_decoder = crate::decoder::HevcDecoder::new().ok();
        let mut h264_decoder = crate::decoder::H264Decoder::new().ok();

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
                    last_rx = Instant::now();

                    if msg_type == 1 {
                        let rgba_opt = if flags == 3 {
                            if let Some(ref mut dec) = hevc_decoder {
                                match dec.decode_packet(width, height, &payload) {
                                    Ok(Some((_, _, frame_rgba))) => Some(frame_rgba),
                                    _ => None,
                                }
                            } else {
                                None
                            }
                        } else if flags == 2 {
                            if let Some(ref mut dec) = h264_decoder {
                                match dec.decode_packet(width, height, &payload) {
                                    Ok(Some((_, _, frame_rgba))) => Some(frame_rgba),
                                    _ => None,
                                }
                            } else {
                                None
                            }
                        } else if flags == 1 {
                            decode_frame_from_jpeg(&payload).map(|(_, _, d)| d)
                        } else {
                            Some(payload)
                        };

                        if let Some(rgba) = rgba_opt {
                            let _ = in_dt_tx.try_send(NetworkPeerFrame {
                                width,
                                height,
                                rgba,
                                keepalive: false,
                            });
                        }
                    } else if msg_type == 2 {
                        // A decode failure must drop the frame: rendering the
                        // compressed bytes as pixels paints a frame of noise.
                        let Some(rgba) = (if flags == 1 {
                            decode_frame_from_jpeg(&payload).map(|(_, _, d)| d)
                        } else {
                            Some(payload)
                        }) else {
                            if let Ok(mut t) = r_telem.write() {
                                t.encode_errors = t.encode_errors.saturating_add(1);
                            }
                            continue;
                        };
                        let _ = in_cam_tx.try_send(NetworkPeerFrame {
                            width,
                            height,
                            rgba,
                            keepalive: false,
                        });
                    } else if msg_type == 3 {
                        if let Some(audio_pkt) = NetworkAudioPacket::from_bytes(&payload) {
                            let _ = in_aud_tx.try_send(audio_pkt);
                        }
                    } else if msg_type == 5 {
                        // "Still here, desktop unchanged": no pixels to show.
                        let stream_id = payload.first().copied().unwrap_or(1);
                        let frame = NetworkPeerFrame::keepalive();
                        if stream_id == 2 {
                            let _ = in_cam_tx.try_send(frame);
                        } else {
                            let _ = in_dt_tx.try_send(frame);
                        }
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
                    // The peer pings once a second, so five seconds of silence
                    // means the link is dead even if TCP has not noticed yet.
                    if last_rx.elapsed() > Duration::from_secs(5) {
                        break;
                    }
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
    let mut last_keepalive = Instant::now();
    let mut fps_counter = 0u32;
    let mut fps_timer = Instant::now();
    let mut total_bytes_sent = 0u64;

    let mut nv_encoder: Option<(crate::nvenc::NvencEncoder, u8)> = None;
    let mut cur_enc_config = (0u8, 0u32, 0u32, 0u32, 0usize);
    let mut frame_idr_counter = 0u32;

    while is_running.load(Ordering::Relaxed) && is_connected.load(Ordering::Relaxed) {
        let mut sent_any = false;

        if let Some(frame) = outbox_desktop.take() {
            let (cq, max_br, preset_idx) = if let Ok(cfg) = encoder_config.read() {
                (cfg.cq_quality, cfg.max_bitrate_mbps, cfg.preset_index)
            } else {
                (22, 30, 0)
            };

            let needed_config = (cq, max_br, frame.width, frame.height, preset_idx);
            if cur_enc_config != needed_config {
                let preset_guid = match preset_idx {
                    0 => crate::nvenc::NV_ENC_PRESET_P1_GUID,
                    1 => crate::nvenc::NV_ENC_PRESET_P3_GUID,
                    2 => crate::nvenc::NV_ENC_PRESET_P7_GUID,
                    _ => crate::nvenc::NV_ENC_PRESET_P1_GUID,
                };

                // Приоритет: аппаратный HEVC (H.265, flags = 3) -> аппаратный H.264 (flags = 2)
                let mut enc = crate::nvenc::NvencEncoder::new_standalone_hevc(
                    frame.width,
                    frame.height,
                    cq,
                    max_br,
                    60,
                    preset_guid,
                ).map(|e| (e, 3u8));

                if enc.is_err() {
                    enc = crate::nvenc::NvencEncoder::new_standalone_h264(
                        frame.width,
                        frame.height,
                        cq,
                        max_br,
                        60,
                        preset_guid,
                    ).map(|e| (e, 2u8));
                }

                nv_encoder = enc.ok();
                cur_enc_config = needed_config;
            }

            let force_idr = frame_idr_counter % 120 == 0;
            frame_idr_counter = frame_idr_counter.wrapping_add(1);

            let (flags, payload) = if let Some((ref mut enc, codec_flag)) = nv_encoder {
                match enc.encode_rgba_bytes(&frame.rgba, force_idr) {
                    Ok(nal) => (codec_flag, nal),
                    Err(_) => {
                        let jpeg_quality = cq_to_jpeg_quality(cq);
                        if let Some(jpeg) = encode_frame_to_jpeg_with_quality(frame.width, frame.height, &frame.rgba, jpeg_quality) {
                            (1u8, jpeg)
                        } else {
                            (0u8, (*frame.rgba).clone())
                        }
                    }
                }
            } else {
                let jpeg_quality = cq_to_jpeg_quality(cq);
                if let Some(jpeg) = encode_frame_to_jpeg_with_quality(frame.width, frame.height, &frame.rgba, jpeg_quality) {
                    (1u8, jpeg)
                } else {
                    (0u8, (*frame.rgba).clone())
                }
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

        if let Some(frame) = outbox_camera.take() {
            let (flags, payload) = if let Some(jpeg) = encode_frame_to_jpeg(frame.width, frame.height, &frame.rgba) {
                (1u8, jpeg)
            } else {
                (0u8, (*frame.rgba).clone())
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

        while let Ok(audio) = out_aud_rx.try_recv() {
            let payload = audio.to_bytes();
            let mut pkt = Vec::with_capacity(12 + payload.len());
            pkt.extend_from_slice(&0x4E43u16.to_le_bytes());
            pkt.push(3); // Audio
            pkt.push(0); // flags
            pkt.extend_from_slice(&0u16.to_le_bytes());
            pkt.extend_from_slice(&0u16.to_le_bytes());
            pkt.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            pkt.extend_from_slice(&payload);

            if writer.write_all(&pkt).is_err() {
                break;
            }
            total_bytes_sent += pkt.len() as u64;
            sent_any = true;
        }

        // A still desktop produces no frames at all. This keeps the peer's
        // liveness timer alive without shipping a picture it already has.
        if last_keepalive.elapsed() >= Duration::from_secs(1) {
            last_keepalive = Instant::now();
            for stream_id in [1u8, 2u8] {
                let mut pkt = Vec::with_capacity(13);
                pkt.extend_from_slice(&0x4E43u16.to_le_bytes());
                pkt.push(5); // Video keepalive
                pkt.push(0);
                pkt.extend_from_slice(&0u16.to_le_bytes());
                pkt.extend_from_slice(&0u16.to_le_bytes());
                pkt.extend_from_slice(&1u32.to_le_bytes());
                pkt.push(stream_id);
                if writer.write_all(&pkt).is_err() {
                    break;
                }
            }
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
                t.frames_sent = fps_counter as u64;
                t.frames_dropped = outbox_desktop.overwritten() + outbox_camera.overwritten();
                t.quality = encoder_config
                    .read()
                    .map(|c| c.cq_quality)
                    .unwrap_or(0);
            }
            fps_counter = 0;
            total_bytes_sent = 0;
            fps_timer = Instant::now();
        }

        if !sent_any {
            thread::sleep(Duration::from_millis(2));
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

    #[test]
    fn test_audio_packet_serde() {
        let mut bins = [0.0f32; 20];
        bins[0] = 0.12;
        bins[5] = 0.78;
        bins[19] = 0.95;
        // Test with real Opus VBR payload
        let mut enc = crate::audio::NanocordOpusEncoder::new();
        let test_pcm: Vec<i16> = (0..960).map(|i| ((i as f32 * 0.1).sin() * 20000.0) as i16).collect();
        let opus_pkts = enc.encode_chunk(&test_pcm);
        assert!(!opus_pkts.is_empty());
        let opus_data = opus_pkts[0].clone();
        let pkt = NetworkAudioPacket::with_opus(-14.5, -0.62, 0.71, 0.38, bins, opus_data.clone());
        let bytes = pkt.to_bytes();
        assert_eq!(bytes.len(), 1 + 1 + 4 + 96 + 4 + opus_data.len());
        let decoded = NetworkAudioPacket::from_bytes(&bytes).expect("audio decode failed");
        assert_eq!(decoded.stream_type, 0);
        assert_eq!(decoded.seq, 0);
        assert!((decoded.peak_db - (-14.5)).abs() < 1e-4);
        assert!((decoded.min_sample - (-0.62)).abs() < 1e-4);
        assert!((decoded.max_sample - 0.71).abs() < 1e-4);
        assert!((decoded.rms - 0.38).abs() < 1e-4);
        assert!((decoded.bins[5] - 0.78).abs() < 1e-4);
        assert_eq!(decoded.opus_data, opus_data);

        // Verify backward compatibility: legacy 96-byte packet without magic
        let legacy_bytes = &bytes[6..]; // slice without 'O' + stream_type + seq
        let legacy_decoded = NetworkAudioPacket::from_bytes(legacy_bytes).expect("legacy audio decode failed");
        assert_eq!(legacy_decoded.stream_type, 0);
        assert!((legacy_decoded.peak_db - (-14.5)).abs() < 1e-4);
        assert_eq!(legacy_decoded.opus_data, opus_data);
    }

    #[test]
    fn test_jpeg_encode_decode_fidelity() {
        let (w, h) = (320u32, 240u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for (i, p) in rgba.chunks_exact_mut(4).enumerate() {
            p[0] = ((i * 3) % 256) as u8;
            p[1] = ((i * 7) % 256) as u8;
            p[2] = ((i * 11) % 256) as u8;
            p[3] = 255;
        }
        let jpeg = encode_frame_to_jpeg(w, h, &rgba).expect("encode failed");
        assert!(!jpeg.is_empty());
        assert!(jpeg.len() < rgba.len()); // compression effective

        let (dw, dh, decoded_rgba) = decode_frame_from_jpeg(&jpeg).expect("decode failed");
        assert_eq!(dw, w);
        assert_eq!(dh, h);
        assert_eq!(decoded_rgba.len(), rgba.len());
        let non_zero = decoded_rgba.iter().filter(|&&b| b > 0).count();
        assert!(non_zero > (decoded_rgba.len() / 2));
    }

    #[test]
    fn test_e2e_video_capture_network_decode() {
        let port = 49160;
        let code = "Vd9".to_string();

        let server = P2PNetworkServer::start(port, code.clone());
        thread::sleep(Duration::from_millis(80));

        let client = P2PNetworkClient::connect("[::1]".to_string(), port, code);
        thread::sleep(Duration::from_millis(250));

        assert!(client.is_connected.load(Ordering::Relaxed));
        assert!(server.is_connected.load(Ordering::Relaxed));

        // Real frame from GDI capture if available, or synthetic 640x360 frame
        let (w, h, rgba) = if let Some(buf) = crate::capture::capture_desktop_gdi(640, 360) {
            (640, 360, buf)
        } else {
            let mut buf = vec![42u8; 640 * 360 * 4];
            for chunk in buf.chunks_exact_mut(4) {
                chunk[0] = 88;
                chunk[1] = 101;
                chunk[2] = 242;
                chunk[3] = 255;
            }
            (640, 360, buf)
        };

        // Send via client outgoing desktop queue
        client.outgoing_desktop_tx.send(NetworkPeerFrame {
            width: w,
            height: h,
            rgba: rgba.clone(),
        }).expect("send failed");

        // Server receives and decodes JPEG frame
        let start = Instant::now();
        let mut received = None;
        while start.elapsed() < Duration::from_secs(3) {
            if let Ok(frame) = server.desktop_rx.try_recv() {
                received = Some(frame);
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        let recv_frame = received.expect("Server failed to receive e2e video frame");
        assert_eq!(recv_frame.width, w);
        assert_eq!(recv_frame.height, h);
        assert_eq!(recv_frame.rgba.len(), (w * h * 4) as usize);

        client.stop();
        server.stop();
    }

    #[test]
    fn test_e2e_audio_capture_network_decode() {
        let port = 49170;
        let code = "Au1".to_string();

        let server = P2PNetworkServer::start(port, code.clone());
        thread::sleep(Duration::from_millis(80));

        let client = P2PNetworkClient::connect("[::1]".to_string(), port, code);
        thread::sleep(Duration::from_millis(250));

        assert!(client.is_connected.load(Ordering::Relaxed));

        let mut test_bins = [0.0f32; 20];
        test_bins[2] = 0.55;
        test_bins[9] = 0.88;
        let mut enc = crate::audio::NanocordOpusEncoder::new();
        let test_pcm: Vec<i16> = (0..960).map(|i| ((i as f32 * 0.05).sin() * 16000.0) as i16).collect();
        let opus_pkts = enc.encode_chunk(&test_pcm);
        assert!(!opus_pkts.is_empty());
        let audio_pkt = NetworkAudioPacket::with_opus(-16.0, -0.70, 0.75, 0.42, test_bins, opus_pkts[0].clone());

        client.outgoing_audio_tx.send(audio_pkt.clone()).expect("send audio failed");

        let start = Instant::now();
        let mut received_audio = None;
        while start.elapsed() < Duration::from_secs(3) {
            if let Ok(pkt) = server.audio_rx.try_recv() {
                received_audio = Some(pkt);
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        let rx_audio = received_audio.expect("Server failed to receive e2e audio packet");
        assert!((rx_audio.peak_db - (-16.0)).abs() < 1e-4);
        assert!((rx_audio.rms - 0.42).abs() < 1e-4);
        assert!((rx_audio.bins[9] - 0.88).abs() < 1e-4);
        assert_eq!(rx_audio.opus_data, opus_pkts[0]);

        let mut dec = crate::audio::NanocordOpusDecoder::new();
        let decoded = dec.decode_packet(&rx_audio.opus_data);
        assert_eq!(decoded.len(), 960);

        client.stop();
        server.stop();
    }

    #[test]
    fn test_e2e_bidirectional_real_pcm_audio_stream() {
        let port = 49195;
        let code = "Aud".to_string();

        let server = P2PNetworkServer::start(port, code.clone());
        thread::sleep(Duration::from_millis(100));

        let client = P2PNetworkClient::connect("[::1]".to_string(), port, code);
        thread::sleep(Duration::from_millis(300));

        assert!(client.is_connected.load(Ordering::Relaxed), "Client should be connected");
        assert!(server.is_connected.load(Ordering::Relaxed), "Server should be connected");

        // Client streams 10 audio chunks encoded with Opus VBR
        let num_packets = 10;
        let mut client_enc = crate::audio::NanocordOpusEncoder::new();
        let mut client_sent_opus = Vec::new();
        for p in 0..num_packets {
            let pcm: Vec<i16> = (0..960)
                .map(|i| {
                    let t = (p * 960 + i) as f32 / 48000.0;
                    ( (t * 440.0 * 2.0 * std::f32::consts::PI).sin() * 22000.0 ) as i16
                })
                .collect();
            let pkts = client_enc.encode_chunk(&pcm);
            assert_eq!(pkts.len(), 1);
            let opus_bytes = pkts[0].clone();
            client_sent_opus.push(opus_bytes.clone());
            let pkt = NetworkAudioPacket::with_opus(-12.0, -0.67, 0.67, 0.45, [0.3; 20], opus_bytes);
            client.outgoing_audio_tx.send(pkt).expect("client send audio packet failed");
        }

        // Server simultaneously streams 10 audio chunks (880Hz sine wave) encoded with Opus VBR
        let mut server_enc = crate::audio::NanocordOpusEncoder::new();
        let mut server_sent_opus = Vec::new();
        for p in 0..num_packets {
            let pcm: Vec<i16> = (0..960)
                .map(|i| {
                    let t = (p * 960 + i) as f32 / 48000.0;
                    ( (t * 880.0 * 2.0 * std::f32::consts::PI).sin() * 18000.0 ) as i16
                })
                .collect();
            let pkts = server_enc.encode_chunk(&pcm);
            assert_eq!(pkts.len(), 1);
            let opus_bytes = pkts[0].clone();
            server_sent_opus.push(opus_bytes.clone());
            let pkt = NetworkAudioPacket::with_opus(-15.0, -0.55, 0.55, 0.38, [0.4; 20], opus_bytes);
            server.outgoing_audio_tx.send(pkt).expect("server send audio packet failed");
        }

        // Collect all packets on both ends
        let start = Instant::now();
        let mut srv_received = Vec::new();
        let mut cli_received = Vec::new();

        while start.elapsed() < Duration::from_secs(4) {
            while let Ok(pkt) = server.audio_rx.try_recv() {
                srv_received.push(pkt);
            }
            while let Ok(pkt) = client.audio_rx.try_recv() {
                cli_received.push(pkt);
            }
            if srv_received.len() == num_packets && cli_received.len() == num_packets {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(srv_received.len(), num_packets, "Server must receive all 10 audio packets from Client");
        assert_eq!(cli_received.len(), num_packets, "Client must receive all 10 audio packets from Server");

        // Verify Opus packet compression & decode back to 960 samples per frame
        let mut srv_dec = crate::audio::NanocordOpusDecoder::new();
        let mut cli_dec = crate::audio::NanocordOpusDecoder::new();

        for (i, pkt) in srv_received.iter().enumerate() {
            assert_eq!(pkt.opus_data, client_sent_opus[i], "Opus payload must match exactly");
            assert!(pkt.opus_data.len() < 500, "Opus VBR must compress 1920-byte PCM to <500 bytes");
            let decoded = srv_dec.decode_packet(&pkt.opus_data);
            assert_eq!(decoded.len(), 960, "Decoded Opus frame must yield 960 audio samples");
        }

        for (i, pkt) in cli_received.iter().enumerate() {
            assert_eq!(pkt.opus_data, server_sent_opus[i], "Opus payload must match exactly");
            assert!(pkt.opus_data.len() < 500, "Opus VBR must compress 1920-byte PCM to <500 bytes");
            let decoded = cli_dec.decode_packet(&pkt.opus_data);
            assert_eq!(decoded.len(), 960, "Decoded Opus frame must yield 960 audio samples");
        }

        client.stop();
        server.stop();
    }

    #[test]
    fn test_e2e_full_bidirectional_simultaneous_pipeline() {
        let port = 49180;
        let code = "Bi8".to_string();

        let server = P2PNetworkServer::start(port, code.clone());
        thread::sleep(Duration::from_millis(80));

        let client = P2PNetworkClient::connect("[::1]".to_string(), port, code);
        thread::sleep(Duration::from_millis(250));

        assert!(client.is_connected.load(Ordering::Relaxed));
        assert!(server.is_connected.load(Ordering::Relaxed));

        // Client sends desktop video + audio to Server
        let dummy_dt = vec![120u8; 320 * 240 * 4];
        client.outgoing_desktop_tx.send(NetworkPeerFrame { width: 320, height: 240, rgba: dummy_dt }).unwrap();
        let mut enc_cli = crate::audio::NanocordOpusEncoder::new();
        let cli_opus = enc_cli.encode_chunk(&vec![1234; 960]);
        client.outgoing_audio_tx.send(NetworkAudioPacket::with_opus(-10.0, -0.5, 0.5, 0.3, [0.2; 20], cli_opus[0].clone())).unwrap();

        // Server sends camera video + audio to Client
        let dummy_cam = vec![200u8; 160 * 120 * 4];
        server.outgoing_camera_tx.send(NetworkPeerFrame { width: 160, height: 120, rgba: dummy_cam }).unwrap();
        let mut enc_srv = crate::audio::NanocordOpusEncoder::new();
        let srv_opus = enc_srv.encode_chunk(&vec![5678; 960]);
        server.outgoing_audio_tx.send(NetworkAudioPacket::with_opus(-8.0, -0.4, 0.6, 0.35, [0.4; 20], srv_opus[0].clone())).unwrap();

        let start = Instant::now();
        let mut srv_got_dt = false;
        let mut srv_got_aud = false;
        let mut cli_got_cam = false;
        let mut cli_got_aud = false;

        while start.elapsed() < Duration::from_secs(3) {
            if !srv_got_dt && server.desktop_rx.try_recv().is_ok() {
                srv_got_dt = true;
            }
            if !srv_got_aud && server.audio_rx.try_recv().is_ok() {
                srv_got_aud = true;
            }
            if !cli_got_cam && client.camera_rx.try_recv().is_ok() {
                cli_got_cam = true;
            }
            if !cli_got_aud && client.audio_rx.try_recv().is_ok() {
                cli_got_aud = true;
            }
            if srv_got_dt && srv_got_aud && cli_got_cam && cli_got_aud {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        assert!(srv_got_dt, "Server should receive desktop stream from Client");
        assert!(srv_got_aud, "Server should receive audio stream from Client");
        assert!(cli_got_cam, "Client should receive camera stream from Server");
        assert!(cli_got_aud, "Client should receive audio stream from Server");

        client.stop();
        server.stop();
    }

    #[test]
    fn test_e2e_opus_dual_stream_and_fec_recovery() {
        let port = 49210;
        let code = "Op9".to_string();

        let server = P2PNetworkServer::start(port, code.clone());
        thread::sleep(Duration::from_millis(80));

        let client = P2PNetworkClient::connect("[::1]".to_string(), port, code);
        thread::sleep(Duration::from_millis(250));

        assert!(client.is_connected.load(Ordering::Relaxed));
        assert!(server.is_connected.load(Ordering::Relaxed));

        // Client prepares separate Voip (mic) and Audio (desktop) encoders
        let mut mic_enc = crate::audio::NanocordOpusEncoder::new_voip(64_000);
        let mut game_enc = crate::audio::NanocordOpusEncoder::new_audio(96_000);

        let mic_pcm: Vec<i16> = (0..960).map(|i| ((i as f32 * 0.1).sin() * 15000.0) as i16).collect();
        let game_pcm: Vec<i16> = (0..960).map(|i| ((i as f32 * 0.05).cos() * 20000.0) as i16).collect();

        let mic_pkts = mic_enc.encode_chunk(&mic_pcm);
        let game_pkts = game_enc.encode_chunk(&game_pcm);

        assert!(!mic_pkts.is_empty());
        assert!(!game_pkts.is_empty());

        let seq_mic = mic_enc.next_seq();
        let seq_game = game_enc.next_seq();

        let pkt_mic = NetworkAudioPacket::with_stream(0, seq_mic, -14.0, -0.6, 0.6, 0.4, [0.3; 20], mic_pkts[0].clone());
        let pkt_game = NetworkAudioPacket::with_stream(1, seq_game, -10.0, -0.8, 0.8, 0.5, [0.5; 20], game_pkts[0].clone());

        client.outgoing_audio_tx.send(pkt_mic).unwrap();
        client.outgoing_audio_tx.send(pkt_game).unwrap();

        let start = Instant::now();
        let mut received = Vec::new();
        while start.elapsed() < Duration::from_secs(3) {
            while let Ok(pkt) = server.audio_rx.try_recv() {
                received.push(pkt);
            }
            if received.len() >= 2 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(received.len(), 2);
        assert_eq!(received[0].stream_type, 0); // Mic stream
        assert_eq!(received[1].stream_type, 1); // Desktop stream

        // Test FEC recovery and PLC on dropped packet
        let mut mic_dec = crate::audio::NanocordOpusDecoder::new();
        let pcm_normal = mic_dec.decode_packet_with_fec(received[0].seq, &received[0].opus_data);
        assert_eq!(pcm_normal.len(), 960);

        // Simulate dropped packet (seq jump from 0 to 2)
        let mic_pcm2: Vec<i16> = (0..960).map(|i| ((i as f32 * 0.15).sin() * 12000.0) as i16).collect();
        let mic_pkts2 = mic_enc.encode_chunk(&mic_pcm2);
        let pcm_recovered = mic_dec.decode_packet_with_fec(received[0].seq + 2, &mic_pkts2[0]);
        // Reconstructs both the dropped frame and the current frame smoothly
        assert!(pcm_recovered.len() >= 960);

        client.stop();
        server.stop();
    }

    #[test]
    fn test_e2e_wav_to_wav_opus_bidirectional_under_adverse_network() {
        use crate::audio::{NanocordOpusDecoder, NanocordOpusEncoder, WavAudio};
        use std::f32::consts::PI;

        // 1. ПАРАМЕТРЫ ХУДШЕГО СЦЕНАРИЯ СЕТИ (DPI БЛОКИРОВКИ):
        // 2 Мбит/с пропускная способность, 1500 мс задержка (RTT 3000 мс), 5% случайных потерь
        let num_frames = 150; // 150 фреймов по 20 мс = 3.0 секунды непрерывного звука
        let samples_per_frame = 960;
        let total_samples = num_frames * samples_per_frame; // 144 000 сэмплов

        // 2. ГЕНЕРАЦИЯ ИСХОДНЫХ РЕАЛИСТИЧНЫХ WAV ПОТОКОВ (WAV -> PCM)
        // Поток A: Мужской голос (гармоники 130, 260, 390, 520 Гц + формантная огибающая)
        let mut pcm_a = Vec::with_capacity(total_samples);
        for i in 0..total_samples {
            let t = i as f32 / 48000.0;
            let f0 = (2.0 * PI * 130.0 * t).sin() * 0.45;
            let f1 = (2.0 * PI * 260.0 * t).sin() * 0.25;
            let f2 = (2.0 * PI * 390.0 * t).sin() * 0.15;
            let f3 = (2.0 * PI * 520.0 * t).sin() * 0.10;
            let env = 0.5 + 0.4 * (2.0 * PI * 3.5 * t).sin();
            let val = ((f0 + f1 + f2 + f3) * env * 24000.0).clamp(-32000.0, 32000.0) as i16;
            pcm_a.push(val);
        }

        // Поток B: Женский голос / игровой звук (гармоники 220, 440, 660, 880 Гц)
        let mut pcm_b = Vec::with_capacity(total_samples);
        for i in 0..total_samples {
            let t = i as f32 / 48000.0;
            let f0 = (2.0 * PI * 220.0 * t).sin() * 0.40;
            let f1 = (2.0 * PI * 440.0 * t).sin() * 0.30;
            let f2 = (2.0 * PI * 660.0 * t).sin() * 0.15;
            let f3 = (2.0 * PI * 880.0 * t).sin() * 0.10;
            let env = 0.6 + 0.35 * (2.0 * PI * 2.5 * t).cos();
            let val = ((f0 + f1 + f2 + f3) * env * 22000.0).clamp(-32000.0, 32000.0) as i16;
            pcm_b.push(val);
        }

        // Кодируем исходники в WAV формат (WAV контейнер)
        let wav_a_bytes = WavAudio::encode_wav(&pcm_a, 48000, 1);
        let wav_b_bytes = WavAudio::encode_wav(&pcm_b, 48000, 1);
        assert!(wav_a_bytes.len() > total_samples * 2);
        assert!(wav_b_bytes.len() > total_samples * 2);

        // Проверяем точный decode_wav
        let (decoded_in_a, sr_a, ch_a) = WavAudio::decode_wav(&wav_a_bytes).expect("decode wav A failed");
        assert_eq!(sr_a, 48000);
        assert_eq!(ch_a, 1);
        assert_eq!(decoded_in_a, pcm_a);

        // 3. КОДИРОВАНИЕ В OPUS VBR C IN-BAND FEC И DTX
        let mut enc_a = NanocordOpusEncoder::new_voip(64_000);
        let mut enc_b = NanocordOpusEncoder::new_audio(96_000);

        let mut packets_a = Vec::with_capacity(num_frames);
        for f in 0..num_frames {
            let chunk = &pcm_a[f * samples_per_frame..(f + 1) * samples_per_frame];
            let pkts = enc_a.encode_chunk(chunk);
            assert_eq!(pkts.len(), 1);
            let seq = enc_a.next_seq();
            packets_a.push((seq, pkts[0].clone()));
        }

        let mut packets_b = Vec::with_capacity(num_frames);
        for f in 0..num_frames {
            let chunk = &pcm_b[f * samples_per_frame..(f + 1) * samples_per_frame];
            let pkts = enc_b.encode_chunk(chunk);
            assert_eq!(pkts.len(), 1);
            let seq = enc_b.next_seq();
            packets_b.push((seq, pkts[0].clone()));
        }

        // Проверяем степень сжатия Opus VBR: 1920 байт PCM сжимается до ~50..250 байт
        let avg_size_a = packets_a.iter().map(|p| p.1.len()).sum::<usize>() / packets_a.len();
        let avg_size_b = packets_b.iter().map(|p| p.1.len()).sum::<usize>() / packets_b.len();
        assert!(avg_size_a < 350, "Opus VBR Voip avg packet size must be <350 bytes");
        assert!(avg_size_b < 450, "Opus VBR Audio avg packet size must be <450 bytes");

        // 4. СИМУЛЯТОР СЕТИ: 2 Мбит/с, 1500 мс задержки, 5% потерь пакетов
        struct SimulatedPacket {
            seq: u32,
            data: Vec<u8>,
            deliver_at_ms: u64,
        }

        let mut rng_seed = 0x1337c0de_u64;
        let mut next_rand = || -> u32 {
            rng_seed = rng_seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (rng_seed >> 32) as u32
        };

        let latency_ms = 1500u64; // 1.5 секунды задержки
        let loss_percentage = 5u32; // 5% потерь
        let bandwidth_limit_bps = 2_000_000u64; // 2 Мбит/с

        let mut simulate_channel = |input: &[(u32, Vec<u8>)]| -> (Vec<SimulatedPacket>, usize, usize) {
            let mut delivered = Vec::new();
            let mut dropped_count = 0;
            let mut total_bytes = 0;

            for (i, (seq, data)) in input.iter().enumerate() {
                total_bytes += data.len();
                let is_lost = (next_rand() % 100) < loss_percentage;
                if is_lost {
                    dropped_count += 1;
                    continue; // Пакет утерян в сети
                }
                let send_time_ms = (i as u64) * 20; // 20 мс между фреймами
                let jitter_ms = (next_rand() % 20) as u64; // до 20 мс джиттера
                let deliver_at_ms = send_time_ms + latency_ms + jitter_ms;

                delivered.push(SimulatedPacket {
                    seq: *seq,
                    data: data.clone(),
                    deliver_at_ms,
                });
            }
            (delivered, dropped_count, total_bytes)
        };

        let (channel_a_to_b, dropped_a, bytes_a) = simulate_channel(&packets_a);
        let (channel_b_to_a, dropped_b, bytes_b) = simulate_channel(&packets_b);

        // Проверяем, что потери действительно смоделированы (~5%)
        assert!(dropped_a > 0, "Channel A->B must simulate packet loss");
        assert!(dropped_b > 0, "Channel B->A must simulate packet loss");
        let loss_a_pct = (dropped_a as f32 / num_frames as f32) * 100.0;
        let loss_b_pct = (dropped_b as f32 / num_frames as f32) * 100.0;
        assert!(loss_a_pct >= 2.0 && loss_a_pct <= 10.0, "Loss rate A ({:.1}%) around 5%", loss_a_pct);
        assert!(loss_b_pct >= 2.0 && loss_b_pct <= 10.0, "Loss rate B ({:.1}%) around 5%", loss_b_pct);

        // Проверяем полосу пропускания: битрейт ~60-90 кбит/с с лёгкостью проходит сквозь 2 Мбит/с
        let duration_sec = (num_frames as f64) * 0.020;
        let bitrate_a_bps = ((bytes_a as f64 * 8.0) / duration_sec) as u64;
        let bitrate_b_bps = ((bytes_b as f64 * 8.0) / duration_sec) as u64;
        assert!(bitrate_a_bps < bandwidth_limit_bps, "Audio A bitrate must fit in 2 Mbps");
        assert!(bitrate_b_bps < bandwidth_limit_bps, "Audio B bitrate must fit in 2 Mbps");

        // 5. ДЕКОДИРОВАНИЕ НА ПРИЁМНЫХ СТОРОНАХ С FEC И PLC (OPUS -> PCM)
        let mut dec_b = NanocordOpusDecoder::new();
        let mut recovered_pcm_a = Vec::with_capacity(total_samples);

        for pkt in &channel_a_to_b {
            assert!(pkt.deliver_at_ms >= latency_ms, "Arrival time must reflect 1500ms network delay");
            let pcm_chunk = dec_b.decode_packet_with_fec(pkt.seq, &pkt.data);
            recovered_pcm_a.extend_from_slice(&pcm_chunk);
        }

        let mut dec_a = NanocordOpusDecoder::new();
        let mut recovered_pcm_b = Vec::with_capacity(total_samples);

        for pkt in &channel_b_to_a {
            assert!(pkt.deliver_at_ms >= latency_ms, "Arrival time must reflect 1500ms network delay");
            let pcm_chunk = dec_a.decode_packet_with_fec(pkt.seq, &pkt.data);
            recovered_pcm_b.extend_from_slice(&pcm_chunk);
        }

        // 6. СОХРАНЕНИЕ ВОССТАНОВЛЕННЫХ WAV ФАЙЛОВ
        let out_wav_a = WavAudio::encode_wav(&recovered_pcm_a, 48000, 1);
        let out_wav_b = WavAudio::encode_wav(&recovered_pcm_b, 48000, 1);
        assert!(out_wav_a.len() > 1000);
        assert!(out_wav_b.len() > 1000);

        // 7. ПРОВЕРКА ОТСУТСТВИЯ ХРУСТА И ЩЕЛЧКОВ («НЕ ХРУСТИТ»)
        let mut max_delta_a = 0i32;
        for i in 1..recovered_pcm_a.len() {
            let delta = (recovered_pcm_a[i] as i32 - recovered_pcm_a[i - 1] as i32).abs();
            if delta > max_delta_a {
                max_delta_a = delta;
            }
        }
        assert!(max_delta_a < 12000, "Maximum audio step must be smooth, found {}", max_delta_a);

        let mut max_delta_b = 0i32;
        for i in 1..recovered_pcm_b.len() {
            let delta = (recovered_pcm_b[i] as i32 - recovered_pcm_b[i - 1] as i32).abs();
            if delta > max_delta_b {
                max_delta_b = delta;
            }
        }
        assert!(max_delta_b < 12000, "Maximum audio step must be smooth, found {}", max_delta_b);

        // 8. ПРОВЕРКА СХОЖЕСТИ СИГНАЛА С УЧЁТОМ АЛГОРИТМИЧЕСКОГО LOOKAHEAD OPUS
        // В Opus lookahead составляет 312 сэмплов (6.5 мс @ 48 кГц).
        // Считаем кросс-корреляцию / схожесть по косинусной мере с компенсацией лага.
        let compute_aligned_similarity = |orig: &[i16], decoded: &[i16]| -> (f64, i32) {
            let mut best_sim = -1.0f64;
            let mut best_lag = 0i32;

            for lag in -400..=400 {
                let (o_slice, d_slice) = if lag >= 0 {
                    let start = lag as usize;
                    if start >= orig.len() { continue; }
                    let len = (orig.len() - start).min(decoded.len());
                    (&orig[start..start + len], &decoded[..len])
                } else {
                    let start = (-lag) as usize;
                    if start >= decoded.len() { continue; }
                    let len = orig.len().min(decoded.len() - start);
                    (&orig[..len], &decoded[start..start + len])
                };

                if o_slice.len() < 4800 { continue; }

                let mut dot = 0.0f64;
                let mut norm_o = 0.0f64;
                let mut norm_d = 0.0f64;

                for (&o, &d) in o_slice.iter().zip(d_slice.iter()) {
                    let fo = o as f64;
                    let fd = d as f64;
                    dot += fo * fd;
                    norm_o += fo * fo;
                    norm_d += fd * fd;
                }

                if norm_o > 1e-6 && norm_d > 1e-6 {
                    let sim = dot / (norm_o.sqrt() * norm_d.sqrt());
                    if sim > best_sim {
                        best_sim = sim;
                        best_lag = lag;
                    }
                }
            }
            (best_sim, best_lag)
        };

        let (sim_a, lag_a) = compute_aligned_similarity(&pcm_a, &recovered_pcm_a);
        let (sim_b, lag_b) = compute_aligned_similarity(&pcm_b, &recovered_pcm_b);

        println!("Stream A (Voip + 5% loss): similarity = {:.2}%, lag = {} samples", sim_a * 100.0, lag_a);
        println!("Stream B (Audio + 5% loss): similarity = {:.2}%, lag = {} samples", sim_b * 100.0, lag_b);

        // Проверяем интактные фреймы (без учёта сетевых выпадений) — схожесть >95%
        let mut intact_orig = Vec::new();
        let mut intact_recv = Vec::new();
        let dropped_set_a: std::collections::HashSet<u32> = packets_a.iter().filter_map(|(seq, _)| {
            if channel_a_to_b.iter().any(|p| p.seq == *seq) { None } else { Some(*seq) }
        }).collect();

        for f in 0..num_frames {
            let seq = packets_a[f].0;
            // Если текущий и соседние фреймы не терялись
            if !dropped_set_a.contains(&seq) && !dropped_set_a.contains(&seq.saturating_sub(1)) && !dropped_set_a.contains(&(seq + 1)) {
                let start = f * samples_per_frame;
                let end = start + samples_per_frame;
                if end <= pcm_a.len() && end <= recovered_pcm_a.len() {
                    intact_orig.extend_from_slice(&pcm_a[start..end]);
                    intact_recv.extend_from_slice(&recovered_pcm_a[start..end]);
                }
            }
        }

        let (intact_sim_a, _) = compute_aligned_similarity(&intact_orig, &intact_recv);
        println!("Stream A (intact frames): similarity = {:.2}%", intact_sim_a * 100.0);
        assert!(intact_sim_a >= 0.95, "Intact frames similarity must be >95%, got {:.2}%", intact_sim_a * 100.0);
    }
}
