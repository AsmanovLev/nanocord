#![allow(dead_code, unused_variables, unused_imports)]

use std::ptr::null_mut;
use windows::core::{Result, GUID};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::Media::MediaFoundation::*;

/// Нативный аппаратный декодер HEVC (H.265) на базе Windows Media Foundation / GPU MFT.
pub struct HevcDecoder {
    transform: IMFTransform,
    pub width: u32,
    pub height: u32,
    is_stream_started: bool,
    rgba_buffer: Vec<u8>,
    pts_hns: i64,
}

impl HevcDecoder {
    pub fn new() -> Result<Self> {
        unsafe {
            let _ = MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET);
            let in_type = MFT_REGISTER_TYPE_INFO {
                guidMajorType: MFMediaType_Video,
                guidSubtype: MFVideoFormat_HEVC,
            };
            let mut p_activate: *mut Option<IMFActivate> = null_mut();
            let mut count: u32 = 0;
            let _ = MFTEnumEx(
                MFT_CATEGORY_VIDEO_DECODER,
                MFT_ENUM_FLAG_ALL,
                Some(&in_type),
                None,
                &mut p_activate,
                &mut count,
            );
            if count > 0 && !p_activate.is_null() {
                let slice = std::slice::from_raw_parts(p_activate, count as usize);
                for act_opt in slice {
                    if let Some(act) = act_opt {
                        if let Ok(transform) = act.ActivateObject::<IMFTransform>() {
                            if let Ok(attrs) = transform.GetAttributes() {
                                let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
                            }
                            return Ok(Self {
                                transform,
                                width: 0,
                                height: 0,
                                is_stream_started: false,
                                rgba_buffer: Vec::new(),
                                pts_hns: 0,
                            });
                        }
                    }
                }
            }
            Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259)))
        }
    }

    pub fn configure(&mut self, width: u32, height: u32) -> Result<()> {
        if self.width == width && self.height == height && self.is_stream_started {
            return Ok(());
        }

        unsafe {
            let in_type = MFCreateMediaType()?;
            in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            in_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_HEVC)?;
            in_type.SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | (height as u64))?;
            self.transform.SetInputType(0, &in_type, 0)?;

            let out_type = MFCreateMediaType()?;
            out_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            out_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            out_type.SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | (height as u64))?;
            self.transform.SetOutputType(0, &out_type, 0)?;

            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);

            self.width = width;
            self.height = height;
            self.is_stream_started = true;
            self.rgba_buffer = vec![0u8; (width * height * 4) as usize];
            Ok(())
        }
    }

    pub fn decode_packet(&mut self, width: u32, height: u32, nal_bytes: &[u8]) -> Result<Option<(u32, u32, Vec<u8>)>> {
        if nal_bytes.is_empty() {
            return Ok(None);
        }

        if !self.is_stream_started || self.width != width || self.height != height {
            self.configure(width, height)?;
        }

        unsafe {
            let sample = MFCreateSample()?;
            let buffer = MFCreateMemoryBuffer(nal_bytes.len() as u32)?;

            let mut p_data: *mut u8 = null_mut();
            let mut max_len = 0u32;
            let mut cur_len = 0u32;
            buffer.Lock(&mut p_data, Some(&mut max_len), Some(&mut cur_len))?;
            std::ptr::copy_nonoverlapping(nal_bytes.as_ptr(), p_data, nal_bytes.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(nal_bytes.len() as u32)?;
            sample.AddBuffer(&buffer)?;
            let _ = sample.SetSampleTime(self.pts_hns);
            let _ = sample.SetSampleDuration(166_666);
            self.pts_hns += 166_666;

            match self.transform.ProcessInput(0, &sample, 0) {
                Ok(_) => {}
                Err(e) => {
                    const MF_E_NOTACCEPTING: i32 = -1072875847;
                    if e.code().0 != MF_E_NOTACCEPTING {
                        return Err(e);
                    }
                }
            }

            let mut out_buffer_desc = MFT_OUTPUT_DATA_BUFFER::default();
            out_buffer_desc.dwStreamID = 0;

            let out_sample = MFCreateSample()?;
            let nv12_size = (width * height * 3 / 2) as u32;
            let out_media_buffer = MFCreateMemoryBuffer(nv12_size * 2)?;
            out_sample.AddBuffer(&out_media_buffer)?;
            out_buffer_desc.pSample = std::mem::ManuallyDrop::new(Some(out_sample));

            let mut status = 0u32;
            match self.transform.ProcessOutput(0, std::slice::from_mut(&mut out_buffer_desc), &mut status) {
                Ok(_) => {
                    if let Some(ref sample_res) = out_buffer_desc.pSample.as_ref() {
                        let media_buf = sample_res.ConvertToContiguousBuffer()?;
                        let mut p_nv12: *mut u8 = null_mut();
                        let mut len = 0u32;
                        media_buf.Lock(&mut p_nv12, None, Some(&mut len))?;

                        let nv12_slice = std::slice::from_raw_parts(p_nv12, len as usize);
                        nv12_to_rgba(width as usize, height as usize, nv12_slice, &mut self.rgba_buffer);
                        media_buf.Unlock()?;

                        return Ok(Some((width, height, self.rgba_buffer.clone())));
                    }
                }
                Err(e) => {
                    const MF_E_TRANSFORM_NEED_MORE_INPUT: i32 = -1072875852;
                    const MF_E_TRANSFORM_STREAM_CHANGE: i32 = -1072861855;
                    if e.code().0 == MF_E_TRANSFORM_NEED_MORE_INPUT {
                        return Ok(None);
                    } else if e.code().0 == MF_E_TRANSFORM_STREAM_CHANGE {
                        if let Ok(new_out_type) = self.transform.GetOutputAvailableType(0, 0) {
                            let _ = self.transform.SetOutputType(0, &new_out_type, 0);
                        }
                        return Ok(None);
                    }
                }
            }

            Ok(None)
        }
    }
}

