#![allow(dead_code, unused_variables, unused_imports)]
#![windows_subsystem = "windows"]

use nanocord::audio;
use nanocord::benchmark;
use nanocord::camera;
use nanocord::capture::{self, DxgiCapture};
use nanocord::network;
use nanocord::nvenc::{self, CodecPreference, NvencEncoder, VideoCodec};
use nanocord::ui::{self, NanocordApp};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

fn main() -> eframe::Result<()> {
    nanocord::update::check_and_install();

    std::panic::set_hook(Box::new(|info| {
        let msg = format!("{:#?}\n", info);
        let _ = std::fs::write("panic.txt", &msg);
        nanocord::diag::log(&format!("PANIC: {}", msg));
        if let Ok(mut exe) = std::env::current_exe() {
            exe.pop();
            exe.push("panic.txt");
            let _ = std::fs::write(exe, &msg);
        }
    }));
    let args: Vec<String> = std::env::args().collect();
    let is_test_mode = args.iter().any(|a| a == "--test");
    let is_bench_mode = args.iter().any(|a| a == "--benchmark");
    let is_cli_mode = args.iter().any(|a| a == "--cli");
    let is_test_capture = args.iter().any(|a| a == "--test-capture");
    let is_test_decoder = args.iter().any(|a| a == "--test-decoder");
    let is_codec_report = args.iter().any(|a| a == "--codecs");
    let is_nvenc_probe = args.iter().any(|a| a == "--nvenc-probe");
    let is_test_mf = args.iter().any(|a| a == "--test-mf");
    let listen_arg = args.iter().position(|a| a == "--listen-test");
    let is_loopback_test = args.iter().any(|a| a == "--loopback-test");
    let upnp_arg = args.iter().position(|a| a == "--upnp");

    if is_test_mode
        || is_bench_mode
        || is_cli_mode
        || is_test_capture
        || is_test_decoder
        || is_codec_report
        || is_nvenc_probe
        || is_test_mf
        || listen_arg.is_some()
        || is_loopback_test
        || upnp_arg.is_some()
    {
        attach_console_if_cli();
        println!("=================================================");
        println!("   NANOCORD - Ultra-Hardcoded Native Streamer    ");
        println!("=================================================");
    }

    if is_test_decoder {
        println!("Testing Hardware NVENC H.265 (HEVC) -> Media Foundation HEVC Decoder...");
        match nanocord::decoder::HevcDecoder::new() {
            Ok(mut dec) => {
                println!("SUCCESS: HevcDecoder initialized!");
                let width = 1280;
                let height = 720;
                let dummy_rgba = vec![128u8; (width * height * 4) as usize];
                match nanocord::nvenc::NvencEncoder::new_standalone_hevc(
                    width,
                    height,
                    22,
                    30,
                    60,
                    nanocord::nvenc::NV_ENC_PRESET_P1_GUID,
                ) {
                    Ok(mut enc) => {
                        println!("SUCCESS: NvencEncoder HEVC initialized!");
                        for i in 1..=5 {
                            let is_idr = i == 1;
                            match enc.encode_rgba_bytes(&dummy_rgba, is_idr) {
                                Ok(nal) => {
                                    println!("  [Frame {}] NVENC H.265 encoded: {} bytes (is_idr: {})", i, nal.len(), is_idr);
                                    match dec.decode_packet(width, height, &nal) {
                                        Ok(Some((dw, dh, rgba))) => {
                                            println!("  [Frame {}] SUCCESS: Hardware HEVC decoded frame: {}x{}, RGBA: {} bytes!", i, dw, dh, rgba.len());
                                        }
                                        Ok(None) => println!("  [Frame {}] Decoder accepted packet (buffered)", i),
                                        Err(e) => println!("  [Frame {}] Decode error: {:?}", i, e),
                                    }
                                }
                                Err(e) => println!("  [Frame {}] NVENC encode error: {:?}", i, e),
                            }
                        }
                    }
                    Err(e) => println!("ERROR: NVENC HEVC init error: {:?}", e),
                }
            }
            Err(e) => println!("ERROR: Failed to initialize HevcDecoder: {:?}", e),
        }
        return Ok(());
    }

    if let Some(pos) = upnp_arg {
        let port = args
            .get(pos + 1)
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(48120);
        run_upnp_probe(port);
        return Ok(());
    }

    if is_nvenc_probe {
        run_nvenc_probe();
        return Ok(());
    }

    if is_test_mf {
        run_mf_test();
        return Ok(());
    }

    if is_loopback_test {
        run_loopback_test();
        return Ok(());
    }

    if let Some(pos) = listen_arg {
        let port = args
            .get(pos + 1)
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(48120);
        run_listen_test(port);
        return Ok(());
    }

    if is_codec_report {
        run_codec_report();
        return Ok(());
    }

    if is_test_capture {
        use std::io::Write;
        let mut f = std::fs::File::create("debug_capture.txt").unwrap();
        println!("Starting test-capture...");
        writeln!(f, "Starting test-capture...").unwrap();
        f.flush().unwrap();

        let (native_w, native_h) = capture::get_screen_resolution();
        println!("Detected native resolution: {}x{}", native_w, native_h);
        writeln!(f, "Detected native resolution: {}x{}", native_w, native_h).unwrap();
        f.flush().unwrap();

        println!("Testing GDI capture...");
        let gdi = capture::capture_desktop_gdi(native_w, native_h);
        println!("GDI captured: is_some={}, len={:?}", gdi.is_some(), gdi.as_ref().map(|v| v.len()));
        if let Some(ref buf) = gdi {
            let non_zero = buf.iter().filter(|&&b| b != 0).count();
            println!("GDI non-zero bytes count: {} / {}", non_zero, buf.len());
            writeln!(f, "GDI non-zero bytes count: {} / {}", non_zero, buf.len()).unwrap();
        }
        writeln!(f, "GDI is_some: {}, len: {:?}", gdi.is_some(), gdi.as_ref().map(|v| v.len())).unwrap();
        f.flush().unwrap();

        println!("Testing DxgiCapture::new()...");
        match capture::DxgiCapture::new() {
            Ok(mut cap) => {
                println!("DxgiCapture created: {}x{}", cap.width, cap.height);
                writeln!(f, "DxgiCapture size: {}x{}", cap.width, cap.height).unwrap();
                f.flush().unwrap();
                match cap.capture_preview_rgba(native_w, native_h) {
                    Ok(Some(rgba)) => {
                        println!("capture_preview_rgba success! len={}", rgba.len());
                        writeln!(f, "capture_preview_rgba success! len={}", rgba.len()).unwrap();
                    }
                    Ok(None) => {
                        println!("capture_preview_rgba returned None (no frame update)");
                        writeln!(f, "capture_preview_rgba returned None").unwrap();
                    }
                    Err(e) => {
                        println!("capture_preview_rgba error: {:?}", e);
                        writeln!(f, "capture_preview_rgba error: {:?}", e).unwrap();
                    }
                }
                f.flush().unwrap();
            }
            Err(e) => {
                println!("DxgiCapture::new failed: {:?}", e);
                writeln!(f, "DxgiCapture::new failed: {:?}", e).unwrap();
                f.flush().unwrap();
            }
        }
        return Ok(());
    }

    let cq_quality = 22; // Target Quality (18..24)
    let max_bitrate_mbps = 30; // 30 Mbps cap

    if is_test_mode {
        println!("[TEST MODE] Running NVENC test with synthetic 1920x1080 frame...");
        run_synthetic_test(cq_quality, max_bitrate_mbps);
        return Ok(());
    }

    if is_bench_mode {
        println!("[BENCHMARK MODE] Running full hardware latency probe...");
        let results = benchmark::run_full_benchmark();
        let (native_w, native_h) = capture::get_screen_resolution();
        let max_hz = capture::get_screen_refresh_rate();
        let matrix = benchmark::build_benchmark_matrix(&results, native_w, native_h, max_hz);

        let fps_targets = benchmark::compute_fps_targets(max_hz);
        print!("{:<10}", "Resolution");
        for &fps in &fps_targets {
            print!(" | {:<12}", format!("{} FPS", fps));
        }
        println!();
        println!("{:-<70}", "");
        for row in &matrix {
            print!("{:<10}", row.res_label);
            for cell in &row.cells {
                if cell.is_viable {
                    print!(" | {:<12}", format!("{} ({:.1}ms)", cell.best_preset_name, cell.latency_ms));
                } else {
                    print!(" | {:<12}", format!("[Лимит] ({:.1}ms)", cell.latency_ms));
                }
            }
            println!();
        }
        return Ok(());
    }

    if is_cli_mode {
        run_cli_capture(cq_quality, max_bitrate_mbps);
        return Ok(());
    }

    // Launch GUI
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 680.0])
            .with_min_inner_size([900.0, 560.0])
            .with_title("NANOCORD"),
        ..Default::default()
    };

    let res = eframe::run_native(
        "NANOCORD",
        options,
        Box::new(|cc| {
            // Apply sleek dark theme
            ui::apply_custom_theme(&cc.egui_ctx);
            Ok(Box::new(NanocordApp::default()))
        }),
    );
    let _ = std::fs::write("run_result.txt", format!("{:#?}", res));
    if let Err(ref e) = res {
        let _ = std::fs::write("eframe_error.txt", format!("{:#?}", e));
    }
    res
}

