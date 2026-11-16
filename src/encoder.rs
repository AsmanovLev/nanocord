//! Единая точка выбора видеокодера для демонстрации экрана.
//!
//! Порядок предпочтений — от лучшего сжатия к максимальной совместимости:
//! **AV1 -> HEVC -> H.264**. Кодер пробуется на NVENC, затем на Media
//! Foundation (аппаратный MFT, в конце — программный).
//!
//! Кодек обязательно согласуется с собеседником: отправить AV1 тому, у кого
//! нет AV1-декодера, значит показать чёрный экран. Поэтому каждый узел
//! объявляет набор своих декодеров при рукопожатии, и кодер поднимает только
//! то, что приёмник реально разберёт.

use windows::core::GUID;
use windows::core::Result;

use crate::mfenc::{decoder_available, encoder_available, MfVideoEncoder};
use crate::nvenc::{NvencEncoder, NV_ENC_PRESET_P1_GUID, VideoCodec};

/// Порядок перебора: сначала лучшее сжатие, последним — то, что понимают все.
pub const LADDER: [VideoCodec; 3] = [VideoCodec::Av1, VideoCodec::Hevc, VideoCodec::H264];

/// Все кодеки, которые этот узел умеет принимать.
pub fn decode_caps() -> u8 {
    let mut mask = 0u8;
    for codec in LADDER {
        if decoder_available(codec) || local_decoder_works(codec) {
            mask |= codec.bit();
        }
    }
    mask
}

/// Декодеры, собранные в приложении как отдельные структуры.
fn local_decoder_works(codec: VideoCodec) -> bool {
    match codec {
        VideoCodec::H264 => crate::decoder::H264Decoder::new().is_ok(),
        VideoCodec::Hevc => crate::decoder::HevcDecoder::new().is_ok(),
        VideoCodec::Av1 => false,
    }
}

/// Кодеки, которые этот узел умеет отправлять (кодер действительно открылся).
pub fn encode_caps() -> u8 {
    let mut mask = 0u8;
    for codec in LADDER {
        if encoder_openable(codec) {
            mask |= codec.bit();
        }
    }
    mask
}

fn encoder_openable(codec: VideoCodec) -> bool {
    // AV1 через NVENC здесь не реализован (нужна своя структура конфигурации),
    // поэтому AV1 идёт только через Media Foundation.
    if codec != VideoCodec::Av1
        && NvencEncoder::new_standalone_codec(
            640,
            360,
            22,
            4,
            30,
            codec.codec_guid(),
            NV_ENC_PRESET_P1_GUID,
        )
        .is_ok()
    {
        return true;
    }
    encoder_available(codec)
}

pub fn mask_names(mask: u8) -> String {
    let mut names = Vec::new();
    for codec in LADDER {
        if mask & codec.bit() != 0 {
            names.push(codec.short_name());
        }
    }
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    }
}

pub enum DesktopEncoder {
    Nvenc(NvencEncoder, VideoCodec),
    MediaFoundation(MfVideoEncoder),
}

impl DesktopEncoder {
    /// Создаёт лучший кодер из тех, что понимает и приёмник (`peer_decode`).
    pub fn create(
        width: u32,
        height: u32,
        cq: u8,
        max_bitrate_mbps: u32,
        fps: u32,
        preset: GUID,
        peer_decode: u8,
    ) -> Option<Self> {
        // Битрейт для MF: из «cq» он не выводится, берём верхнюю границу.
        let bitrate = (max_bitrate_mbps.max(4) * 1_000_000).min(40_000_000);

        for codec in LADDER {
            if peer_decode & codec.bit() == 0 {
                continue;
            }

            if codec != VideoCodec::Av1 {
                match NvencEncoder::new_standalone_codec(
                    width,
                    height,
                    cq,
                    max_bitrate_mbps,
                    fps,
                    codec.codec_guid(),
                    preset,
                ) {
                    Ok(encoder) => {
                        crate::diag::log(&format!(
                            "encoder: {}(NVENC) {width}x{height} fps={fps}",
                            codec.short_name()
                        ));
                        return Some(Self::Nvenc(encoder, codec));
                    }
                    Err(e) => crate::diag::log(&format!(
                        "encoder: NVENC {} unavailable ({})",
                        codec.short_name(),
                        e.message()
                    )),
                }
            }

            match MfVideoEncoder::new(codec, width, height, fps.max(1), bitrate) {
                Ok(encoder) => {
                    crate::diag::log(&format!(
                        "encoder: {}(MF) {width}x{height} fps={fps} {bitrate}bps via {}",
                        codec.short_name(),
                        encoder.label()
                    ));
                    return Some(Self::MediaFoundation(encoder));
                }
                Err(e) => crate::diag::log(&format!(
                    "encoder: MF {} unavailable ({})",
                    codec.short_name(),
                    e.message()
                )),
            }
        }

        crate::diag::log(&format!(
            "encoder: nothing works for peer capabilities [{}]",
            mask_names(peer_decode)
        ));
        None
    }

    pub fn encode_rgba_bytes(&mut self, rgba: &[u8], force_idr: bool) -> Result<Vec<u8>> {
        match self {
            Self::Nvenc(encoder, _) => encoder.encode_rgba_bytes(rgba, force_idr),
            Self::MediaFoundation(encoder) => encoder.encode_rgba_bytes(rgba, force_idr),
        }
    }

    /// Флаг кодека для протокола: 2 — H.264, 3 — HEVC, 4 — AV1.
    pub fn codec_flag(&self) -> u8 {
        match self {
            Self::Nvenc(_, codec) => codec.wire_flag(),
            Self::MediaFoundation(encoder) => encoder.codec_flag(),
        }
    }

    pub fn codec(&self) -> VideoCodec {
        match self {
            Self::Nvenc(_, codec) => *codec,
            Self::MediaFoundation(encoder) => encoder.codec(),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Nvenc(_, VideoCodec::Av1) => "AV1(NVENC)",
            Self::Nvenc(_, VideoCodec::Hevc) => "HEVC(NVENC)",
            Self::Nvenc(_, VideoCodec::H264) => "H264(NVENC)",
            Self::MediaFoundation(encoder) => match encoder.codec() {
                VideoCodec::Av1 => "AV1(MF)",
                VideoCodec::Hevc => "HEVC(MF)",
                VideoCodec::H264 => "H264(MF)",
            },
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
