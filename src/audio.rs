#![allow(dead_code, unused_variables)]

use std::collections::VecDeque;
use std::f32::consts::PI;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use windows::core::{Interface, Result, GUID};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, IAudioCaptureClient, IAudioClient, IAudioRenderClient,
    IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
    DEVICE_STATE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED, STGM,
};
use windows::Win32::System::Variant::VARENUM;
use opus_pure::{Application, OpusDecoder, OpusEncoder, RateControl};

pub const AUDIO_SPECTROGRAM_BINS: usize = 20;

/// Высокопроизводительный энкодер Opus с VBR (Variable Bitrate), FEC и DTX для P2P-стриминга
pub struct NanocordOpusEncoder {
    encoder: OpusEncoder,
    pcm_buffer: Vec<i16>,
    seq: u32,
}

impl NanocordOpusEncoder {
    /// Создаёт энкодер по умолчанию (VoIP режим 64 kbps VBR с FEC и DTX)
    pub fn new() -> Self {
        Self::new_voip(64_000)
    }

    /// Режим передачи голоса / микрофона: Application::Voip, In-band FEC, DTX (тишина сжимается в ~2 байта)
    pub fn new_voip(bitrate_bps: i32) -> Self {
        let mut encoder = OpusEncoder::new(48000, 1, Application::Voip).expect("OpusEncoder Voip creation failed");
        encoder.rate_control = RateControl::Vbr;
        encoder.bitrate_bps = bitrate_bps; // По умолчанию 64 kbps (стандарт качества Discord)
        encoder.complexity = 8;
        encoder.use_inband_fec = true;
        encoder.packet_loss_perc = 15; // Повышенная устойчивость к потерям пакетов (DPI, нестабильные сети)
        encoder.use_dtx = true;
        Self {
            encoder,
            pcm_buffer: Vec::with_capacity(1920),
            seq: 0,
        }
    }

    /// Режим трансляции системного звука / игры: Application::Audio, Fullband Hi-Fi, In-band FEC
    pub fn new_audio(bitrate_bps: i32) -> Self {
        let mut encoder = OpusEncoder::new(48000, 1, Application::Audio).expect("OpusEncoder Audio creation failed");
        encoder.rate_control = RateControl::Vbr;
        encoder.bitrate_bps = bitrate_bps; // По умолчанию 96-128 kbps
        encoder.complexity = 8;
        encoder.use_inband_fec = true;
        encoder.packet_loss_perc = 10;
        encoder.use_dtx = false;
        Self {
            encoder,
            pcm_buffer: Vec::with_capacity(1920),
            seq: 0,
        }
    }

    pub fn set_bitrate(&mut self, bitrate_bps: i32) {
        self.encoder.bitrate_bps = bitrate_bps;
    }

    pub fn set_packet_loss_perc(&mut self, perc: i32) {
        self.encoder.packet_loss_perc = perc;
    }

    pub fn set_dtx(&mut self, enabled: bool) {
        self.encoder.use_dtx = enabled;
    }

    pub fn set_fec(&mut self, enabled: bool) {
        self.encoder.use_inband_fec = enabled;
    }

    pub fn next_seq(&mut self) -> u32 {
        let s = self.seq;
        self.seq = self.seq.wrapping_add(1);
        s
    }

    pub fn reset(&mut self) {
        let _ = self.encoder.reset_state();
        self.pcm_buffer.clear();
        self.seq = 0;
    }

    /// Принимает произвольную порцию PCM сэмплов и кодирует в Opus пакеты по 960 сэмплов (20 мс @ 48 кГц) с VBR
    pub fn encode_chunk(&mut self, samples: &[i16]) -> Vec<Vec<u8>> {
        self.pcm_buffer.extend_from_slice(samples);
        let frame_size = 960; // 20 ms при 48 000 Гц

        // Защита от накопления задержки: если накопилось больше 100 мс (5 фреймов), сбрасываем старый хвост
        if self.pcm_buffer.len() > frame_size * 5 {
            let excess = self.pcm_buffer.len() - frame_size * 2;
            self.pcm_buffer.drain(0..excess);
        }

        let mut packets = Vec::new();
        let mut out_buf = vec![0u8; 1500];

        while self.pcm_buffer.len() >= frame_size {
            let frame: Vec<i16> = self.pcm_buffer.drain(0..frame_size).collect();
            if let Ok(len) = self.encoder.encode_s16(&frame, frame_size, &mut out_buf) {
                if len > 0 {
                    packets.push(out_buf[..len].to_vec());
                }
            }
        }
        packets
    }

