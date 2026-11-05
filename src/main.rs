#![allow(dead_code, unused_variables, unused_imports)]
#![windows_subsystem = "windows"]

use nanocord::audio;
use nanocord::benchmark;
use nanocord::camera;
use nanocord::capture::{self, DxgiCapture};
use nanocord::network;
use nanocord::nvenc::{self, NvencEncoder};
use nanocord::ui::{self, NanocordApp};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

fn main() -> eframe::Result<()> {
    std::panic::set_hook(Box::new(|info| {
        let _ = std::fs::write("panic.txt", format!("{:#?}", info));
    }));
    let args: Vec<String> = std::env::args().collect();
    let is_test_mode = args.iter().any(|a| a == "--test");
    let is_bench_mode = args.iter().any(|a| a == "--benchmark");
    let is_cli_mode = args.iter().any(|a| a == "--cli");
    let is_test_capture = args.iter().any(|a| a == "--test-capture");

    if is_test_mode || is_bench_mode || is_cli_mode || is_test_capture {
        attach_console_if_cli();
        println!("=================================================");
        println!("   NANOCORD - Ultra-Hardcoded Native Streamer    ");
        println!("=================================================");
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

    println!("[2/2] Initializing NVENC H.265 (HEVC) CQ-VBR Hardware Encoder...");
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

    println!("\nCapturing and encoding live desktop frames...");

    for i in 1..=5 {
        match capture.acquire_next_frame(100) {
            Ok(Some((texture, _frame_info))) => {
                let is_keyframe = i == 1;
                match encoder.encode_texture(&texture, &capture.context, is_keyframe) {
                    Ok(nal_bytes) => {
                        println!(
                            "  [Frame #{:02}] Encoded HEVC size: {} bytes ({})",
                            i,
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

    let mut encoder = NvencEncoder::new(&dev, width, height, cq_quality, max_bitrate_mbps)
        .expect("Failed to initialize NvencEncoder");

    println!("\nEncoding 5 test frames...");
    for i in 1..=5 {
        let is_keyframe = i == 1;
        let nal_bytes = encoder.encode_texture(&tex, &ctx, is_keyframe).expect("Encoding failed");
        println!(
            "  [Frame #{:02}] Encoded HEVC size: {} bytes ({})",
            i,
            nal_bytes.len(),
            if is_keyframe { "Keyframe / IDR" } else { "P-frame" }
        );
    }
    println!("\nSynthetic test complete!");
}
