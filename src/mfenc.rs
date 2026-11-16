#![allow(dead_code)]

//! Аппаратно-независимый энкодер H.264 через Media Foundation.
//!
//! Зачем: NVENC есть не на всякой карте и не во всяком драйвере. На ноутбуке с
//! гибридной графикой `nvEncOpenEncodeSessionEx` может отвечать
//! NV_ENC_ERR_INVALID_VERSION (15) на любой версии API — тогда приложение
//! оставалось без видео вообще. Media Foundation даёт три пути сразу:
//! аппаратный MFT (AMD VCN / Intel QSV / NVIDIA), а если их нет — программный
//! H264 Encoder MFT, который есть в любой Windows.
//!
//! Кодирование потоковое: кадр BGRA -> NV12 (BT.709, ограниченный диапазон) ->
//! IMFSample -> Annex B байты, то есть тот же формат, что отдаёт NVENC.

use std::sync::OnceLock;
use windows::core::{Interface, Result, GUID};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::Variant::VARIANT;

fn hr(code: i32) -> windows::core::Error {
    windows::core::Error::from_hresult(windows::core::HRESULT(code))
}

/// Media Foundation нужно поднять один раз на процесс.
fn mf_ready() -> Result<()> {
    static INIT: OnceLock<i32> = OnceLock::new();
    let code = *INIT.get_or_init(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        match MFStartup(MF_VERSION, MFSTARTUP_LITE) {
            Ok(()) => 0,
            Err(e) => e.code().0,
        }
    });
    if code == 0 { Ok(()) } else { Err(hr(code)) }
}

/// Четырёхсимвольный код подтипа: `NV12`, `H264`, `YUY2` и т.п.
fn fourcc(guid: &GUID) -> String {
    // FourCC лежит в Data1 и записан little-endian: MFVideoFormat_H264 -> "H264".
    guid.data1
        .to_le_bytes()
        .iter()
        .map(|b| if b.is_ascii_graphic() { *b as char } else { '?' })
        .collect()
}

fn friendly_name(activate: &IMFActivate) -> String {
    unsafe {
        let mut value = windows::core::PWSTR::null();
        let mut len = 0u32;
        if activate
            .GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut value, &mut len)
            .is_ok()
            && !value.is_null()
        {
            let text = value.to_string().unwrap_or_default();
            windows::Win32::System::Com::CoTaskMemFree(Some(value.as_ptr() as *const _));
            return text;
        }
    }
    "MFT".to_string()
}

/// Все подходящие MFT-энкодеры H.264: сначала аппаратные, потом программные.
fn encoder_candidates() -> Vec<(IMFTransform, String)> {
    let output_info = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let groups = [
        (
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            "hardware",
        ),
        (
            MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_LOCALMFT | MFT_ENUM_FLAG_SORTANDFILTER,
            "software",
        ),
    ];

    let mut found = Vec::new();
    for (flags, kind) in groups {
        unsafe {
            let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut count = 0u32;
            if MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                flags,
                None,
                Some(&output_info),
                &mut activates,
                &mut count,
            )
            .is_err()
                || activates.is_null()
            {
                continue;
            }
            for index in 0..count as usize {
                let activate = (*activates.add(index)).clone();
                let Some(activate) = activate else { continue };
                let label = format!("{} [{kind}]", friendly_name(&activate));
                if let Ok(transform) = activate.ActivateObject::<IMFTransform>() {
                    // ShutdownObject здесь вызывать нельзя: он выгружает DLL
                    // энкодера (amdenc64.dll), а объект transform остаётся
                    // живым — следующий вызов уходит в выгруженную память
                    // и валит процесс с access violation.
                    found.push((transform, label));
                }
                drop(activate);
            }
            windows::Win32::System::Com::CoTaskMemFree(Some(activates as *const _));
        }
    }
    found
}

/// Формат кадра на входе энкодера: MFT-ы принимают разное.
#[derive(Clone, Copy, PartialEq)]
enum InputFormat {
    Nv12,
    I420,
}