    /// Сбрасывает остаток буфера с дополнением нулями для закрытия сессии
    pub fn encode_flush(&mut self) -> Vec<Vec<u8>> {
        let frame_size = 960;
        let mut packets = Vec::new();
        if !self.pcm_buffer.is_empty() {
            while self.pcm_buffer.len() < frame_size {
                self.pcm_buffer.push(0);
            }
            let frame: Vec<i16> = self.pcm_buffer.drain(0..frame_size).collect();
            let mut out_buf = vec![0u8; 1500];
            if let Ok(len) = self.encoder.encode_s16(&frame, frame_size, &mut out_buf) {
                if len > 0 {
                    packets.push(out_buf[..len].to_vec());
                }
            }
        }
        packets
    }
}

impl Default for NanocordOpusEncoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Утилита для сохранения и парсинга стандартных 16-битных PCM WAV файлов (48 кГц моно/стерео)
pub struct WavAudio;

impl WavAudio {
    /// Кодирует PCM сэмплы в стандартный WAV контейнер (RIFF/WAVE 16-bit PCM)
    pub fn encode_wav(samples: &[i16], sample_rate: u32, channels: u16) -> Vec<u8> {
        let byte_rate = sample_rate * channels as u32 * 2;
        let block_align = channels * 2;
        let data_size = (samples.len() * 2) as u32;
        let file_size = 36 + data_size;

        let mut buf = Vec::with_capacity(44 + samples.len() * 2);
        // RIFF header
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&file_size.to_le_bytes());
        buf.extend_from_slice(b"WAVE");

        // fmt chunk
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes()); // PCM format
        buf.extend_from_slice(&channels.to_le_bytes());
        buf.extend_from_slice(&sample_rate.to_le_bytes());
        buf.extend_from_slice(&byte_rate.to_le_bytes());
        buf.extend_from_slice(&block_align.to_le_bytes());
        buf.extend_from_slice(&16u16.to_le_bytes()); // 16-bit

        // data chunk
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&data_size.to_le_bytes());
        for &s in samples {
            buf.extend_from_slice(&s.to_le_bytes());
        }

        buf
    }

    /// Декодирует WAV файл обратно в 16-битный PCM буфер
    pub fn decode_wav(data: &[u8]) -> Option<(Vec<i16>, u32, u16)> {
        if data.len() < 44 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
            return None;
        }

        let mut offset = 12;
        let mut sample_rate = 48000u32;
        let mut channels = 1u16;
        let mut samples = Vec::new();

        while offset + 8 <= data.len() {
            let chunk_id = &data[offset..offset + 4];
            let chunk_size = u32::from_le_bytes(data[offset + 4..offset + 8].try_into().ok()?) as usize;
            offset += 8;

            if chunk_id == b"fmt " && chunk_size >= 16 && offset + 16 <= data.len() {
                channels = u16::from_le_bytes(data[offset + 2..offset + 4].try_into().ok()?);
                sample_rate = u32::from_le_bytes(data[offset + 4..offset + 8].try_into().ok()?);
            } else if chunk_id == b"data" {
                let actual_data_len = chunk_size.min(data.len() - offset);
                let num_samples = actual_data_len / 2;
                samples.reserve(num_samples);
                for i in 0..num_samples {
                    let s = i16::from_le_bytes(data[offset + i * 2..offset + i * 2 + 2].try_into().ok()?);
                    samples.push(s);
                }
                break;
            }
            offset += chunk_size;
        }

        Some((samples, sample_rate, channels))
    }
}

/// Высокопроизводительный декодер Opus с In-band FEC и PLC для кристально чистого звука без треска
pub struct NanocordOpusDecoder {
    decoder: OpusDecoder,
    last_seq: Option<u32>,
}

impl NanocordOpusDecoder {
    pub fn new() -> Self {
        let decoder = OpusDecoder::new(48000, 1).expect("OpusDecoder creation failed");
        Self {
            decoder,
            last_seq: None,
        }
    }

    pub fn reset(&mut self) {
        let _ = self.decoder.reset_state();
        self.last_seq = None;
    }