/// `--codecs`: отчёт о том, что умеет аппаратный энкодер на этой машине.
///
/// Открывает короткоживущую NVENC-сессию без захвата экрана, поэтому работает и
/// без DXGI Desktop Duplication (например, по SSH/RDP). Результат печатается и
/// дублируется в `codec_report.txt` — у GUI-подсистемы не всегда есть консоль.
fn run_codec_report() {
    let mut lines: Vec<String> = Vec::new();
    lines.push("NVENC codec capability report".to_string());
    lines.push("-".repeat(50));
    // Согласование: что умеем отправлять и что умеем принимать. Кодер
    // поднимается на пересечении с возможностями собеседника.
    lines.push(format!(
        "Encode (ours)   : {}",
        nanocord::encoder::mask_names(nanocord::encoder::encode_caps())
    ));
    lines.push(format!(
        "Decode (ours)   : {}",
        nanocord::encoder::mask_names(nanocord::encoder::decode_caps())
    ));

    match nvenc::probe_available_codecs() {
        Ok(codecs) => {
            if codecs.is_empty() {
                lines.push("NVENC opened a session but reported no supported codecs".to_string());
            } else {
                let names = codecs
                    .iter()
                    .map(|codec| codec.name())
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push(format!("Driver supports : {names}"));
                // Порядок предпочтения API и политика живого стрима — разные вещи,
                // их легко перепутать при чтении отчёта.
                let preferred = nvenc::choose_codec(&codecs, CodecPreference::Auto)
                    .map(|codec| codec.name())
                    .unwrap_or("none");
                lines.push(format!("Preferred codec : {preferred} (explicit API, CodecPreference::Auto)"));
                lines.push(
                    "Stream policy   : AV1 -> HEVC -> H.264, restricted by what the peer decodes"
                        .to_string(),
                );
                if !codecs.contains(&VideoCodec::Hevc) {
                    lines.push(
                        "HEVC is absent  : this GPU has no HEVC encoder, H.264 is the only option"
                            .to_string(),
                    );
                }
            }
        }
        Err(e) => lines.push(format!("NVENC unavailable: {e}")),
    }

    let report = lines.join("\n");
    println!("\n{report}");
    let _ = std::fs::write("codec_report.txt", format!("{report}\n"));
}