/// Кодировщик H.264 на Media Foundation.
pub struct MfH264Encoder {
    transform: IMFTransform,
    width: u32,
    height: u32,
    fps: u32,
    frame_index: i64,
    label: String,
    frame_buf: Vec<u8>,
    input_format: InputFormat,
    started: bool,
    /// Собственный буфер вывода нужен MFT, который не выдаёт сэмплы сам.
    needs_output_buffer: bool,
    output_buffer_size: u32,
}

impl MfH264Encoder {
    pub fn new(width: u32, height: u32, fps: u32, bitrate_bps: u32) -> Result<Self> {
        mf_ready()?;
        let candidates = encoder_candidates();
        if candidates.is_empty() {
            return Err(hr(0x80004005u32 as i32)); // E_FAIL: ни одного H.264 MFT
        }

        let mut failures = Vec::new();
        for (transform, label) in candidates {
            match Self::configure(transform, width, height, fps, bitrate_bps, label.clone()) {
                Ok(mut encoder) => {
                    // Проверяем энкодер чёрным кадром сразу: у NVIDIA MFT
                    // внутри тот же сломанный NVENC, и падать лучше на старте,
                    // а не в первом живом кадре.
                    let probe = vec![0u8; (width * height * 4) as usize];
                    match encoder.encode_rgba_bytes(&probe, true) {
                        Ok(_) => {
                            crate::diag::log(&format!("mfenc: using {label} {}x{fps}", width));
                            return Ok(encoder);
                        }
                        Err(e) => failures.push(format!("{label}: {} ({:#010X})", e.message(), e.code().0)),
                    }
                }
                Err(e) => {
                    crate::diag::log(&format!("mfenc {label}: unusable - {}", e.message()));
                    failures.push(format!("{label}: {} ({:#010X})", e.message(), e.code().0));
                }
            }
        }

        crate::diag::log(&format!("mfenc: no usable encoder [{}]", failures.join("; ")));
        Err(hr(0x80004005u32 as i32))
    }