    /// Декодирует VBR пакет Opus обратно в 960 сэмплов 16-бит PCM
    pub fn decode_packet(&mut self, opus_packet: &[u8]) -> Vec<i16> {
        let frame_size = 960;
        let mut pcm_out = vec![0i16; frame_size];
        if opus_packet.is_empty() {
            // PLC (Packet Loss Concealment): декодирование интерполяции при пропущенном пакете
            if let Ok(samples_decoded) = self.decoder.decode_s16(&[], frame_size, &mut pcm_out) {
                pcm_out.truncate(samples_decoded);
                return pcm_out;
            }
            return Vec::new();
        }
        match self.decoder.decode_s16(opus_packet, frame_size, &mut pcm_out) {
            Ok(samples_decoded) => {
                pcm_out.truncate(samples_decoded);
                pcm_out
            }
            Err(_) => Vec::new(),
        }
    }

    /// Умное декодирование с контролем сетевых пропусков: если пакет был потерян,
    /// строго в хронологическом порядке восстанавливает его через In-Band FEC текущего пакета,
    /// либо интерполирует через PLC (Packet Loss Concealment), предотвращая любые щелчки («хруст»).
    pub fn decode_packet_with_fec(&mut self, seq: u32, opus_packet: &[u8]) -> Vec<i16> {
        let frame_size = 960;
        let mut result = Vec::new();

        if self.last_seq.is_none() && seq > 0 && seq <= 5 {
            // Если самые первые пакеты сессии были потеряны в сети, заполняем их интерполяцией PLC
            for _ in 0..seq {
                let mut plc_buf = vec![0i16; frame_size];
                if let Ok(n) = self.decoder.decode_s16(&[], frame_size, &mut plc_buf) {
                    plc_buf.truncate(n);
                    result.extend_from_slice(&plc_buf);
                }
            }
        }

        if let Some(prev) = self.last_seq {
            let diff = seq.wrapping_sub(prev);
            if diff > 1 && diff <= 4 {
                // Если выпало больше 1 пакета (например, 2 или 3),
                // хронологически самые ранние выпавшие фреймы сначала интерполируем через PLC
                for _ in 0..(diff - 2) {
                    let mut plc_buf = vec![0i16; frame_size];
                    if let Ok(n) = self.decoder.decode_s16(&[], frame_size, &mut plc_buf) {
                        plc_buf.truncate(n);
                        result.extend_from_slice(&plc_buf);
                    }
                }

                // Непосредственно предшествующий фрейм восстанавливаем через In-Band FEC текущего пакета
                let mut fec_buf = vec![0i16; frame_size];
                let mut fec_ok = false;
                if !opus_packet.is_empty() {
                    if let Ok(n) = self.decoder.decode_fec_s16(opus_packet, frame_size, &mut fec_buf) {
                        if n > 0 {
                            fec_buf.truncate(n);
                            result.extend_from_slice(&fec_buf);
                            fec_ok = true;
                        }
                    }
                }
                // Если FEC не сработал (например, пакет тишины DTX), закрываем PLC
                if !fec_ok {
                    let mut plc_buf = vec![0i16; frame_size];
                    if let Ok(n) = self.decoder.decode_s16(&[], frame_size, &mut plc_buf) {
                        plc_buf.truncate(n);
                        result.extend_from_slice(&plc_buf);
                    }
                }
            } else if diff > 4 && diff < 1000 {
                // Большой пропуск (зависание сети / DPI): генерируем 2 фрейма мягкого затухания PLC
                for _ in 0..2 {
                    let mut plc_buf = vec![0i16; frame_size];
                    if let Ok(n) = self.decoder.decode_s16(&[], frame_size, &mut plc_buf) {
                        plc_buf.truncate(n);
                        result.extend_from_slice(&plc_buf);
                    }
                }
            }
        }
        self.last_seq = Some(seq);

        if !opus_packet.is_empty() {
            let mut out_buf = vec![0i16; frame_size];
            match self.decoder.decode_s16(opus_packet, frame_size, &mut out_buf) {
                Ok(n) => {
                    out_buf.truncate(n);
                    result.extend_from_slice(&out_buf);
                }
                Err(_) => {}
            }
        }

        result
    }
}