/// `--upnp [порт]`: проверка проброса порта через UPnP без запуска стрима.
///
/// Печатает, нашёлся ли шлюз, какой у него внешний адрес и согласился ли он
/// открыть порт. Роутеры отказывают по-разному, и код ошибки UPnP в логе
/// говорит больше, чем «не получилось».
/// `--loopback-test`: полный сеанс в одном процессе — рукопожатие NC03,
/// выбор кодека по маскам, транспорт и декодирование на обеих сторонах.
///
/// Это единственная проверка, которая ловит разрыв именно между частями:
/// энкодер сам по себе (`--test-mf`) может работать, а сеанс — нет.
fn run_loopback_test() {
    use std::time::{Duration, Instant};

    let report = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("loopback_report.txt")))
        .unwrap_or_else(|| std::path::PathBuf::from("loopback_report.txt"));
    let mut lines: Vec<String> = Vec::new();

    let port = 41777u16;
    let code = "LBT".to_string();

    let server = nanocord::network::P2PNetworkServer::start(port, code.clone());
    std::thread::sleep(Duration::from_millis(400));
    let client = nanocord::network::P2PNetworkClient::connect(
        "127.0.0.1".to_string(),
        port,
        code,
    );

    // 1920x1080 — не случайность: Media Foundation выравнивает высоту до 1088,
    // и на этом размере вылезал MF_E_BUFFERTOOSMALL, из-за которого демонстрация
    // экрана не показывала ничего.
    let (width, height) = (1920u32, 1080u32);
    let (cam_w, cam_h) = (1280u32, 720u32);
    let mut frame_a = vec![0u8; (width * height * 4) as usize];
    let mut frame_b = vec![0u8; (width * height * 4) as usize];

    let deadline = Instant::now() + Duration::from_secs(12);
    let mut from_client = 0u32;
    let mut from_server = 0u32;
    let mut cam_from_client = 0u32;
    let mut cam_from_server = 0u32;
    let mut bytes_sent_estimate = 0usize;
    let mut bytes_from_client = 0usize;
    let mut bytes_from_server = 0usize;
    let mut tick = 0u32;

    while Instant::now() < deadline {
        // Кадры сдвигаются: на статичной картинке разница энкодеров не видна.
        for y in 0..height as usize {
            for x in 0..width as usize {
                let p = (y * width as usize + x) * 4;
                let shift = (tick as usize * 17) % width as usize;
                let lit = ((x + shift) / 120) % 2 == 0;
                frame_a[p] = if lit { 240 } else { 30 };
                frame_a[p + 1] = if lit { 190 } else { 40 };
                frame_a[p + 2] = if lit { 90 } else { 60 };
                frame_a[p + 3] = 255;
                let lit_b = ((x + shift * 2) / 200) % 2 == 0;
                frame_b[p] = if lit_b { 60 } else { 200 };
                frame_b[p + 1] = if lit_b { 220 } else { 70 };
                frame_b[p + 2] = if lit_b { 240 } else { 40 };
                frame_b[p + 3] = 255;
            }
        }
        tick += 1;

        client
            .outbox_desktop
            .put(nanocord::network::OutgoingFrame {
                width,
                height,
                rgba: std::sync::Arc::new(frame_a.clone()),
            });
        server
            .outbox_desktop
            .put(nanocord::network::OutgoingFrame {
                width,
                height,
                rgba: std::sync::Arc::new(frame_b.clone()),
            });

        // Камера: тот же кадр, но своего размера — проверяем, что сжатие
        // камеры не мешает демонстрации и наоборот.
        client
            .outbox_camera
            .put(nanocord::network::OutgoingFrame {
                width: cam_w,
                height: cam_h,
                rgba: std::sync::Arc::new(frame_a[..(cam_w * cam_h * 4) as usize].to_vec()),
            });
        server
            .outbox_camera
            .put(nanocord::network::OutgoingFrame {
                width: cam_w,
                height: cam_h,
                rgba: std::sync::Arc::new(frame_b[..(cam_w * cam_h * 4) as usize].to_vec()),
            });

        while let Ok(frame) = server.desktop_rx.try_recv() {
            from_client += 1;
            bytes_from_client += frame.rgba.len();
        }
        while let Ok(frame) = client.desktop_rx.try_recv() {
            from_server += 1;
            bytes_from_server += frame.rgba.len();
        }
        while let Ok(_) = server.camera_rx.try_recv() {
            cam_from_client += 1;
        }
        while let Ok(_) = client.camera_rx.try_recv() {
            cam_from_server += 1;
        }
        bytes_sent_estimate += width as usize * 4;
        std::thread::sleep(Duration::from_millis(40));
    }

    let connected = client.is_connected.load(std::sync::atomic::Ordering::Relaxed)
        && server.is_connected.load(std::sync::atomic::Ordering::Relaxed);
    let client_codec = client.telemetry.read().map(|t| t.codec).unwrap_or(0);
    let server_codec = server.telemetry.read().map(|t| t.codec).unwrap_or(0);

    lines.push(format!("handshake: {}", if connected { "OK" } else { "FAILED" }));
    lines.push(format!(
        "client -> server: {from_client} frames, codec flag {client_codec}"
    ));
    lines.push(format!(
        "server -> client: {from_server} frames, codec flag {server_codec}"
    ));
    lines.push(format!("bytes received: {bytes_from_server} / {bytes_from_client}"));
    lines.push(format!(
        "camera: client->server {cam_from_client} frames, server->client {cam_from_server} frames"
    ));

    client.is_running.store(false, std::sync::atomic::Ordering::Relaxed);
    server.is_running.store(false, std::sync::atomic::Ordering::Relaxed);

    for line in &lines {
        println!("{line}");
    }
    let _ = std::fs::write(&report, format!("{}\n", lines.join("\n")));
}

