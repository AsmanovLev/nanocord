//! Самопроверка видео-тракта целиком на машине пользователя.
//!
//! Захват → NVENC → декодер Media Foundation → сравнение с исходником.
//! Это тот же путь, которым идёт демонстрация, только без второго
//! собеседника и без сети: если картинка портится здесь, виноват кодек,
//! а если нет — транспорт или приёмная сторона.

pub fn run_video_roundtrip() -> Result<String, String> {
    let (w, h) = (1280u32, 720u32);

    let rgba = crate::capture::capture_desktop_gdi(w, h)
        .ok_or_else(|| "GDI-захват не вернул кадр".to_string())?;
    crate::diag::dump_frame("diag_src", w, h, &rgba);

    let mut enc = crate::nvenc::NvencEncoder::new_standalone_h264(
        w,
        h,
        22,
        30,
        30,
        crate::nvenc::NV_ENC_PRESET_P3_GUID,
    )
    .map_err(|e| format!("NVENC init {:#010X}", e.code().0))?;

    // NVENC отдаёт данные не с первого кадра: сначала копит IDR.
    let mut nal = Vec::new();
    for _ in 0..4 {
        match enc.encode_rgba_bytes(&rgba, true) {
            Ok(bytes) => {
                if !bytes.is_empty() {
                    nal = bytes;
                    break;
                }
            }
            Err(e) => return Err(format!("encode {:#010X}", e.code().0)),
        }
    }
    if nal.is_empty() {
        return Err("энкодер не выдал данные".to_string());
    }

    let mut dec = crate::decoder::H264Decoder::new()
        .map_err(|e| format!("декодер init {:#010X}", e.code().0))?;

    let mut decoded = None;
    for _ in 0..10 {
        match dec.decode_packet(w, h, &nal) {
            Ok(Some((_, _, frame))) => {
                decoded = Some(frame);
                break;
            }
            Ok(None) => continue,
            Err(e) => return Err(format!("decode {:#010X}", e.code().0)),
        }
    }
    let decoded = decoded.ok_or_else(|| "декодер не вернул кадр".to_string())?;

    crate::diag::dump_frame("diag_roundtrip", w, h, &decoded);

    let pixels = (w as usize * h as usize * 4).min(decoded.len()).min(rgba.len());
    if pixels == 0 {
        return Err("пустой кадр".to_string());
    }
    let mut sum = 0u64;
    for i in 0..pixels {
        sum += (rgba[i] as i32 - decoded[i] as i32).unsigned_abs() as u64;
    }
    let avg = sum as f64 / pixels as f64;

    // Сжатие H.264 при cq=22 даёт единицы; диагональные полосы дают десятки.
    let verdict = if avg < 12.0 { "OK" } else { "ПОДОЗРИТЕЛЬНО" };

    Ok(format!(
        "{}: {}x{} avg_diff={:.2}/255 ({}), кадры diag_src.bmp / diag_roundtrip.bmp",
        verdict, w, h, avg, nal.len()
    ))
}