    fn configure(
        transform: IMFTransform,
        width: u32,
        height: u32,
        fps: u32,
        bitrate_bps: u32,
        label: String,
    ) -> Result<Self> {
        unsafe {
            let size = ((width as u64) << 32) | height as u64;
            let rate = ((fps as u64) << 32) | 1;
            let par = (1u64 << 32) | 1;

            // Аппаратные MFT (AMD VCN, Intel QSV) асинхронные: до снятия
            // блокировки они отвергают установку типов (MF_E_TRANSFORM_ASYNC_LOCKED).
            let unlocked = match transform.GetAttributes() {
                Ok(attributes) => attributes
                    .SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)
                    .is_ok(),
                Err(_) => false,
            };

            // Для энкодера первым ставится ВЫХОДНОЙ тип: пока его нет, список
            // входных типов пуст и GetInputAvailableType возвращает ошибку.
            let mut output_types = Vec::new();
            let mut output_ok = false;
            for index in 0..16u32 {
                let Ok(available) = transform.GetOutputAvailableType(0, index) else {
                    break;
                };
                let subtype = available
                    .GetGUID(&MF_MT_SUBTYPE)
                    .map(|g| fourcc(&g))
                    .unwrap_or_default();
                output_types.push(subtype.clone());
                if subtype != "H264" {
                    continue;
                }
                let _ = available.SetUINT32(&MF_MT_AVG_BITRATE, bitrate_bps);
                let _ = available.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32);
                let _ = available.SetUINT32(
                    &MF_MT_INTERLACE_MODE,
                    MFVideoInterlace_Progressive.0 as u32,
                );
                let _ = available.SetUINT64(&MF_MT_FRAME_SIZE, size);
                let _ = available.SetUINT64(&MF_MT_FRAME_RATE, rate);
                let _ = available.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, par);
                if transform.SetOutputType(0, &available, 0).is_ok() {
                    output_ok = true;
                    break;
                }
            }
            crate::diag::log(&format!(
                "mfenc {label}: unlocked={unlocked} output types [{}] set={output_ok}",
                output_types.join(", ")
            ));
            if !output_ok {
                return Err(windows::core::Error::from_hresult(MF_E_INVALIDMEDIATYPE));
            }

            // Теперь спрашиваем, какой вход энкодер реально принимает.
            let mut input_types = Vec::new();
            let mut offered: Vec<(InputFormat, String, IMFMediaType)> = Vec::new();
            for index in 0..16u32 {
                let Ok(available) = transform.GetInputAvailableType(0, index) else {
                    break;
                };
                let subtype = available
                    .GetGUID(&MF_MT_SUBTYPE)
                    .map(|g| fourcc(&g))
                    .unwrap_or_default();
                input_types.push(subtype.clone());
                let format = match subtype.as_str() {
                    "NV12" => Some(InputFormat::Nv12),
                    "I420" | "IYUV" => Some(InputFormat::I420),
                    _ => None,
                };
                if let Some(format) = format {
                    offered.push((format, subtype, available));
                }
            }
            // NV12 вперёд: он же нужен NVENC и совпадает с конвертером по умолчанию.
            offered.sort_by_key(|(format, _, _)| match format {
                InputFormat::Nv12 => 0,
                InputFormat::I420 => 1,
            });
            let mut chosen: Option<(InputFormat, String)> = None;
            for (format, subtype, available) in offered {
                let _ = available.SetUINT32(
                    &MF_MT_INTERLACE_MODE,
                    MFVideoInterlace_Progressive.0 as u32,
                );
                let _ = available.SetUINT64(&MF_MT_FRAME_SIZE, size);
                let _ = available.SetUINT64(&MF_MT_FRAME_RATE, rate);
                let _ = available.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, par);
                if transform.SetInputType(0, &available, 0).is_ok() {
                    chosen = Some((format, subtype));
                    break;
                }
            }
            crate::diag::log(&format!(
                "mfenc {label}: input types [{}] chosen={:?}",
                input_types.join(", "),
                chosen.as_ref().map(|(_, name)| name.clone())
            ));
            let Some((input_format, _)) = chosen else {
                return Err(windows::core::Error::from_hresult(MF_E_INVALIDMEDIATYPE));
            };

            let encoder = Self {
                transform,
                width,
                height,
                fps,
                frame_index: 0,
                label,
                frame_buf: vec![0u8; (width * height * 3 / 2) as usize],
                input_format,
                started: false,
                needs_output_buffer: false,
                output_buffer_size: 0,
            };
            encoder.apply_codec_settings(bitrate_bps);
            let (needs_output_buffer, output_buffer_size) = encoder.output_stream_info();
            crate::diag::log(&format!(
                "mfenc: output provides_samples={needs_output_buffer} size={output_buffer_size}"
            ));
            Ok(Self {
                needs_output_buffer,
                output_buffer_size,
                ..encoder
            })
        }
    }

    fn output_stream_info(&self) -> (bool, u32) {
        unsafe {
            match self.transform.GetOutputStreamInfo(0) {
                Ok(info) => {
                    let provides =
                        info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
                    (provides, info.cbSize.max(self.width * self.height))
                }
                Err(_) => (false, self.width * self.height * 3),
            }
        }
    }

    /// Низкая задержка, CBR и размер GOP. Всё необязательное: MFT может не
    /// поддерживать конкретную настройку, и это не повод не кодировать.
    fn apply_codec_settings(&self, bitrate_bps: u32) {
        unsafe {
            let Ok(codec) = self.transform.cast::<ICodecAPI>() else {
                return;
            };
            let set_u32 = |key: &GUID, value: u32| {
                let variant = VARIANT::from(value);
                let _ = codec.SetValue(key, &variant);
            };
            let set_bool = |key: &GUID, value: bool| {
                let variant = VARIANT::from(value);
                let _ = codec.SetValue(key, &variant);
            };

            set_u32(
                &CODECAPI_AVEncCommonRateControlMode,
                eAVEncCommonRateControlMode_CBR.0 as u32,
            );
            set_u32(&CODECAPI_AVEncCommonMeanBitRate, bitrate_bps);
            set_u32(&CODECAPI_AVEncCommonMaxBitRate, (bitrate_bps * 3) / 2);
            set_bool(&CODECAPI_AVLowLatencyMode, true);
            set_u32(&CODECAPI_AVEncMPVDefaultBPictureCount, 0);
            set_u32(&CODECAPI_AVEncMPVGOPSize, self.fps.max(1) * 2);
        }
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn codec_flag(&self) -> u8 {
        2 // H.264
    }

    fn ensure_started(&mut self) {
        if self.started {
            return;
        }
        unsafe {
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
        }
        self.started = true;
    }

    /// BGRA-байты кадра -> Annex B H.264.
    pub fn encode_rgba_bytes(&mut self, rgba: &[u8], force_idr: bool) -> Result<Vec<u8>> {
        self.convert_to_input(rgba);
        self.encode_frame(force_idr)
    }

    /// BGRA -> NV12/I420, BT.709 ограниченный диапазон.
    fn convert_to_input(&mut self, rgba: &[u8]) {
        let width = self.width as usize;
        let height = self.height as usize;
        let needed = width * height * 4;
        let y_len = width * height;
        if rgba.len() < needed || self.frame_buf.len() < y_len + y_len / 2 {
            return;
        }

        let (y_plane, rest) = self.frame_buf.split_at_mut(y_len);
        let chroma_len = y_len / 4;

        for y in 0..height {
            let row = y * width * 4;
            for x in 0..width {
                let p = row + x * 4;
                let r = rgba[p] as i32;
                let g = rgba[p + 1] as i32;
                let b = rgba[p + 2] as i32;
                y_plane[y * width + x] =
                    (((47 * r + 157 * g + 16 * b + 128) >> 8) + 16).clamp(16, 235) as u8;
            }
        }

        // Хрома считается по блоку 2x2: у 4:2:0 одна пара U/V на четыре пикселя.
        let mut interleaved = 0usize;
        for y in (0..height).step_by(2) {
            for x in (0..width).step_by(2) {
                let base = y * width * 4 + x * 4;
                let mut r = 0i32;
                let mut g = 0i32;
                let mut b = 0i32;
                for offset in [0usize, 4, width * 4, width * 4 + 4] {
                    let p = base + offset;
                    if p + 2 >= rgba.len() {
                        continue;
                    }
                    r += rgba[p] as i32;
                    g += rgba[p + 1] as i32;
                    b += rgba[p + 2] as i32;
                }
                r /= 4;
                g /= 4;
                b /= 4;
                let u = (((-26 * r - 86 * g + 112 * b + 128) >> 8) + 128).clamp(16, 240) as u8;
                let v = (((112 * r - 102 * g - 10 * b + 128) >> 8) + 128).clamp(16, 240) as u8;

                match self.input_format {
                    InputFormat::Nv12 => {
                        if interleaved + 1 < rest.len() {
                            rest[interleaved] = u;
                            rest[interleaved + 1] = v;
                        }
                        interleaved += 2;
                    }
                    InputFormat::I420 => {
                        let index = (y / 2) * (width / 2) + x / 2;
                        if index < chroma_len {
                            rest[index] = u;
                            rest[chroma_len + index] = v;
                        }
                    }
                }
            }
        }
    }

    pub fn encode_frame(&mut self, force_idr: bool) -> Result<Vec<u8>> {
        self.ensure_started();
        unsafe {
            if force_idr {
                if let Ok(codec) = self.transform.cast::<ICodecAPI>() {
                    let variant = VARIANT::from(1u32);
                    let _ = codec.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &variant);
                }
            }

            let buffer = MFCreateMemoryBuffer(self.frame_buf.len() as u32)?;
            {
                let mut ptr: *mut u8 = std::ptr::null_mut();
                buffer.Lock(&mut ptr, None, None)?;
                std::ptr::copy_nonoverlapping(self.frame_buf.as_ptr(), ptr, self.frame_buf.len());
                buffer.Unlock()?;
            }
            buffer.SetCurrentLength(self.frame_buf.len() as u32)?;

            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            let time = self.frame_index * 10_000_000 / self.fps.max(1) as i64;
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(10_000_000 / self.fps.max(1) as i64)?;

            if let Err(e) = self.transform.ProcessInput(0, &sample, 0) {
                crate::diag::log(&format!("mfenc: ProcessInput failed {:#010X}", e.code().0));
                return Err(e);
            }
            self.frame_index += 1;

            let mut collected: Vec<u8> = Vec::new();
            loop {
                let mut mft_out = MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: std::mem::ManuallyDrop::new(None),
                    dwStatus: 0,
                    pEvents: std::mem::ManuallyDrop::new(None),
                };
                let mut out_sample: Option<IMFSample> = None;
                if !self.needs_output_buffer {
                    let out_buffer = MFCreateMemoryBuffer(self.output_buffer_size.max(64 * 1024))?;
                    let s = MFCreateSample()?;
                    s.AddBuffer(&out_buffer)?;
                    out_sample = Some(s);
                }
                mft_out.pSample = std::mem::ManuallyDrop::new(out_sample);

                let mut status = 0u32;
                let outcome = self
                    .transform
                    .ProcessOutput(0, std::slice::from_mut(&mut mft_out), &mut status);
                match outcome {
                    Ok(()) => {
                        crate::diag::log(&format!(
                            "mfenc: output ok status={status} has_sample={}",
                            mft_out.pSample.is_some()
                        ));
                        let produced = std::mem::ManuallyDrop::take(&mut mft_out.pSample);
                        std::mem::ManuallyDrop::drop(&mut mft_out.pEvents);
                        if let Some(produced) = produced {
                            match produced.ConvertToContiguousBuffer() {
                                Ok(contiguous) => {
                                    let mut ptr: *mut u8 = std::ptr::null_mut();
                                    let mut len = 0u32;
                                    match contiguous.Lock(&mut ptr, None, Some(&mut len)) {
                                        Ok(()) => {
                                            crate::diag::log(&format!(
                                                "mfenc: got {len} bytes (total after)={}",
                                                collected.len() + len as usize
                                            ));
                                            if len > 0 && !ptr.is_null() {
                                                collected.extend_from_slice(
                                                    std::slice::from_raw_parts(ptr, len as usize),
                                                );
                                            }
                                            let _ = contiguous.Unlock();
                                        }
                                        Err(e) => crate::diag::log(&format!(
                                            "mfenc: buffer Lock failed {:#010X}",
                                            e.code().0
                                        )),
                                    }
                                }
                                Err(e) => crate::diag::log(&format!(
                                    "mfenc: ConvertToContiguousBuffer failed {:#010X}",
                                    e.code().0
                                )),
                            }
                        }
                        // Не break по status: часть MFT отдаёт несколько
                        // сэмплов подряд с нулевым статусом. Единственный
                        // признак «пока всё» — MF_E_TRANSFORM_NEED_MORE_INPUT.
                        let _ = status;
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                        std::mem::ManuallyDrop::drop(&mut mft_out.pEvents);
                        crate::diag::log_once("mfenc: output drained (need more input)");
                        break;
                    }
                    Err(e) => {
                        std::mem::ManuallyDrop::drop(&mut mft_out.pEvents);
                        crate::diag::log(&format!(
                            "mfenc: ProcessOutput failed {:#010X} {}",
                            e.code().0,
                            e.message()
                        ));
                        return Err(e);
                    }
                }
            }

            crate::diag::log(&format!(
                "mfenc: frame {} -> {} bytes",
                self.frame_index,
                collected.len()
            ));
            Ok(collected)
        }
    }
}