/// `--listen-test <port>`: слушает порт в этом же процессе, чтобы проверять
/// доступность извне. Отдельный процесс не годится: правило Windows Firewall
/// привязано к nanocord.exe, а не к тому, кто ещё решит послушать порт.
fn run_listen_test(port: u16) {
    use std::io::{Read, Write};
    let report = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("listen_report.txt")))
        .unwrap_or_else(|| std::path::PathBuf::from("listen_report.txt"));

    let listener = match std::net::TcpListener::bind(("0.0.0.0", port)) {
        Ok(listener) => listener,
        Err(e) => {
            let _ = std::fs::write(&report, format!("bind failed: {e}\n"));
            return;
        }
    };
    let _ = std::fs::write(&report, format!("listening on 0.0.0.0:{port}\n"));

    let _ = listener.set_nonblocking(false);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(240);
    while std::time::Instant::now() < deadline {
        listener.set_nonblocking(true).ok();
        match listener.accept() {
            Ok((mut stream, peer)) => {
                let mut buf = [0u8; 128];
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                let read = stream.read(&mut buf).unwrap_or(0);
                let text = String::from_utf8_lossy(&buf[..read]).to_string();
                let _ = stream.write_all(b"nanocord-listen-ok\n");
                let _ = std::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(&report)
                    .and_then(|mut f| {
                        use std::io::Write as _;
                        writeln!(f, "accepted {peer} got={text:?}")
                    });
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(200)),
        }
    }
}