impl Default for NanocordOpusDecoder {
    fn default() -> Self {
        Self::new()
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PROPERTYKEY {
    pub fmtid: GUID,
    pub pid: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct AudioSpectrumPacket {
    pub peak_db: f32,
    pub min_sample: f32,
    pub max_sample: f32,
    pub rms: f32,
    pub bins: [f32; AUDIO_SPECTROGRAM_BINS],
    pub pcm_chunk: Vec<i16>,
}

/// Перечисление всех активных аудиоустройств Windows (Микрофоны или Динамики)
pub fn enumerate_audio_devices(is_input: bool) -> Vec<AudioDevice> {
    let mut list = Vec::new();
    list.push(AudioDevice {
        id: "default".to_string(),
        name: if is_input {
            "По умолчанию (Системный микрофон)".to_string()
        } else {
            "По умолчанию (Системные динамики)".to_string()
        },
    });

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        if let Ok(enumerator) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) {
            let flow = if is_input { eCapture } else { eRender };
            // DEVICE_STATE(1) = ACTIVE
            if let Ok(collection) = enumerator.EnumAudioEndpoints(flow, DEVICE_STATE(1)) {
                if let Ok(count) = collection.GetCount() {
                    for i in 0..count {
                        if let Ok(dev) = collection.Item(i) {
                            let mut dev_id = String::new();
                            if let Ok(p_id) = dev.GetId() {
                                dev_id = p_id.to_string().unwrap_or_default();
                                windows::Win32::System::Com::CoTaskMemFree(Some(p_id.as_ptr() as *const _));
                            }

                            let mut name = format!("Аудио устройство #{}", i + 1);
                            if let Ok(store) = dev.OpenPropertyStore(STGM(0)) {
                                // PKEY_Device_FriendlyName = {a45c254e-df1c-4efd-8020-67d146a850e0}, 14
                                let pkey = windows::Win32::Foundation::PROPERTYKEY {
                                    fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
                                    pid: 14,
                                };
                                if let Ok(val) = store.GetValue(&pkey) {
                                    if val.Anonymous.Anonymous.vt == VARENUM(31) && !val.Anonymous.Anonymous.Anonymous.pwszVal.is_null() {
                                        if let Ok(s) = val.Anonymous.Anonymous.Anonymous.pwszVal.to_string() {
                                            name = s;
                                        }
                                    }
                                }
                            }
                            if !dev_id.is_empty() {
                                list.push(AudioDevice { id: dev_id, name });
                            }
                        }
                    }
                }
            }
        }
    }
    list
}

/// Плеер входящего звука через Windows WASAPI (воспроизведение в колонки / наушники)
pub struct WasapiAudioPlayer {
    pcm_tx: SyncSender<Vec<i16>>,
    pub is_running: Arc<AtomicBool>,
}

impl WasapiAudioPlayer {
    pub fn new() -> Self {
        Self::with_device(None)
    }

    pub fn with_device(device_id: Option<String>) -> Self {
        let (tx, rx) = sync_channel(64);
        let is_running = Arc::new(AtomicBool::new(true));
        let run_clone = is_running.clone();

        thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            let _ = run_wasapi_playback_loop(device_id, run_clone, rx);
        });

        Self {
            pcm_tx: tx,
            is_running,
        }
    }

    pub fn play_samples(&self, samples: Vec<i16>) {
        if !samples.is_empty() {
            let _ = self.pcm_tx.try_send(samples);
        }
    }
}

fn resample_linear(input: &[i16], in_rate: usize, out_rate: usize) -> Vec<f32> {
    if input.is_empty() {
        return Vec::new();
    }
    if in_rate == out_rate {
        return input.iter().map(|&s| s as f32 / 32768.0).collect();
    }
    let ratio = in_rate as f64 / out_rate as f64;
    let out_len = (input.len() as f64 / ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_idx = i as f64 * ratio;
        let idx0 = src_idx.floor() as usize;
        let idx1 = (idx0 + 1).min(input.len() - 1);
        let frac = (src_idx - idx0 as f64) as f32;
        let s0 = input[idx0] as f32 / 32768.0;
        let s1 = input[idx1] as f32 / 32768.0;
        out.push(s0 + (s1 - s0) * frac);
    }
    out
}

fn resample_i16_to_48k(input: &[i16], in_rate: usize) -> Vec<i16> {
    if in_rate == 48000 || input.is_empty() {
        return input.to_vec();
    }
    let ratio = in_rate as f64 / 48000.0;
    let out_len = (input.len() as f64 / ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_idx = i as f64 * ratio;
        let idx0 = src_idx.floor() as usize;
        let idx1 = (idx0 + 1).min(input.len() - 1);
        let frac = (src_idx - idx0 as f64) as f32;
        let s0 = input[idx0] as f32;
        let s1 = input[idx1] as f32;
        out.push((s0 + (s1 - s0) * frac).clamp(-32768.0, 32767.0) as i16);
    }
    out
}

