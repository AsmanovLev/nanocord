#![allow(dead_code, unused_variables, unused_imports)]

use crate::capture::get_screen_resolution;
use crate::nvenc::{
    NvencEncoder, NV_ENC_PRESET_P1_GUID, NV_ENC_PRESET_P3_GUID, NV_ENC_PRESET_P4_GUID,
    NV_ENC_PRESET_P5_GUID, NV_ENC_PRESET_P6_GUID, NV_ENC_PRESET_P7_GUID,
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
pub struct MatrixCell {
    pub res_width: u32,
    pub res_height: u32,
    pub res_label: String,
    pub fps: u32,
    pub best_preset_id: u8,
    pub best_preset_name: String,
    pub latency_ms: f32,
    pub budget_ms: f32,
    pub headroom_percent: f32,
    pub is_viable: bool,
}

#[derive(Clone, Debug)]
pub struct MatrixRow {
    pub res_label: String,
    pub width: u32,
    pub height: u32,
    pub cells: Vec<MatrixCell>,
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
        (3840, 2160, "4K UHD"),
    ];

    let valid_resolutions: Vec<(u32, u32, &str)> = all_resolutions
        .into_iter()
        .filter(|&(w, h, _)| w <= native_w && h <= native_h)
        .collect();

    let presets = [
        (NV_ENC_PRESET_P1_GUID, "P1", 1u8),
        (NV_ENC_PRESET_P3_GUID, "P3", 3u8),
        (NV_ENC_PRESET_P4_GUID, "P4", 4u8),
        (NV_ENC_PRESET_P5_GUID, "P5", 5u8),
        (NV_ENC_PRESET_P6_GUID, "P6", 6u8),
        (NV_ENC_PRESET_P7_GUID, "P7", 7u8),
    ];

    // Dummy D3D11 Device for testing
    let mut dev = None;
    let mut ctx = None;
    let mut fl = windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;

    unsafe {
        if windows::Win32::Graphics::Direct3D11::D3D11CreateDevice(
            None,
            windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE,
            windows::Win32::Foundation::HMODULE::default(),
            windows::Win32::Graphics::Direct3D11::D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0]),
            windows::Win32::Graphics::Direct3D11::D3D11_SDK_VERSION,
            Some(&mut dev),
            Some(&mut fl),
            Some(&mut ctx),
        ).is_err() {
            return results;
        }
    }

    let dev = match dev {
        Some(d) => d,
        None => return results,
    };
    let ctx = match ctx {
        Some(c) => c,
        None => return results,
    };

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
                // Прогрев (warmup) 2 кадра
                for _ in 0..2 {
                    let _ = encoder.encode_texture(&tex, &ctx, false);
                }

                let mut times = Vec::new();
                let mut idr_size = 0;
                let mut p_size = 0;

                // Замеряем 15 кадров
                for i in 0..15 {
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

/// Вычисляет столбцы FPS на основе формулы: [24, 60, min(max_fps, prev*2)]
pub fn compute_fps_targets(max_screen_fps: u32) -> Vec<u32> {
    let mut targets = vec![24, 60];
    let mut prev = 60;
    while prev < max_screen_fps {
        let next = (prev * 2).min(max_screen_fps);
        if next > prev && !targets.contains(&next) {
            targets.push(next);
            prev = next;
        } else {
            break;
        }
    }
    targets
}

/// Строит лаконичную матрицу профилей Resolution × FPS Target
pub fn build_benchmark_matrix(
    results: &[BenchmarkResult],
    native_w: u32,
    native_h: u32,
    max_screen_fps: u32,
) -> Vec<MatrixRow> {
    let fps_columns = compute_fps_targets(max_screen_fps);

    let all_resolutions = [
        (854, 480, "480p"),
        (1280, 720, "720p"),
        (1920, 1080, "1080p"),
        (2560, 1440, "1440p"),
        (3840, 2160, "4K"),
    ];

    let valid_resolutions: Vec<(u32, u32, &str)> = all_resolutions
        .into_iter()
        .filter(|&(w, h, _)| w <= native_w && h <= native_h)
        .collect();

    let mut rows = Vec::new();

    for &(w, h, res_label) in &valid_resolutions {
        let mut cells = Vec::new();

        for &fps in &fps_columns {
            let budget = 1000.0 / fps as f32;

            // Ищем лучший пресет для данного разрешения и FPS
            // Сначала пробуем те, что укладываются в 75% бюджета (с запасом): P4 -> P3 -> P1
            let candidates: Vec<&BenchmarkResult> = results.iter()
                .filter(|r| r.width == w)
                .collect();

            // Ищем с запасом
            let safe_pick = candidates.iter()
                .filter(|r| r.avg_latency_ms <= budget * 0.80)
                .max_by_key(|r| r.preset_id);

            // Если с запасом нет, пробуем хотя бы впритык (<= 100% бюджета)
            let tight_pick = candidates.iter()
                .filter(|r| r.avg_latency_ms <= budget)
                .min_by(|a, b| a.avg_latency_ms.partial_cmp(&b.avg_latency_ms).unwrap());

            let chosen = safe_pick.or(tight_pick);

            let cell = if let Some(&r) = chosen {
                let headroom = ((budget - r.avg_latency_ms) / budget) * 100.0;
                MatrixCell {
                    res_width: w,
                    res_height: h,
                    res_label: res_label.to_string(),
                    fps,
                    best_preset_id: r.preset_id,
                    best_preset_name: format!("P{}", r.preset_id),
                    latency_ms: r.avg_latency_ms,
                    budget_ms: budget,
                    headroom_percent: headroom,
                    is_viable: true,
                }
            } else {
                // Превышает бюджет даже на P1
                let fastest_p1 = candidates.iter().find(|r| r.preset_id == 1);
                let lat = fastest_p1.map(|r| r.avg_latency_ms).unwrap_or(15.0);
                MatrixCell {
                    res_width: w,
                    res_height: h,
                    res_label: res_label.to_string(),
                    fps,
                    best_preset_id: 1,
                    best_preset_name: "P1".to_string(),
                    latency_ms: lat,
                    budget_ms: budget,
                    headroom_percent: ((budget - lat) / budget) * 100.0,
                    is_viable: false,
                }
            };

            cells.push(cell);
        }

        rows.push(MatrixRow {
            res_label: res_label.to_string(),
            width: w,
            height: h,
            cells,
        });
    }

    rows
}
