#![allow(dead_code, unused_variables)]

use std::f32::consts::PI;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::thread;
use std::time::Duration;
use windows::core::{Interface, Result, GUID};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, IAudioCaptureClient, IAudioClient,
    IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
    WAVEFORMATEX,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED,
};

pub const AUDIO_SPECTROGRAM_BINS: usize = 20;

#[derive(Clone, Debug)]
pub struct AudioSpectrumPacket {
    pub peak_db: f32,
    pub bins: [f32; AUDIO_SPECTROGRAM_BINS],
}

/// Запускает фоновый захват реального звука через Windows WASAPI
/// is_loopback = true  -> Звук системы/игры (WASAPI Loopback Capture с колонок/наушников)
/// is_loopback = false -> Микрофон (Default Audio Capture Endpoint)
pub fn start_wasapi_audio_worker(is_loopback: bool) -> Receiver<AudioSpectrumPacket> {
    let (tx, rx) = sync_channel(8);

    thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }

        loop {
            let res = run_wasapi_capture_loop(is_loopback, &tx);
            if res.is_err() {
                // Если устройство временно недоступно (например, отключено в настройках звука),
                // отправляем тишину и пробуем переподключиться через 1 сек
                let silence = AudioSpectrumPacket {
                    peak_db: -60.0,
                    bins: [0.01; AUDIO_SPECTROGRAM_BINS],
                };
                let _ = tx.try_send(silence);
                thread::sleep(Duration::from_millis(1000));
            }
        }

        #[allow(unreachable_code)]
        unsafe {
            CoUninitialize();
        }
    });

    rx
}

fn run_wasapi_capture_loop(is_loopback: bool, tx: &SyncSender<AudioSpectrumPacket>) -> Result<()> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;

        let data_flow = if is_loopback { eRender } else { eCapture };
        let device: IMMDevice = enumerator.GetDefaultAudioEndpoint(data_flow, eConsole)?;

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

        loop {
            thread::sleep(Duration::from_millis(35)); // ~28-30 FPS для спектрограммы

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
                    // Флаг AUDCLNT_BUFFERFLAGS_SILENT = 0x2
                    let is_silent = (flags & 0x2) != 0;

                    if is_silent {
                        for _ in 0..num_frames_read {
                            sample_ring.push(0.0);
                        }
                    } else if bits_per_sample == 32 {
                        let float_ptr = p_data as *const f32;
                        for f in 0..num_frames_read as usize {
                            // Микшируем стерео/многоканал в моно
                            let mut sum = 0.0f32;
                            for c in 0..channels {
                                sum += *float_ptr.add(f * channels + c);
                            }
                            sample_ring.push(sum / channels as f32);
                        }
                    } else if bits_per_sample == 16 {
                        let i16_ptr = p_data as *const i16;
                        for f in 0..num_frames_read as usize {
                            let mut sum = 0.0f32;
                            for c in 0..channels {
                                sum += *i16_ptr.add(f * channels + c) as f32 / 32768.0;
                            }
                            sample_ring.push(sum / channels as f32);
                        }
                    }

                    capture_client.ReleaseBuffer(num_frames_read)?;
                }

                next_packet_size = capture_client.GetNextPacketSize()?;
            }

            // Оставляем в кольцевом буфере последние 1024 семпла
            if sample_ring.len() > 1024 {
                let excess = sample_ring.len() - 1024;
                sample_ring.drain(0..excess);
            }

            // Вычисляем RMS и 20 спектральных бинов по реальным данным
            let mut packet = AudioSpectrumPacket {
                peak_db: -60.0,
                bins: [0.01; AUDIO_SPECTROGRAM_BINS],
            };

            let n = sample_ring.len();
            if n >= 256 {
                // 1. RMS
                let mut sum_sq = 0.0f32;
                for &s in &sample_ring {
                    sum_sq += s * s;
                }
                let rms = (sum_sq / n as f32).sqrt();
                packet.peak_db = (20.0 * rms.max(1e-4).log10()).clamp(-60.0, 0.0);

                // 2. Спектральные бины через быстрое вычисление мощности частоты с окном Ханна
                let analysis_len = n.min(512);
                let start_idx = n - analysis_len;
                let slice = &sample_ring[start_idx..];

                for (b, &fc) in center_freqs.iter().enumerate() {
                    let k = fc / sample_rate * analysis_len as f32;
                    let omega = 2.0 * PI * k / analysis_len as f32;

                    let mut real = 0.0f32;
                    let mut imag = 0.0f32;

                    for (m, &val) in slice.iter().enumerate() {
                        // Окно Ханна: 0.5 * (1 - cos(2*pi*m / N))
                        let w = 0.5 * (1.0 - (2.0 * PI * m as f32 / analysis_len as f32).cos());
                        let sample = val * w;
                        let angle = omega * m as f32;
                        real += sample * angle.cos();
                        imag -= sample * angle.sin();
                    }

                    let mag = (real * real + imag * imag).sqrt() / (analysis_len as f32 * 0.25);
                    // Логарифмическая компрессия для контрастного отображения на спектрограмме
                    let energy = (mag * 6.5).clamp(0.01, 0.99);
                    packet.bins[b] = energy;
                }
            }

            let _ = tx.try_send(packet);
        }
    }
}