fn run_wasapi_playback_loop(
    device_id: Option<String>,
    is_running: Arc<AtomicBool>,
    rx: Receiver<Vec<i16>>,
) -> Result<()> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device: IMMDevice = if let Some(ref dev_id) = device_id {
            if dev_id != "default" && !dev_id.is_empty() {
                let id_wide: Vec<u16> = dev_id.encode_utf16().chain(std::iter::once(0)).collect();
                match enumerator.GetDevice(windows::core::PCWSTR(id_wide.as_ptr())) {
                    Ok(d) => d,
                    Err(_) => enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?,
                }
            } else {
                enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?
            }
        } else {
            enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?
        };
        let audio_client: IAudioClient = device.Activate::<IAudioClient>(CLSCTX_ALL, None)?;
        let mix_format_ptr = audio_client.GetMixFormat()?;
        let pwfx = &*mix_format_ptr;

        let sample_rate = pwfx.nSamplesPerSec as usize;
        let channels = pwfx.nChannels as usize;
        let bits_per_sample = pwfx.wBitsPerSample as usize;

        audio_client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            0,
            1_000_000, // 100ms
            0,
            mix_format_ptr,
            None,
        )?;

        let buffer_frame_count = audio_client.GetBufferSize()?;
        let render_client: IAudioRenderClient = audio_client.GetService::<IAudioRenderClient>()?;
        audio_client.Start()?;

        let mut sample_queue: VecDeque<f32> = VecDeque::with_capacity(48000);
        let target_prebuffer = (sample_rate as f32 * 0.040) as usize; // 40ms подушка от джиттера (2 Opus фрейма)
        let max_buffer = (sample_rate as f32 * 0.600) as usize; // 600ms адаптивный предел для джиттера в сетях с задержками до 1500мс
        let mut is_prebuffering = true;
        let mut last_sample = 0.0f32;

        while is_running.load(Ordering::Relaxed) {
            while let Ok(chunk) = rx.try_recv() {
                let resampled = resample_linear(&chunk, 48000, sample_rate);
                for s in resampled {
                    sample_queue.push_back(s);
                }
            }

            if sample_queue.len() > max_buffer {
                // Плавное выравнивание по 20мс за цикл вместо резкого среза (предотвращает щелчки)
                let step = (sample_rate as f32 * 0.020) as usize;
                sample_queue.drain(0..step.min(sample_queue.len().saturating_sub(target_prebuffer)));
            }

            if is_prebuffering && sample_queue.len() >= target_prebuffer {
                is_prebuffering = false;
            }

            let padding = audio_client.GetCurrentPadding()?;
            let frames_to_fill = (buffer_frame_count - padding) as usize;

            if frames_to_fill > 0 {
                if let Ok(p_data) = render_client.GetBuffer(frames_to_fill as u32) {
                    if !p_data.is_null() {
                        if bits_per_sample == 32 {
                            let float_ptr = p_data as *mut f32;
                            for f in 0..frames_to_fill {
                                let sample = if !is_prebuffering {
                                    if let Some(s) = sample_queue.pop_front() {
                                        last_sample = s;
                                        s
                                    } else {
                                        is_prebuffering = true;
                                        last_sample *= 0.92; // Мягкое экспоненциальное затухание (предотвращает клики)
                                        last_sample
                                    }
                                } else {
                                    last_sample *= 0.92;
                                    last_sample
                                };
                                for c in 0..channels {
                                    *float_ptr.add(f * channels + c) = sample;
                                }
                            }
                        } else if bits_per_sample == 16 {
                            let i16_ptr = p_data as *mut i16;
                            for f in 0..frames_to_fill {
                                let sample = if !is_prebuffering {
                                    if let Some(s) = sample_queue.pop_front() {
                                        last_sample = s;
                                        (s.clamp(-1.0, 1.0) * 32767.0) as i16
                                    } else {
                                        is_prebuffering = true;
                                        last_sample *= 0.92;
                                        (last_sample.clamp(-1.0, 1.0) * 32767.0) as i16
                                    }
                                } else {
                                    last_sample *= 0.92;
                                    (last_sample.clamp(-1.0, 1.0) * 32767.0) as i16
                                };
                                for c in 0..channels {
                                    *i16_ptr.add(f * channels + c) = sample;
                                }
                            }
                        }
                        let _ = render_client.ReleaseBuffer(frames_to_fill as u32, 0);
                    }
                }
            }

            thread::sleep(Duration::from_millis(5));
        }
    }
    Ok(())
}