/// Нативный аппаратный декодер H.264 на базе встроенного в Windows Media Foundation MFT (CMSH264DecoderMFT).
/// Не требует внешних DLL (FFmpeg), работает из коробки на любой версии Windows 10/11.
pub struct H264Decoder {
    transform: IMFTransform,
    pub width: u32,
    pub height: u32,
    is_stream_started: bool,
    rgba_buffer: Vec<u8>,
    pts_hns: i64,
}

impl H264Decoder {
    pub fn new() -> Result<Self> {
        unsafe {
            let _ = MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET);
            let transform: IMFTransform = CoCreateInstance(&CMSH264DecoderMFT, None, CLSCTX_INPROC_SERVER)?;

            if let Ok(attrs) = transform.GetAttributes() {
                let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
            }

            Ok(Self {
                transform,
                width: 0,
                height: 0,
                is_stream_started: false,
                rgba_buffer: Vec::new(),
                pts_hns: 0,
            })
        }
    }

    /// Инициализирует входной и выходной форматы потока под заданное разрешение
    pub fn configure(&mut self, width: u32, height: u32) -> Result<()> {
        if self.width == width && self.height == height && self.is_stream_started {
            return Ok(());
        }

        unsafe {
            let in_type = MFCreateMediaType()?;
            in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            in_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            in_type.SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | (height as u64))?;
            self.transform.SetInputType(0, &in_type, 0)?;

            // Задаем выходной формат NV12
            let out_type = MFCreateMediaType()?;
            out_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            out_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            out_type.SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | (height as u64))?;
            self.transform.SetOutputType(0, &out_type, 0)?;

            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);

            self.width = width;
            self.height = height;
            self.is_stream_started = true;
            self.rgba_buffer = vec![0u8; (width * height * 4) as usize];
            Ok(())
        }
    }

    /// Декодирует порцию NAL-битстрима H.264 и возвращает RGBA-кадр при готовности
    pub fn decode_packet(&mut self, width: u32, height: u32, nal_bytes: &[u8]) -> Result<Option<(u32, u32, Vec<u8>)>> {
        if nal_bytes.is_empty() {
            return Ok(None);
        }

        if !self.is_stream_started || self.width != width || self.height != height {
            self.configure(width, height)?;
        }

        unsafe {
            // 1. Создаем IMFSample для входного H.264 NAL пакета
            let sample = MFCreateSample()?;
            let buffer = MFCreateMemoryBuffer(nal_bytes.len() as u32)?;

            let mut p_data: *mut u8 = null_mut();
            let mut max_len = 0u32;
            let mut cur_len = 0u32;
            buffer.Lock(&mut p_data, Some(&mut max_len), Some(&mut cur_len))?;
            std::ptr::copy_nonoverlapping(nal_bytes.as_ptr(), p_data, nal_bytes.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(nal_bytes.len() as u32)?;
            sample.AddBuffer(&buffer)?;
            let _ = sample.SetSampleTime(self.pts_hns);
            let _ = sample.SetSampleDuration(166_666);
            self.pts_hns += 166_666;

            // 2. Отправляем в MFT
            match self.transform.ProcessInput(0, &sample, 0) {
                Ok(_) => {}
                Err(e) => {
                    const MF_E_NOTACCEPTING: i32 = -1072875847; // 0xC00D36B9
                    if e.code().0 != MF_E_NOTACCEPTING {
                        return Err(e);
                    }
                }
            }

            // 3. Вычитываем готовый декодированный NV12 кадр
            let mut out_buffer_desc = MFT_OUTPUT_DATA_BUFFER::default();
            out_buffer_desc.dwStreamID = 0;

            let out_sample = MFCreateSample()?;
            let nv12_size = (width * height * 3 / 2) as u32;
            let out_media_buffer = MFCreateMemoryBuffer(nv12_size * 2)?;
            out_sample.AddBuffer(&out_media_buffer)?;
            out_buffer_desc.pSample = std::mem::ManuallyDrop::new(Some(out_sample));

            let mut status = 0u32;
            match self.transform.ProcessOutput(0, std::slice::from_mut(&mut out_buffer_desc), &mut status) {
                Ok(_) => {
                    if let Some(ref sample_res) = out_buffer_desc.pSample.as_ref() {
                        let media_buf = sample_res.ConvertToContiguousBuffer()?;
                        let mut p_nv12: *mut u8 = null_mut();
                        let mut len = 0u32;
                        media_buf.Lock(&mut p_nv12, None, Some(&mut len))?;

                        let nv12_slice = std::slice::from_raw_parts(p_nv12, len as usize);
                        nv12_to_rgba(width as usize, height as usize, nv12_slice, &mut self.rgba_buffer);
                        media_buf.Unlock()?;

                        return Ok(Some((width, height, self.rgba_buffer.clone())));
                    }
                }
                Err(e) => {
                    const MF_E_TRANSFORM_NEED_MORE_INPUT: i32 = -1072875852; // 0xC00D36B4
                    const MF_E_TRANSFORM_STREAM_CHANGE: i32 = -1072861855; // 0xC00D6D61
                    if e.code().0 == MF_E_TRANSFORM_NEED_MORE_INPUT {
                        return Ok(None);
                    } else if e.code().0 == MF_E_TRANSFORM_STREAM_CHANGE {
                        // Stream format changed according to bitstream SPS/PPS. Re-negotiate output type.
                        if let Ok(new_out_type) = self.transform.GetOutputAvailableType(0, 0) {
                            let _ = self.transform.SetOutputType(0, &new_out_type, 0);
                        }
                        return Ok(None);
                    } else {
                        eprintln!("[H264Decoder] ProcessOutput returned: 0x{:08X}", e.code().0 as u32);
                    }
                }
            }

            Ok(None)
        }
    }
}

