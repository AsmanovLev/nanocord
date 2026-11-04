#![allow(dead_code, unused_variables, unused_imports)]

use crate::capture::get_screen_resolution;
use crate::nvenc::{
    NvencEncoder, NV_ENC_PRESET_P1_GUID, NV_ENC_PRESET_P3_GUID, NV_ENC_PRESET_P4_GUID,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

#[derive(Clone, Debug)]
pub struct BenchmarkResult {
    pub resolution: String,
    pub width: u32,
    pub height: u32,
    pub preset_name: String,
    pub preset_id: u8,
    pub avg_latency_ms: f32,
    pub min_latency_ms: f32,
    pub max_fps: u32,
    pub idr_size_kb: f32,
    pub p_frame_size_kb: f32,
}

#[derive(Clone, Debug)]
pub struct ProfileRecommendation {
    pub title: String,
    pub target_fps: u32,
    pub width: u32,
    pub height: u32,
    pub res_label: String,
    pub preset_name: String,
    pub preset_id: u8,
    pub latency_ms: f32,
    pub frame_budget_ms: f32,
    pub headroom_percent: f32,
    pub description: String,
}

pub fn run_full_benchmark() -> Vec<BenchmarkResult> {
    let mut results = Vec::new();

    // 1. Получаем физическое разрешение основного монитора
    let (native_w, native_h) = get_screen_resolution();

    // 2. Тестируем только те разрешения, которые не превышают физический экран!
    let all_resolutions = [
        (854, 480, "480p SD"),
        (1280, 720, "720p HD"),
        (1920, 1080, "1080p Full HD"),
        (2560, 1440, "1440p 2K"),
    ];

    let valid_resolutions: Vec<(u32, u32, &str)> = all_resolutions
        .into_iter()
        .filter(|&(w, h, _)| w <= native_w && h <= native_h)
        .collect();

    let presets = [
        (NV_ENC_PRESET_P1_GUID, "P1 (Ultra-Low Latency)", 1u8),
        (NV_ENC_PRESET_P3_GUID, "P3 (Balanced Quality)", 3u8),
        (NV_ENC_PRESET_P4_GUID, "P4 (High Quality)", 4u8),
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

    for &(w, h, res_label) in &valid_resolutions {
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

        for &(preset_guid, preset_name, preset_id) in &presets {
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
                        preset_id,
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

/// Вычисляет оптимальные профили для различных целевых FPS
pub fn generate_recommendations(results: &[BenchmarkResult]) -> Vec<ProfileRecommendation> {
    let targets = [
        (24, "24 FPS (Cinema / Desktop)"),
        (60, "60 FPS (Balanced Sweet-Spot)"),
        (60, "60 FPS (Ultra-Low Latency)"),
        (120, "120 FPS (High Refresh Rate)"),
        (144, "144+ FPS (eSports Extreme)"),
    ];

    let mut recs = Vec::new();

    for &(fps, title) in &targets {
        let budget = 1000.0 / fps as f32;

        let best_match = match (fps, title) {
            (24, _) => {
                // Ищем самое высокое разрешение с максимальным качеством (P4 или P3)
                results.iter()
                    .filter(|r| r.avg_latency_ms <= budget * 0.75)
                    .max_by(|a, b| (a.width, a.preset_id).cmp(&(b.width, b.preset_id)))
            }
            (60, "60 FPS (Balanced Sweet-Spot)") => {
                // Ищем 1080p (или наивысшее доступное) с пресетом P3
                results.iter()
                    .filter(|r| r.preset_id == 3 && r.avg_latency_ms <= budget)
                    .max_by_key(|r| r.width)
                    .or_else(|| {
                        results.iter()
                            .filter(|r| r.avg_latency_ms <= budget)
                            .max_by_key(|r| (r.width, r.preset_id))
                    })
            }
            (60, "60 FPS (Ultra-Low Latency)") => {
                // Ищем пресет P1 с максимальным запасом для тяжелых игр
                results.iter()
                    .filter(|r| r.preset_id == 1 && r.avg_latency_ms <= budget * 0.65)
                    .max_by_key(|r| r.width)
            }
            (120, _) => {
                // Ищем профиль, который укладывается в 8.33 мс (обычно 720p P1)
                results.iter()
                    .filter(|r| r.avg_latency_ms <= budget * 0.85)
                    .max_by_key(|r| (r.width, r.preset_id))
            }
            (144, _) => {
                // Самая минимальная задержка (720p P1 или 480p P1)
                results.iter()
                    .filter(|r| r.preset_id == 1)
                    .min_by(|a, b| a.avg_latency_ms.partial_cmp(&b.avg_latency_ms).unwrap())
            }
            _ => None,
        };

        if let Some(r) = best_match {
            let headroom = ((budget - r.avg_latency_ms) / budget) * 100.0;
            let desc = match fps {
                24 => "Максимум четкости для текста и рабочего стола. Задержка не ощущается.",
                60 if title.contains("Sweet-Spot") => "Оптимальное сочетание качества графики P3 и плавной картинки 60 к/с.",
                60 => "Сверхбыстрый пресет P1. Огромный запас для игр со 100% загрузкой GPU.",
                120 => "Высокая герцовка. Задержка строго ниже 8.3 мс для киберспортивных мониторов.",
                _ => "Экстремально быстрый захват и энкод с откликом <5 мс.",
            };

            recs.push(ProfileRecommendation {
                title: title.to_string(),
                target_fps: fps,
                width: r.width,
                height: r.height,
                res_label: r.resolution.clone(),
                preset_name: r.preset_name.clone(),
                preset_id: r.preset_id,
                latency_ms: r.avg_latency_ms,
                frame_budget_ms: budget,
                headroom_percent: headroom,
                description: desc.to_string(),
            });
        }
    }

    recs
}
