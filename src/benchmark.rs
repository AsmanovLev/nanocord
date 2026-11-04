use crate::nvenc::{
    NvencEncoder, NV_ENC_PRESET_P1_GUID, NV_ENC_PRESET_P3_GUID,
};
use windows::core::GUID;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    D3D11_BIND_RENDER_TARGET, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

#[derive(Clone, Debug)]
pub struct BenchmarkResult {
    pub resolution: String,
    pub width: u32,
    pub height: u32,
    pub preset_name: String,
    pub avg_latency_ms: f32,
    pub min_latency_ms: f32,
    pub max_fps: u32,
    pub idr_size_kb: f32,
    pub p_frame_size_kb: f32,
}

pub fn run_full_benchmark() -> Vec<BenchmarkResult> {
    let mut results = Vec::new();

    let resolutions = [
        (1280, 720, "720p HD"),
        (1920, 1080, "1080p Full HD"),
        (2560, 1440, "1440p 2K"),
    ];

    let presets = [
        (NV_ENC_PRESET_P1_GUID, "P1 (Ultra-Low Latency)"),
        (NV_ENC_PRESET_P3_GUID, "P3 (Balanced Quality)"),
    ];

    // Dummy D3D11 Device for testing
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
        ).expect("Failed to create dummy D3D11 device for benchmark");
    }

    let dev = dev.unwrap();
    let ctx = ctx.unwrap();

    for &(w, h, res_label) in &resolutions {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
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
            if dev.CreateTexture2D(&desc, None, Some(&mut tex)).is_err() {
                continue;
            }
        }
        let tex = tex.unwrap();

        for &(preset_guid, preset_name) in &presets {
            if let Ok(mut encoder) = NvencEncoder::new_with_preset(&dev, w, h, 22, 30, preset_guid) {
                // Прогрев (warmup) 3 кадра
                for _ in 0..3 {
                    let _ = encoder.encode_texture(&tex, &ctx, false);
                }

                let mut times = Vec::new();
                let mut idr_size = 0;
                let mut p_size = 0;

                // Замеряем 20 кадров
                for i in 0..20 {
                    let is_idr = i == 0;
                    let t0 = std::time::Instant::now();
                    if let Ok(nal) = encoder.encode_texture(&tex, &ctx, is_idr) {
                        let elapsed = t0.elapsed().as_secs_f32() * 1000.0;
                        times.push(elapsed);
                        if is_idr {
                            idr_size = nal.len();
                        } else {
                            p_size = nal.len();
                        }
                    }
                }

                if !times.is_empty() {
                    let avg_ms = times.iter().sum::<f32>() / times.len() as f32;
                    let min_ms = times.iter().copied().fold(f32::INFINITY, f32::min);
                    let max_fps = if avg_ms > 0.0 { (1000.0 / avg_ms) as u32 } else { 0 };

                    results.push(BenchmarkResult {
                        resolution: res_label.to_string(),
                        width: w,
                        height: h,
                        preset_name: preset_name.to_string(),
                        avg_latency_ms: avg_ms,
                        min_latency_ms: min_ms,
                        max_fps,
                        idr_size_kb: idr_size as f32 / 1024.0,
                        p_frame_size_kb: p_size as f32 / 1024.0,
                    });
                }
            }
        }
    }

    results
}
