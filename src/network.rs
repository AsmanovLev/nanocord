#![allow(dead_code, unused_variables)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
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