/// Запускает фоновый захват реального звука через Windows WASAPI
/// is_loopback = true  -> Звук системы/игры (WASAPI Loopback Capture с колонок/наушников)
/// is_loopback = false -> Микрофон (Default Audio Capture Endpoint либо указанный ID устройства)
pub fn start_wasapi_audio_worker(
    is_loopback: bool,
    device_id: Option<String>,
    is_running: Arc<AtomicBool>,
) -> Receiver<AudioSpectrumPacket> {
    let (tx, rx) = sync_channel(64);
    let dev_id_clone = device_id;
    let run_clone = is_running;

    thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }

        while run_clone.load(Ordering::Relaxed) {
            let res = run_wasapi_capture_loop(is_loopback, dev_id_clone.clone(), run_clone.clone(), &tx);
            if res.is_err() && run_clone.load(Ordering::Relaxed) {
                let silence = AudioSpectrumPacket {
                    peak_db: -60.0,
                    min_sample: 0.0,
                    max_sample: 0.0,
                    rms: 0.0,
                    bins: [0.01; AUDIO_SPECTROGRAM_BINS],
                    pcm_chunk: Vec::new(),
                };
                let _ = tx.try_send(silence);
                thread::sleep(Duration::from_millis(100));
            }
        }

        #[allow(unreachable_code)]
        unsafe {
            CoUninitialize();
        }
    });

    rx
}