fn run_upnp_probe(port: u16) {
    // Отчёт в файл: у GUI-сборки stdout при перенаправлении теряется, а через
    // SSH видно только файлы.
    let report = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("upnp_report.txt")))
        .unwrap_or_else(|| std::path::PathBuf::from("upnp_report.txt"));
    let mut lines: Vec<String> = Vec::new();
    let mut push = |text: String| {
        println!("{text}");
        lines.push(text);
    };

    push(format!("UPnP check for TCP port {port}"));
    match nanocord::upnp::local_ipv4() {
        Some(ip) => push(format!("  local IPv4    : {ip}")),
        None => push("  local IPv4    : не определён".to_string()),
    }

    match nanocord::upnp::discover(std::time::Duration::from_secs(4)) {
        Ok(gateway) => {
            push(format!("  gateway       : {}", gateway.control_url));
            push(format!("  service       : {}", gateway.service_type));
            match nanocord::upnp::external_ip(&gateway, std::time::Duration::from_secs(4)) {
                Ok(ip) => push(format!("  external IPv4 : {ip}")),
                Err(e) => push(format!("  external IPv4 : {e}")),
            }
        }
        Err(e) => {
            push(format!("  gateway       : {e}"));
            let _ = std::fs::write(&report, format!("{}\n", lines.join("\n")));
            return;
        }
    }

    match nanocord::upnp::ensure_mapping(port, "nanocord") {
        Ok(mapping) => {
            push(format!(
                "  mapping       : TCP {} -> {}:{} OK",
                mapping.external_port,
                nanocord::upnp::local_ipv4()
                    .map(|ip| ip.to_string())
                    .unwrap_or_else(|| "?".to_string()),
                mapping.internal_port
            ));
            if mapping.external_ip != "?" {
                push(format!(
                    "  connect with  : {}:{}",
                    mapping.external_ip, mapping.external_port
                ));
            }
        }
        Err(e) => push(format!("  mapping       : {e}")),
    }

    let _ = std::fs::write(&report, format!("{}\n", lines.join("\n")));
}

