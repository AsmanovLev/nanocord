mod benchmark;
mod capture;
mod nvenc;
mod ui;

use capture::DxgiCapture;
use nvenc::NvencEncoder;
use ui::NanocordApp;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let is_test_mode = args.iter().any(|a| a == "--test");
    let is_bench_mode = args.iter().any(|a| a == "--benchmark");
    let is_cli_mode = args.iter().any(|a| a == "--cli");

    println!("=================================================");
    println!("   NANOCORD - Ultra-Hardcoded Native Streamer    ");
    println!("=================================================");

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
        for r in &results {
            println!(
                "  {} | {}: Avg Latency: {:.2} ms | Min: {:.2} ms | Max FPS: {} | IDR: {:.1} KB, P: {:.1} KB",
                r.resolution, r.preset_name, r.avg_latency_ms, r.min_latency_ms, r.max_fps, r.idr_size_kb, r.p_frame_size_kb
            );
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
            .with_inner_size([880.0, 560.0])
            .with_min_inner_size([720.0, 480.0])
            .with_title("NANOCORD - Ultra-Low Latency Streamer"),
        ..Default::default()
    };

    eframe::run_native(
        "NANOCORD",
        options,
        Box::new(|_cc| Ok(Box::new(NanocordApp::default()))),
    )
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

    // Создаем dummy устройство для захвата
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