fn run_wasapi_capture_loop(
    is_loopback: bool,
    device_id: Option<String>,
    is_running: Arc<AtomicBool>,
    tx: &SyncSender<AudioSpectrumPacket>,
) -> Result<()> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;

        let data_flow = if is_loopback { eRender } else { eCapture };
        let device: IMMDevice = if let Some(ref dev_id) = device_id {
            if dev_id != "default" && !dev_id.is_empty() {
                let id_wide: Vec<u16> = dev_id.encode_utf16().chain(std::iter::once(0)).collect();
                match enumerator.GetDevice(windows::core::PCWSTR(id_wide.as_ptr())) {
                    Ok(d) => d,
                    Err(_) => enumerator.GetDefaultAudioEndpoint(data_flow, eConsole)?,
                }
            } else {
                enumerator.GetDefaultAudioEndpoint(data_flow, eConsole)?
            }
        } else {
            enumerator.GetDefaultAudioEndpoint(data_flow, eConsole)?
        };

        let audio_client: IAudioClient = device.Activate::<IAudioClient>(CLSCTX_ALL, None)?;
        let mix_format_ptr = audio_client.GetMixFormat()?;
        let pwfx = &*mix_format_ptr;

        let sample_rate = pwfx.nSamplesPerSec as f32;
        let channels = pwfx.nChannels as usize;
        let bits_per_sample = pwfx.wBitsPerSample as usize;

        let stream_flags = if is_loopback {
            AUDCLNT_STREAMFLAGS_LOOPBACK
        } else {
            0
        };

        // Буфер на 100 мс (1_000_000 * 100 нс)
        audio_client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            stream_flags,
            1_000_000,
            0,
            mix_format_ptr,
            None,
        )?;

        let capture_client: IAudioCaptureClient = audio_client.GetService::<IAudioCaptureClient>()?;
        audio_client.Start()?;

        // Накопительный буфер для спектрального анализа (~1024 семплов)
        let mut sample_ring: Vec<f32> = Vec::with_capacity(2048);

        // Частотные центры 20 полос (от 60 Гц до 16 кГц по логарифмической шкале)
        let mut center_freqs = [0.0f32; AUDIO_SPECTROGRAM_BINS];
        let f_min = 60.0f32;
        let f_max = 16000.0f32;
        for i in 0..AUDIO_SPECTROGRAM_BINS {
            center_freqs[i] = f_min * (f_max / f_min).powf(i as f32 / (AUDIO_SPECTROGRAM_BINS - 1) as f32);
        }

        while is_running.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(20)); // Ровно 50 тиков в секунду (для 10с буфера из 500 семплов)

            let mut pcm_captured: Vec<i16> = Vec::new();
            let mut next_packet_size = capture_client.GetNextPacketSize()?;

            while next_packet_size > 0 {
                let mut p_data: *mut u8 = std::ptr::null_mut();
                let mut num_frames_read = 0u32;
                let mut flags = 0u32;

                let hr = capture_client.GetBuffer(
                    &mut p_data,
                    &mut num_frames_read,
                    &mut flags,
                    None,
                    None,
                );

                if hr.is_ok() && num_frames_read > 0 && !p_data.is_null() {
                    let is_silent = (flags & 0x2) != 0;

                    if is_silent {
                        for _ in 0..num_frames_read {
                            sample_ring.push(0.0);
                            pcm_captured.push(0);
                        }
                    } else if bits_per_sample == 32 {
                        let float_ptr = p_data as *const f32;
                        for f in 0..num_frames_read as usize {
                            let mut sum = 0.0f32;
                            for c in 0..channels {
                                sum += *float_ptr.add(f * channels + c);
                            }
                            let mono = sum / channels as f32;
                            sample_ring.push(mono);
                            pcm_captured.push((mono.clamp(-1.0, 1.0) * 32767.0) as i16);
                        }
                    } else if bits_per_sample == 16 {
                        let i16_ptr = p_data as *const i16;
                        for f in 0..num_frames_read as usize {
                            let mut sum = 0.0f32;
                            for c in 0..channels {
                                sum += *i16_ptr.add(f * channels + c) as f32 / 32768.0;
                            }
                            let mono = sum / channels as f32;
                            sample_ring.push(mono);
                            pcm_captured.push((mono.clamp(-1.0, 1.0) * 32767.0) as i16);
                        }
                    }

                    capture_client.ReleaseBuffer(num_frames_read)?;
                }

                next_packet_size = capture_client.GetNextPacketSize()?;
            }

            let pcm_48k = if (sample_rate - 48000.0).abs() > 100.0 && !pcm_captured.is_empty() {
                resample_i16_to_48k(&pcm_captured, sample_rate as usize)
            } else {
                pcm_captured
            };

            let had_new_samples = !pcm_48k.is_empty();
            if !had_new_samples {
                sample_ring.clear();
            } else if sample_ring.len() > 1024 {
                let excess = sample_ring.len() - 1024;
                sample_ring.drain(0..excess);
            }

            let mut packet = AudioSpectrumPacket {
                peak_db: -60.0,
                min_sample: 0.0,
                max_sample: 0.0,
                rms: 0.0,
                bins: [0.01; AUDIO_SPECTROGRAM_BINS],
                pcm_chunk: pcm_48k,
            };

            if had_new_samples && !packet.pcm_chunk.is_empty() {
                let mut sum_sq = 0.0f64;
                let mut min_val = 0.0f32;
                let mut max_val = 0.0f32;
                for &s_i16 in &packet.pcm_chunk {
                    let s = s_i16 as f32 / 32768.0;
                    sum_sq += (s * s) as f64;
                    if s < min_val { min_val = s; }
                    if s > max_val { max_val = s; }
                }
                let count = packet.pcm_chunk.len() as f64;
                let rms = (sum_sq / count).sqrt() as f32;
                packet.rms = rms;
                packet.min_sample = min_val;
                packet.max_sample = max_val;
                packet.peak_db = (20.0 * rms.max(1e-4).log10()).clamp(-60.0, 0.0);

                let n = sample_ring.len();
                if n >= 256 {
                    let analysis_len = n.min(512);
                    let start_idx = n - analysis_len;
                    let slice = &sample_ring[start_idx..];

                    for (b, &fc) in center_freqs.iter().enumerate() {
                        let k = fc / sample_rate * analysis_len as f32;
                        let omega = 2.0 * PI * k / analysis_len as f32;
                        let mut real = 0.0f32;
                        let mut imag = 0.0f32;

                        for (m, &val) in slice.iter().enumerate() {
                            let w = 0.5 * (1.0 - (2.0 * PI * m as f32 / analysis_len as f32).cos());
                            let sample = val * w;
                            let angle = omega * m as f32;
                            real += sample * angle.cos();
                            imag -= sample * angle.sin();
                        }

                        let mag = (real * real + imag * imag).sqrt() / (analysis_len as f32 * 0.25);
                        let energy = (mag * 6.5).clamp(0.01, 0.99);
                        packet.bins[b] = energy;
                    }
                }
            }

            let _ = tx.try_send(packet);
        }
        let _ = audio_client.Stop();
    }
    Ok(())
}