/// `--test-mf`: кодирование через Media Foundation и контрольная расшифровка
/// тем же MF-декодером, что используется на приёмной стороне.
fn run_mf_test() {
    let mut lines = Vec::new();
    // Отчёт пишем рядом с exe: у задачи планировщика рабочий каталог может
    // быть другим, и относительный путь уводил файл в неизвестном направлении.
    let report = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("mf_test.txt")))
        .unwrap_or_else(|| std::path::PathBuf::from("mf_test.txt"));
    let (width, height) = (1280u32, 720u32);
    let push = |lines: &mut Vec<String>, text: String| {
        println!("{text}");
        lines.push(text);
    };

    // Кадр с цветными полосами: по нему видно и «позеленело ли», и поехали ли строки.
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for y in 0..height as usize {
        for x in 0..width as usize {
            let p = (y * width as usize + x) * 4;
            let band = x * 8 / width as usize;
            rgba[p] = if band % 3 == 0 { 255 } else { 40 };
            rgba[p + 1] = if band % 3 == 1 { 255 } else { 40 };
            rgba[p + 2] = if band % 3 == 2 { 255 } else { 40 };
            rgba[p + 3] = 255;
        }
    }

    // Тот же выбор, что в живом стриме: NVENC -> NVENC HEVC -> Media Foundation.
    let mut enc = match nanocord::encoder::DesktopEncoder::create(
        width,
        height,
        22,
        8,
        30,
        nanocord::encoder::preset_by_index(0),
        0b111, // локальный тест: считаем, что собеседник понимает всё
    ) {
        Some(enc) => enc,
        None => {
            push(&mut lines, "ERROR: no encoder available".to_string());
            if let Err(err) = std::fs::write(&report, format!("{}\n", lines.join("\n"))) {
                println!("write failed: {err}");
            }
            return;
        }
    };
    push(&mut lines, format!("encoder: {}", enc.label()));

    // Декодер — под фактически выбранный кодек: HEVC, разобранный H.264-декодером,
    // молча остаётся «buffered» и выглядит как отсутствие картинки.
    let dec_subtype = match enc.codec() {
        nanocord::nvenc::VideoCodec::Av1 => {
            windows::Win32::Media::MediaFoundation::MFVideoFormat_AV1
        }
        nanocord::nvenc::VideoCodec::Hevc => {
            windows::Win32::Media::MediaFoundation::MFVideoFormat_HEVC
        }
        nanocord::nvenc::VideoCodec::H264 => {
            windows::Win32::Media::MediaFoundation::MFVideoFormat_H264
        }
    };
    let mut dec = nanocord::decoder::H264Decoder::with_subtype(dec_subtype).ok();
    nanocord::diag::log(&format!("mf test: report path {}", report.display()));
    let flush = |lines: &Vec<String>| {
        // Пишем после каждого кадра: если декодер упадёт на живом потоке,
        // отчёт всё равно останется с данными до места падения.
        let _ = std::fs::write(&report, format!("{}\n", lines.join("\n")));
    };

    for frame in 1..=30u32 {
        // Кадр сдвигается: на статичной картинке P-кадры получаются по 19 байт
        // и по ним не понять, живой ли энкодер.
        for y in 0..height as usize {
            for x in 0..width as usize {
                let p = (y * width as usize + x) * 4;
                let shift = (frame as usize * 40) % width as usize;
                let lit = ((x + shift) / 160) % 2 == 0;
                rgba[p] = if lit { 235 } else { 20 };
                rgba[p + 1] = if lit { 200 } else { 30 };
                rgba[p + 2] = if lit { 120 } else { 60 };
            }
        }
        match enc.encode_rgba_bytes(&rgba, frame == 1) {
            Ok(nal) => {
                push(
                    &mut lines,
                    format!("frame {frame}: {} bytes, first4={:02X?}", nal.len(), &nal[..4.min(nal.len())]),
                );
                if let Some(dec) = dec.as_mut() {
                    match dec.decode_packet(width, height, &nal) {
                        Ok(Some((w, h, out))) => push(
                            &mut lines,
                            format!("  decoded {w}x{h} {} bytes", out.len()),
                        ),
                        Ok(None) => push(&mut lines, "  decoded: buffered".to_string()),
                        Err(e) => push(&mut lines, format!("  decode error: {e:?}")),
                    }
                }
            }
            Err(e) => push(&mut lines, format!("frame {frame}: encode error {e:?}")),
        }
        flush(&lines);
    }
    if let Err(e) = std::fs::write(&report, format!("{}\n", lines.join("\n"))) {
        println!("write failed: {e}");
    }
}