/// Быстрое целочисленное преобразование цветового пространства NV12 (Y-plane + interleaved UV) в RGBA8888
#[inline]
pub fn nv12_to_rgba(width: usize, height: usize, nv12: &[u8], rgba: &mut [u8]) {
    let y_plane_size = width * height;
    if nv12.len() < y_plane_size + (y_plane_size / 2) || rgba.len() < width * height * 4 {
        return;
    }

    let y_plane = &nv12[..y_plane_size];
    let uv_plane = &nv12[y_plane_size..];

    for y in 0..height {
        let y_row = y * width;
        let uv_row = (y / 2) * width;
        let dst_row = y * width * 4;

        for x in 0..width {
            let y_val = y_plane[y_row + x] as i32;
            let uv_idx = uv_row + (x & !1);
            let u_val = uv_plane[uv_idx] as i32;
            let v_val = uv_plane[uv_idx + 1] as i32;

            // Стандартное быстрое целочисленное преобразование BT.601
            let c = y_val - 16;
            let d = u_val - 128;
            let e = v_val - 128;

            let r = ((298 * c + 409 * e + 128) >> 8).clamp(0, 255) as u8;
            let g = ((298 * c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255) as u8;
            let b = ((298 * c + 516 * d + 128) >> 8).clamp(0, 255) as u8;

            let d_idx = dst_row + x * 4;
            rgba[d_idx] = r;
            rgba[d_idx + 1] = g;
            rgba[d_idx + 2] = b;
            rgba[d_idx + 3] = 255;
        }
    }
}
