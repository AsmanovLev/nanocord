//! Единая точка выбора видеокодера для демонстрации экрана.
//!
//! Порядок предпочтений: NVENC H.264 -> NVENC HEVC -> Media Foundation H.264.
//! Если NVENC не открывается (бывает на гибридных ноутбуках и на старых
//! драйверах), MF-путь всё равно даёт H.264, который декодируется везде.
//! Без такого резерва приложение оставалось без видео вообще.

use windows::core::Result;

use crate::mfenc::MfH264Encoder;
use crate::nvenc::{NvencEncoder, NV_ENC_PRESET_P1_GUID};
use windows::core::GUID;

/// Флаг кодека в телеметрии: 2 — H.264, 3 — HEVC.
pub const CODEC_H264: u8 = 2;
pub const CODEC_HEVC: u8 = 3;

pub enum DesktopEncoder {
    Nvenc(NvencEncoder, u8),
    MediaFoundation(MfH264Encoder),
}

impl DesktopEncoder {
    /// Создаёт лучший доступный кодер для заданного размера кадра.
    pub fn create(
        width: u32,
        height: u32,
        cq: u8,
        max_bitrate_mbps: u32,
        fps: u32,
        preset: GUID,
    ) -> Option<Self> {
        // H.264 первым: он декодируется на любом железе, HEVC — только резерв.
        match NvencEncoder::new_standalone_h264(
            width,
            height,
            cq,
            max_bitrate_mbps,
            fps,
            preset,
        ) {
            Ok(encoder) => {
                crate::diag::log(&format!("encoder: H264(NVENC) {width}x{height} fps={fps}"));
                return Some(Self::Nvenc(encoder, CODEC_H264));
            }
            Err(e) => crate::diag::log(&format!(
                "encoder: NVENC H.264 unavailable ({})",
                e.message()
            )),
        }

        match NvencEncoder::new_standalone_hevc(
            width,
            height,
            cq,
            max_bitrate_mbps,
            fps,
            preset,
        ) {
            Ok(encoder) => {
                crate::diag::log(&format!("encoder: HEVC(NVENC) {width}x{height} fps={fps}"));
                return Some(Self::Nvenc(encoder, CODEC_HEVC));
            }
            Err(e) => crate::diag::log(&format!(
                "encoder: NVENC HEVC unavailable ({})",
                e.message()
            )),
        }

        // Битрейт из «cq» не выводится напрямую: для MF берём верхнюю границу.
        let bitrate = (max_bitrate_mbps.max(4) * 1_000_000).min(40_000_000);
        match MfH264Encoder::new(width, height, fps.max(1), bitrate) {
            Ok(encoder) => {
                crate::diag::log(&format!(
                    "encoder: H264(MediaFoundation {}={}) {width}x{height} fps={fps} {}bps",
                    "mft",
                    encoder.label(),
                    bitrate
                ));
                Some(Self::MediaFoundation(encoder))
            }
            Err(e) => {
                crate::diag::log(&format!("encoder: no encoder available ({e:?})"));
                None
            }
        }
    }

    pub fn encode_rgba_bytes(&mut self, rgba: &[u8], force_idr: bool) -> Result<Vec<u8>> {
        match self {
            Self::Nvenc(encoder, _) => encoder.encode_rgba_bytes(rgba, force_idr),
            Self::MediaFoundation(encoder) => encoder.encode_rgba_bytes(rgba, force_idr),
        }
    }

    /// Значение флага кодека для телеметрии и протокола.
    pub fn codec_flag(&self) -> u8 {
        match self {
            Self::Nvenc(_, flag) => *flag,
            Self::MediaFoundation(encoder) => encoder.codec_flag(),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Nvenc(_, CODEC_HEVC) => "HEVC(NVENC)",
            Self::Nvenc(_, _) => "H264(NVENC)",
            Self::MediaFoundation(_) => "H264(MF)",
        }
    }
}

/// Пресет по индексу настройки — один для всех путей.
pub fn preset_by_index(index: usize) -> GUID {
    match index {
        0 => NV_ENC_PRESET_P1_GUID,
        1 => crate::nvenc::NV_ENC_PRESET_P3_GUID,
        2 => crate::nvenc::NV_ENC_PRESET_P7_GUID,
        _ => NV_ENC_PRESET_P1_GUID,
    }
}