/// `--nvenc-probe`: какая версия NVENC API принимается драйвером.
fn run_nvenc_probe() {
    // Пишем в файл: у GUI-сборки stdout при перенаправлении теряется.
    let mut lines = vec!["NVENC API version probe".to_string()];
    for (api, struct_ver, status) in nanocord::nvenc::probe_api_versions() {
        let verdict = if status == 0 { "OK" } else { "rejected" };
        lines.push(format!(
            "  apiVersion {api:#010X} struct {struct_ver:#010X} -> status {status} {verdict}"
        ));
    }
    let text = lines.join("\n");
    println!("{text}");
    let _ = std::fs::write("nvenc_probe.txt", format!("{text}\n"));
}

fn attach_console_if_cli() {
    unsafe {
        type AttachConsoleFn = unsafe extern "system" fn(u32) -> i32;
        let k32_name: Vec<u16> = "kernel32.dll\0".encode_utf16().collect();
        if let Ok(h) = windows::Win32::System::LibraryLoader::LoadLibraryW(windows::core::PCWSTR(k32_name.as_ptr())) {
            if let Some(proc) = windows::Win32::System::LibraryLoader::GetProcAddress(
                h,
                windows::core::PCSTR(b"AttachConsole\0".as_ptr()),
            ) {
                let attach: AttachConsoleFn = std::mem::transmute(proc);
                const ATTACH_PARENT_PROCESS: u32 = 0xFFFFFFFF;
                let _ = attach(ATTACH_PARENT_PROCESS);
            }
        }
    }
}

fn run_cli_capture(cq_quality: u8, max_bitrate_mbps: u32) {
    println!("[1/2] Initializing DXGI Desktop Duplication...");
    let mut capture = match DxgiCapture::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to initialize DXGI capture: {:?}", e);
            eprintln!("\n(Note: DXGI Desktop Duplication requires an interactive Windows user session.");
            eprintln!(" Run `nanocord.exe --test` to test NVENC hardware encoding directly).");
            return;
        }
    };
    println!(
        "       -> Desktop Resolution: {}x{}",
        capture.width, capture.height
    );

    println!("[2/2] Initializing NVENC CQ-VBR hardware encoder (HEVC if supported, else H.264)...");
    let mut encoder = match NvencEncoder::new(
        &capture.device,
        capture.width,
        capture.height,
        cq_quality,
        max_bitrate_mbps,
    ) {
        Ok(enc) => enc,
        Err(e) => {
            eprintln!("Failed to initialize NVENC: {:?}", e);
            return;
        }
    };
    println!("       -> Active codec: {}", encoder.codec().name());

    println!("\nCapturing and encoding live desktop frames...");

    for i in 1..=5 {
        match capture.acquire_next_frame(100) {
            Ok(Some((texture, _frame_info))) => {
                let is_keyframe = i == 1;
                match encoder.encode_texture(&texture, &capture.context, is_keyframe) {
                    Ok(nal_bytes) => {
                        println!(
                            "  [Frame #{:02}] Encoded {} size: {} bytes ({})",
                            i,
                            encoder.codec().short_name(),
                            nal_bytes.len(),
                            if is_keyframe { "Keyframe / IDR" } else { "P-frame" }
                        );
                    }
                    Err(e) => {
                        eprintln!("  [Frame #{:02}] Encode error: {:?}", i, e);
                    }
                }
                let _ = capture.release_frame();
            }
            Ok(None) => {
                println!("  [Frame #{:02}] Screen unchanged (skipped)", i);
            }
            Err(e) => {
                eprintln!("  [Frame #{:02}] Capture error: {:?}", i, e);
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(16)); // ~60 FPS
    }

    println!("\nLive capture pipeline test complete successfully!");
}

fn run_synthetic_test(cq_quality: u8, max_bitrate_mbps: u32) {
    let width = 1920;
    let height = 1080;

    let mut dev = None;
    let mut ctx = None;
    let mut fl = windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;

    unsafe {
        windows::Win32::Graphics::Direct3D11::D3D11CreateDevice(
            None,
            windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE,
            windows::Win32::Foundation::HMODULE::default(),
            windows::Win32::Graphics::Direct3D11::D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0]),
            windows::Win32::Graphics::Direct3D11::D3D11_SDK_VERSION,
            Some(&mut dev),
            Some(&mut fl),
            Some(&mut ctx),
        ).expect("Failed to create dummy D3D11 device");
    }

    let dev = dev.unwrap();
    let ctx = ctx.unwrap();

    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };

    let mut tex = None;
    unsafe {
        dev.CreateTexture2D(&desc, None, Some(&mut tex)).expect("CreateTexture2D failed");
    }
    let tex = tex.unwrap();

    let mut encoder = match NvencEncoder::new(&dev, width, height, cq_quality, max_bitrate_mbps) {
        Ok(encoder) => encoder,
        Err(e) => {
            // Паника тут оставляла пустой экран и один panic.txt без причины.
            println!("ERROR: NVENC init failed: {e:?}");
            nanocord::diag::log(&format!("nvenc test: init failed {e:?}"));
            return;
        }
    };

    println!("\nEncoding 5 test frames...");
    for i in 1..=5 {
        let is_keyframe = i == 1;
        let nal_bytes = encoder.encode_texture(&tex, &ctx, is_keyframe).expect("Encoding failed");
        println!(
            "  [Frame #{:02}] Encoded {} size: {} bytes ({})",
            i,
            encoder.codec().short_name(),
            nal_bytes.len(),
            if is_keyframe { "Keyframe / IDR" } else { "P-frame" }
        );
    }
    println!("\nSynthetic test complete!");
}
