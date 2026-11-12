#![allow(dead_code, unused_variables, unused_imports)]

use crate::audio::{start_wasapi_audio_worker, AudioSpectrumPacket, AUDIO_SPECTROGRAM_BINS};
use crate::benchmark::{build_benchmark_matrix, run_full_benchmark, BenchmarkResult, MatrixCell, MatrixRow};
use crate::camera::{start_camera_worker, CameraFrame};
use crate::capture::{capture_desktop_gdi, get_screen_resolution, start_desktop_preview_worker, CaptureStats, PreviewFrame};
use crate::latest::LatestSlot;
use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, Stroke, Vec2};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

pub const WAVEFORM_SAMPLES: usize = 500; // 500 срезов истории (~10 секунд)

// Google Material Icons символы (шрифт MaterialIcons-Regular.ttf)
pub const ICON_MIC: &str = "\u{e029}";
pub const ICON_MIC_OFF: &str = "\u{e02b}";
pub const ICON_HEADSET: &str = "\u{e310}";
pub const ICON_HEADSET_OFF: &str = "\u{e33a}";
pub const ICON_VIDEOCAM: &str = "\u{e04b}";
pub const ICON_VIDEOCAM_OFF: &str = "\u{e04c}";
pub const ICON_SCREEN_SHARE: &str = "\u{e0e2}";
pub const ICON_STOP_SCREEN_SHARE: &str = "\u{e0e3}";
pub const ICON_CALL_END: &str = "\u{e0b1}";
pub const ICON_SUN: &str = "\u{e430}";
pub const ICON_MOON: &str = "\u{e51c}";
pub const ICON_FULLSCREEN: &str = "\u{e5d0}";
pub const ICON_FULLSCREEN_EXIT: &str = "\u{e5d1}";
pub const ICON_SETTINGS: &str = "\u{e8b8}";
pub const ICON_REFRESH: &str = "\u{e5d5}";
pub const ICON_LANGUAGE: &str = "\u{e894}";

pub const HERO_MJ_BYTES: &[u8] = include_bytes!("../assets/hero_mj.png");

pub use crate::i18n::AppLanguage;

#[derive(Clone, Copy)]
pub struct WaveformPoint {
    pub min_val: f32,
    pub max_val: f32,
    pub rms: f32,
}

#[derive(Clone)]
pub struct AudioWaveform {
    pub history: VecDeque<WaveformPoint>,
    pub peak_db: f32,
    pub volume: f32,
    pub is_muted: bool,
    pub smoothed_rms: f32,
}

impl AudioWaveform {
    pub fn new() -> Self {
        let mut history = VecDeque::with_capacity(WAVEFORM_SAMPLES);
        for _ in 0..WAVEFORM_SAMPLES {
            history.push_back(WaveformPoint { min_val: 0.0, max_val: 0.0, rms: 0.0 });
        }
        Self {
            history,
            peak_db: -60.0,
            volume: 1.0,
            is_muted: false,
            smoothed_rms: 0.0,
        }
    }

    /// Добавление реального среза звуковой волны (10 сек буфер на черном фоне)
    pub fn push_real_sample(&mut self, min_val: f32, max_val: f32, rms: f32, peak_db: f32) {
        if self.is_muted {
            self.peak_db = -60.0;
            self.smoothed_rms = 0.0;
            self.history.pop_front();
            self.history.push_back(WaveformPoint { min_val: 0.0, max_val: 0.0, rms: 0.0 });
        } else {
            let vol = self.volume.clamp(0.0, 2.0);
            self.peak_db = (peak_db + (vol - 1.0) * 12.0).clamp(-60.0, 0.0);
            let scaled_min = (min_val * vol).clamp(-1.0, 1.0);
            let scaled_max = (max_val * vol).clamp(-1.0, 1.0);
            let scaled_rms = (rms * vol).clamp(0.0, 1.0);

            // Плавное сглаживание для анимации пузырька громкости в кнопке
            if scaled_rms > self.smoothed_rms {
                self.smoothed_rms = self.smoothed_rms * 0.35 + scaled_rms * 0.65;
            } else {
                self.smoothed_rms = self.smoothed_rms * 0.82 + scaled_rms * 0.18;
            }

            self.history.pop_front();
            self.history.push_back(WaveformPoint {
                min_val: scaled_min,
                max_val: scaled_max,
                rms: scaled_rms,
            });
        }
    }
}

/// Цветовая шкала спектрограммы (неоново-технологичный градиент для темной темы)
fn spectrogram_color(val: f32) -> Color32 {
    let v = val.clamp(0.0, 1.0);
    if v < 0.05 {
        Color32::from_rgb(20, 22, 28) // Глубокий темный фон
    } else if v < 0.25 {
        let t = (v - 0.05) / 0.20;
        let r = (20.0 * (1.0 - t) + 30.0 * t) as u8;
        let g = (30.0 * (1.0 - t) + 120.0 * t) as u8;
        let b = (80.0 * (1.0 - t) + 245.0 * t) as u8;
        Color32::from_rgb(r, g, b)
    } else if v < 0.55 {
        let t = (v - 0.25) / 0.30;
        let r = (30.0 * (1.0 - t) + 35.0 * t) as u8;
        let g = (120.0 * (1.0 - t) + 220.0 * t) as u8;
        let b = (245.0 * (1.0 - t) + 120.0 * t) as u8;
        Color32::from_rgb(r, g, b)
    } else if v < 0.80 {
        let t = (v - 0.55) / 0.25;
        let r = (35.0 * (1.0 - t) + 245.0 * t) as u8;
        let g = (220.0 * (1.0 - t) + 200.0 * t) as u8;
        let b = (120.0 * (1.0 - t) + 30.0 * t) as u8;
        Color32::from_rgb(r, g, b)
    } else {
        let t = (v - 0.80) / 0.20;
        let r = (245.0 * (1.0 - t) + 255.0 * t) as u8;
        let g = (200.0 * (1.0 - t) + 60.0 * t) as u8;
        let b = (30.0 * (1.0 - t) + 40.0 * t) as u8;
        Color32::from_rgb(r, g, b)
    }
}

/// Расчет прямоугольника с идеальным сохранением соотношения сторон (16:9 без сплющивания/ужатия)
pub fn aspect_fit(avail_rect: Rect, content_w: f32, content_h: f32) -> Rect {
    if content_w <= 0.0 || content_h <= 0.0 || avail_rect.width() <= 0.0 || avail_rect.height() <= 0.0 {
        return avail_rect;
    }
    let target_ratio = content_w / content_h;
    let avail_ratio = avail_rect.width() / avail_rect.height();

    let (fit_w, fit_h) = if avail_ratio > target_ratio {
        (avail_rect.height() * target_ratio, avail_rect.height())
    } else {
        (avail_rect.width(), avail_rect.width() / target_ratio)
    };

    Rect::from_center_size(avail_rect.center(), Vec2::new(fit_w, fit_h))
}

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum PresetMode {
    AutoSmart,
    ManualP1,
    ManualP3,
    ManualP4,
    ManualP5,
    ManualP6,
    ManualP7,
}

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum StageSource {
    RemoteDesktop,
    RemoteCamera,
    LocalDesktop,
    LocalCamera,
}

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum AppTheme {
    DarkDiscord,
    OledMidnight,
    LightClean,
}

#[derive(Clone, Copy)]
pub struct ThemeColors {
    pub bg_app: Color32,
    pub bg_top_nav: Color32,
    pub bg_card: Color32,
    pub bg_card_secondary: Color32,
    pub bg_stage: Color32,
    pub bg_dock: Color32,
    pub border: Color32,
    pub border_faint: Color32,
    pub text_main: Color32,
    pub text_sub: Color32,
    pub pill_inactive_bg: Color32,
    pub pill_inactive_fg: Color32,
    pub pill_active_bg: Color32,
    pub pill_active_fg: Color32,
}

impl AppTheme {
    pub fn colors(&self) -> ThemeColors {
        match self {
            AppTheme::DarkDiscord => ThemeColors {
                bg_app: Color32::from_rgb(17, 18, 20),
                bg_top_nav: Color32::from_rgb(30, 31, 34),
                bg_card: Color32::from_rgb(26, 28, 32),
                bg_card_secondary: Color32::from_rgb(18, 20, 24),
                bg_stage: Color32::from_rgb(14, 16, 20),
                bg_dock: Color32::from_rgb(30, 31, 36),
                border: Color32::from_rgb(45, 48, 56),
                border_faint: Color32::from_rgb(38, 41, 48),
                text_main: Color32::from_rgb(242, 243, 245),
                text_sub: Color32::from_rgb(160, 170, 185),
                pill_inactive_bg: Color32::from_rgb(45, 48, 56),
                pill_inactive_fg: Color32::WHITE,
                pill_active_bg: Color32::WHITE,
                pill_active_fg: Color32::from_rgb(18, 20, 26),
            },
            AppTheme::OledMidnight => ThemeColors {
                bg_app: Color32::BLACK,
                bg_top_nav: Color32::from_rgb(10, 10, 12),
                bg_card: Color32::from_rgb(14, 14, 18),
                bg_card_secondary: Color32::from_rgb(8, 8, 10),
                bg_stage: Color32::BLACK,
                bg_dock: Color32::from_rgb(16, 16, 20),
                border: Color32::from_rgb(32, 32, 40),
                border_faint: Color32::from_rgb(22, 22, 28),
                text_main: Color32::WHITE,
                text_sub: Color32::from_rgb(180, 185, 200),
                pill_inactive_bg: Color32::from_rgb(22, 22, 28),
                pill_inactive_fg: Color32::WHITE,
                pill_active_bg: Color32::from_rgb(88, 101, 242),
                pill_active_fg: Color32::WHITE,
            },
            AppTheme::LightClean => ThemeColors {
                bg_app: Color32::from_rgb(240, 242, 247),
                bg_top_nav: Color32::WHITE,
                bg_card: Color32::WHITE,
                bg_card_secondary: Color32::from_rgb(244, 246, 250),
                bg_stage: Color32::from_rgb(230, 234, 242),
                bg_dock: Color32::WHITE,
                border: Color32::from_rgb(212, 218, 230),
                border_faint: Color32::from_rgb(225, 230, 240),
                text_main: Color32::from_rgb(20, 24, 32),
                text_sub: Color32::from_rgb(80, 88, 104),
                pill_inactive_bg: Color32::from_rgb(228, 232, 242),
                pill_inactive_fg: Color32::from_rgb(30, 34, 44),
                pill_active_bg: Color32::from_rgb(88, 101, 242),
                pill_active_fg: Color32::WHITE,
            },
        }
    }
}

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Tab {
    Lobby,
    CallAndStream,
    BenchmarkMatrix,
    AudioAndSpectrum,
    EncoderSettings,
}

pub struct NanocordApp {
    // Вкладка
    pub active_tab: Tab,

    // Тема оформления
    pub app_theme: AppTheme,
    pub applied_theme: Option<AppTheme>,

    // Язык интерфейса
    pub app_language: AppLanguage,

    // Режим Fullscreen
    pub is_fullscreen: bool,

    // Управление интерфейсом
    pub is_sidebar_collapsed: bool,
    pub fit_aspect_ratio: bool,

    // Главный экран и источники
    pub active_stage: StageSource,
    pub is_streaming: Arc<AtomicBool>,
    pub is_mic_muted: bool,
    pub is_deafened: bool,
    pub is_camera_on: bool,
    pub is_camera_on_flag: Arc<AtomicBool>,
    pub is_camera_pip: bool,

    // Реальный захват рабочего стола (DXGI + GDI)
    pub desktop_texture: Option<egui::TextureHandle>,
    preview_rx: Option<Arc<LatestSlot<PreviewFrame>>>,
    preview_stats: Arc<CaptureStats>,
    target_fps_shared: Arc<AtomicU32>,

    // Реальный захват веб-камеры (Media Foundation / nokhwa)
    pub camera_texture: Option<egui::TextureHandle>,
    camera_rx: Option<Receiver<CameraFrame>>,

    // Реальный захват аудио (Windows WASAPI)
    mic_audio_rx: Option<Receiver<AudioSpectrumPacket>>,
    mic_is_running: Arc<AtomicBool>,
    game_audio_rx: Option<Receiver<AudioSpectrumPacket>>,
    game_is_running: Arc<AtomicBool>,

    // Воспроизведение звука и устройства
    pub audio_player: Arc<crate::audio::WasapiAudioPlayer>,
    pub listen_to_mic_sidetone: bool,
    pub mic_agc: crate::audio::AutoGain,
    pub input_devices: Vec<crate::audio::AudioDevice>,
    pub output_devices: Vec<crate::audio::AudioDevice>,
    pub selected_input_device_idx: usize,
    pub selected_output_device_idx: usize,
    pub applied_input_device_id: String,
    pub applied_output_device_id: String,

    // Разрешение экрана и стрима
    pub native_width: u32,
    pub native_height: u32,
    pub native_hz: u32,
    pub available_resolutions: Vec<(u32, u32, &'static str)>,
    pub selected_res_idx: usize,

    // Параметры энкодера
    pub target_fps: u32,
    pub preset_mode: PresetMode,
    pub manual_preset: u8,
    pub cq_quality: u8,
    pub max_bitrate_mbps: u32,
    pub encoder_config: Arc<std::sync::RwLock<crate::network::VideoEncoderConfig>>,

    // Двухканальная звуковая волна (10 секунд истории)
    pub mic_waveform: AudioWaveform,
    pub game_waveform: AudioWaveform,
    pub anim_time: f32,

    pub mic_opus_encoder: crate::audio::NanocordOpusEncoder,
    pub game_opus_encoder: crate::audio::NanocordOpusEncoder,
    pub peer_mic_opus_decoder: crate::audio::NanocordOpusDecoder,
    pub peer_game_opus_decoder: crate::audio::NanocordOpusDecoder,

    // Сетевой P2P сервер и клиент (IPv6 + IPv4)
    pub p2p_server: Option<crate::network::P2PNetworkServer>,
    pub p2p_client: Option<crate::network::P2PNetworkClient>,
    pub remote_desktop_texture: Option<egui::TextureHandle>,
    pub remote_camera_texture: Option<egui::TextureHandle>,
    pub last_remote_desktop_time: Option<Instant>,
    pub last_remote_camera_time: Option<Instant>,

    // Герой-маскот и внешний публичный IP в обход прокси
    pub hero_mj_texture: Option<egui::TextureHandle>,
    pub public_ip: Arc<std::sync::RwLock<Option<String>>>,
    pub is_fetching_ip: Arc<std::sync::atomic::AtomicBool>,
    pub downmix_to_mono: bool,

    // Поля начального экрана (Комната / Lobby)
    pub server_port_str: String,
    pub server_code_str: String,
    pub client_target_str: String,
    pub client_status_msg: String,
    pub copy_toast: Option<(String, Instant)>,
    pub local_ipv6_hint: String,
    pub local_ipv4_hint: String,

    // Бенчмарк матрица
    pub benchmark_results: Option<Vec<BenchmarkResult>>,
    pub benchmark_matrix: Option<Vec<MatrixRow>>,
    pub is_benchmarking: bool,
    bench_rx: Option<Receiver<Vec<BenchmarkResult>>>,

    screenshot_taken: bool,
}

impl Default for NanocordApp {
    fn default() -> Self {
        let (native_w, native_h) = get_screen_resolution();
        let native_hz = crate::capture::get_screen_refresh_rate();

        let mut available = vec![
            (854, 480, "480p SD"),
            (1280, 720, "720p HD"),
            (1920, 1080, "1080p Full HD"),
        ];

        if native_w >= 2560 && native_h >= 1440 {
            available.push((2560, 1440, "1440p 2K"));
        }
        if native_w >= 3840 && native_h >= 2160 {
            available.push((3840, 2160, "4K UHD"));
        }

        let default_res_idx = available.len().saturating_sub(1);
        let default_matrix = build_default_matrix(native_w, native_h, native_hz);

        let is_streaming = Arc::new(AtomicBool::new(false));
        let is_camera_on_flag = Arc::new(AtomicBool::new(false));
        let encoder_config = Arc::new(std::sync::RwLock::new(crate::network::VideoEncoderConfig::default()));

        // 1. Запуск настоящего фонового захвата экрана в полном нативном разрешении (30 FPS при стриме, 1 FPS в режиме паузы)
        let capture_stats = Arc::new(CaptureStats::default());
        let target_fps_shared = Arc::new(AtomicU32::new(30));
        let rx_desktop = start_desktop_preview_worker(
            native_w,
            native_h,
            target_fps_shared.clone(),
            is_streaming.clone(),
            capture_stats.clone(),
        );

        // 2. Запуск настоящего фонового захвата веб-камеры (спит с 0% CPU/GPU, пока камера выключена)
        let rx_camera = start_camera_worker(is_camera_on_flag.clone());

        // 3. Запуск реального захвата аудио через Windows WASAPI (микрофон и loopback системы)
        let mic_is_running = Arc::new(AtomicBool::new(true));
        let rx_mic = start_wasapi_audio_worker(false, None, mic_is_running.clone());
        let game_is_running = Arc::new(AtomicBool::new(true));
        let rx_game = start_wasapi_audio_worker(true, None, game_is_running.clone());

        // 4. Сохранение контрольного скриншота экрана для инспекции
        if let Some(rgba) = capture_desktop_gdi(native_w, native_h) {
            let path = "C:\\Users\\User\\.gemini\\antigravity\\brain\\7c6de5f1-98be-4f2e-bcbc-6dffdeafac75\\screenshot.png";
            let _ = image::save_buffer(path, &rgba, native_w, native_h, image::ColorType::Rgba8);
        }

        let (hint_v6, hint_v4) = crate::network::get_local_ip_hint();
        let in_devs = crate::audio::enumerate_audio_devices(true);
        let out_devs = crate::audio::enumerate_audio_devices(false);
        let initial_in_id = in_devs.get(0).map(|d| d.id.clone()).unwrap_or_else(|| "default".to_string());
        let initial_out_id = out_devs.get(0).map(|d| d.id.clone()).unwrap_or_else(|| "default".to_string());

        let public_ip = Arc::new(std::sync::RwLock::new(None));
        let is_fetching_ip = Arc::new(AtomicBool::new(true));

        {
            let ip_clone = public_ip.clone();
            let is_fetching_clone = is_fetching_ip.clone();
            std::thread::spawn(move || {
                if let Some(ip) = crate::network::fetch_real_public_ip() {
                    if let Ok(mut lock) = ip_clone.write() {
                        *lock = Some(ip);
                    }
                }
                is_fetching_clone.store(false, Ordering::Relaxed);
            });
        }

        Self {
            active_tab: Tab::Lobby,
            app_theme: AppTheme::DarkDiscord,
            applied_theme: None,
            app_language: AppLanguage::Ru,
            is_fullscreen: false,
            is_sidebar_collapsed: false,
            fit_aspect_ratio: true,

            active_stage: StageSource::RemoteDesktop,
            is_streaming,
            is_mic_muted: false,
            is_deafened: false,
            is_camera_on: false,
            is_camera_on_flag,
            is_camera_pip: false,

            desktop_texture: None,
            preview_rx: Some(rx_desktop),
            preview_stats: capture_stats,
            target_fps_shared,

            camera_texture: None,
            camera_rx: Some(rx_camera),

            mic_audio_rx: Some(rx_mic),
            mic_is_running,
            game_audio_rx: Some(rx_game),
            game_is_running,

            audio_player: Arc::new(crate::audio::WasapiAudioPlayer::new()),
            listen_to_mic_sidetone: false,
            mic_agc: crate::audio::AutoGain::default(),
            input_devices: in_devs,
            output_devices: out_devs,
            selected_input_device_idx: 0,
            selected_output_device_idx: 0,
            applied_input_device_id: initial_in_id,
            applied_output_device_id: initial_out_id,

            native_width: native_w,
            native_height: native_h,
            native_hz,
            available_resolutions: available,
            selected_res_idx: default_res_idx,

            target_fps: 30,
            preset_mode: PresetMode::AutoSmart,
            manual_preset: 3,
            cq_quality: 22,
            max_bitrate_mbps: 30,
            encoder_config,

            mic_waveform: AudioWaveform::new(),
            game_waveform: AudioWaveform::new(),
            anim_time: 0.0,

            mic_opus_encoder: crate::audio::NanocordOpusEncoder::new_voip(64_000),
            game_opus_encoder: crate::audio::NanocordOpusEncoder::new_audio(96_000),
            peer_mic_opus_decoder: crate::audio::NanocordOpusDecoder::new(),
            peer_game_opus_decoder: crate::audio::NanocordOpusDecoder::new(),

            p2p_server: None,
            p2p_client: None,
            remote_desktop_texture: None,
            remote_camera_texture: None,
            last_remote_desktop_time: None,
            last_remote_camera_time: None,

            hero_mj_texture: None,
            public_ip,
            is_fetching_ip,
            downmix_to_mono: true,

            server_port_str: "48120".to_string(),
            server_code_str: crate::network::generate_random_room_code(),
            client_target_str: String::new(),
            client_status_msg: "Не подключен".to_string(),
            copy_toast: None,
            local_ipv6_hint: hint_v6,
            local_ipv4_hint: hint_v4,

            benchmark_results: None,
            benchmark_matrix: Some(default_matrix),
            is_benchmarking: false,
            bench_rx: None,
            screenshot_taken: true,
        }
    }
}

pub fn apply_theme_mode(ctx: &egui::Context, theme: AppTheme) {
    match theme {
        AppTheme::DarkDiscord => {
            let mut visuals = egui::Visuals::dark();
            visuals.dark_mode = true;
            visuals.override_text_color = Some(Color32::from_rgb(242, 243, 245));
            visuals.panel_fill = Color32::from_rgb(17, 18, 20); // Discord Deep Dark
            visuals.window_fill = Color32::from_rgb(22, 24, 28);
            visuals.faint_bg_color = Color32::from_rgb(30, 31, 34);
            visuals.extreme_bg_color = Color32::from_rgb(12, 13, 15);

            visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(30, 31, 34);
            visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(45, 48, 54));
            visuals.widgets.noninteractive.corner_radius = CornerRadius::same(8);
            visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(215, 220, 228));

            visuals.widgets.inactive.bg_fill = Color32::from_rgb(43, 45, 49);
            visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(56, 58, 64));
            visuals.widgets.inactive.corner_radius = CornerRadius::same(8);
            visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(242, 243, 245));

            visuals.widgets.hovered.bg_fill = Color32::from_rgb(60, 63, 69);
            visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(88, 101, 242));
            visuals.widgets.hovered.corner_radius = CornerRadius::same(8);
            visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);

            visuals.widgets.active.bg_fill = Color32::from_rgb(88, 101, 242);
            visuals.widgets.active.bg_stroke = Stroke::new(1.0, Color32::from_rgb(71, 82, 196));
            visuals.widgets.active.corner_radius = CornerRadius::same(8);
            visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);

            visuals.slider_trailing_fill = true;
            ctx.set_visuals(visuals);
        }
        AppTheme::OledMidnight => {
            let mut visuals = egui::Visuals::dark();
            visuals.dark_mode = true;
            visuals.override_text_color = Some(Color32::WHITE);
            visuals.panel_fill = Color32::BLACK;
            visuals.window_fill = Color32::from_rgb(8, 8, 10);
            visuals.faint_bg_color = Color32::from_rgb(16, 16, 20);
            visuals.extreme_bg_color = Color32::BLACK;
            visuals.slider_trailing_fill = true;

            visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(14, 14, 18);
            visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(32, 32, 40));
            visuals.widgets.noninteractive.corner_radius = CornerRadius::same(8);
            visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(220, 225, 235));

            visuals.widgets.inactive.bg_fill = Color32::from_rgb(22, 22, 28);
            visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(44, 44, 54));
            visuals.widgets.inactive.corner_radius = CornerRadius::same(8);
            visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::WHITE);

            visuals.widgets.hovered.bg_fill = Color32::from_rgb(38, 38, 50);
            visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(88, 101, 242));
            visuals.widgets.hovered.corner_radius = CornerRadius::same(8);
            visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);

            visuals.widgets.active.bg_fill = Color32::from_rgb(88, 101, 242);
            visuals.widgets.active.bg_stroke = Stroke::new(1.0, Color32::WHITE);
            visuals.widgets.active.corner_radius = CornerRadius::same(8);
            visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);

            ctx.set_visuals(visuals);
        }
        AppTheme::LightClean => {
            let mut visuals = egui::Visuals::light();
            visuals.dark_mode = false;
            visuals.override_text_color = Some(Color32::from_rgb(20, 24, 34));
            visuals.panel_fill = Color32::from_rgb(240, 242, 247);
            visuals.window_fill = Color32::WHITE;
            visuals.faint_bg_color = Color32::from_rgb(235, 238, 245);
            visuals.extreme_bg_color = Color32::from_rgb(215, 222, 235);
            visuals.slider_trailing_fill = true;

            visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(235, 238, 245);
            visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(205, 212, 224));
            visuals.widgets.noninteractive.corner_radius = CornerRadius::same(8);
            visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(30, 32, 40));

            // Inactive widgets (slider rails, default buttons, inputs): clearly visible light slate blue
            visuals.widgets.inactive.bg_fill = Color32::from_rgb(218, 225, 238);
            visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(180, 192, 212));
            visuals.widgets.inactive.corner_radius = CornerRadius::same(8);
            visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(20, 24, 34));

            visuals.widgets.hovered.bg_fill = Color32::from_rgb(205, 215, 235);
            visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(88, 101, 242));
            visuals.widgets.hovered.corner_radius = CornerRadius::same(8);
            visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::from_rgb(88, 101, 242));

            visuals.widgets.active.bg_fill = Color32::from_rgb(88, 101, 242);
            visuals.widgets.active.bg_stroke = Stroke::new(1.0, Color32::from_rgb(71, 82, 196));
            visuals.widgets.active.corner_radius = CornerRadius::same(8);
            visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);

            ctx.set_visuals(visuals);
        }
    }
}

pub fn setup_custom_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "material_icons".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!("../assets/MaterialIcons-Regular.ttf"))),
    );
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .push("material_icons".to_owned());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .push("material_icons".to_owned());
    ctx.set_fonts(fonts);
}

pub fn apply_custom_theme(ctx: &egui::Context) {
    setup_custom_fonts(ctx);
    apply_theme_mode(ctx, AppTheme::DarkDiscord);
}

// =========================================================================
// НАДЕЖНЫЕ ВЕКТОРНЫЕ ИКОНКИ ДЛЯ DOCK И ИНТЕРФЕЙСА (НЕ ЗАВИСЯТ ОТ ШРИФТОВ EMOJI)
// =========================================================================
fn draw_vector_mic(painter: &egui::Painter, center: Pos2, icon_col: Color32, is_muted: bool) {
    let cap_rect = Rect::from_center_size(Pos2::new(center.x, center.y - 2.5), Vec2::new(7.0, 12.0));
    painter.rect_filled(cap_rect, CornerRadius::same(3), icon_col);

    let bracket_y = center.y + 3.5;
    painter.line_segment([Pos2::new(center.x - 6.5, center.y - 1.0), Pos2::new(center.x - 6.5, bracket_y)], Stroke::new(1.8, icon_col));
    painter.line_segment([Pos2::new(center.x - 6.5, bracket_y), Pos2::new(center.x + 6.5, bracket_y)], Stroke::new(1.8, icon_col));
    painter.line_segment([Pos2::new(center.x + 6.5, bracket_y), Pos2::new(center.x + 6.5, center.y - 1.0)], Stroke::new(1.8, icon_col));

    painter.line_segment([Pos2::new(center.x, bracket_y), Pos2::new(center.x, bracket_y + 5.0)], Stroke::new(1.8, icon_col));
    painter.line_segment([Pos2::new(center.x - 4.5, bracket_y + 5.0), Pos2::new(center.x + 4.5, bracket_y + 5.0)], Stroke::new(1.8, icon_col));

    if is_muted {
        painter.line_segment([Pos2::new(center.x - 8.0, center.y - 8.0), Pos2::new(center.x + 8.0, center.y + 8.0)], Stroke::new(2.4, Color32::WHITE));
    }
}

fn draw_vector_headphones(painter: &egui::Painter, center: Pos2, icon_col: Color32, is_deafened: bool) {
    let p0 = Pos2::new(center.x - 7.5, center.y + 1.0);
    let p1 = Pos2::new(center.x - 7.5, center.y - 6.0);
    let p2 = Pos2::new(center.x + 7.5, center.y - 6.0);
    let p3 = Pos2::new(center.x + 7.5, center.y + 1.0);
    painter.line_segment([p0, p1], Stroke::new(1.8, icon_col));
    painter.line_segment([p1, p2], Stroke::new(1.8, icon_col));
    painter.line_segment([p2, p3], Stroke::new(1.8, icon_col));

    painter.rect_filled(Rect::from_center_size(Pos2::new(center.x - 7.5, center.y + 2.0), Vec2::new(4.5, 9.0)), CornerRadius::same(2), icon_col);
    painter.rect_filled(Rect::from_center_size(Pos2::new(center.x + 7.5, center.y + 2.0), Vec2::new(4.5, 9.0)), CornerRadius::same(2), icon_col);

    if is_deafened {
        painter.line_segment([Pos2::new(center.x - 8.0, center.y - 8.0), Pos2::new(center.x + 8.0, center.y + 8.0)], Stroke::new(2.4, Color32::WHITE));
    }
}

fn draw_vector_camera(painter: &egui::Painter, center: Pos2, icon_col: Color32, is_off: bool) {
    let body = Rect::from_center_size(Pos2::new(center.x - 2.5, center.y), Vec2::new(13.0, 10.0));
    painter.rect_filled(body, CornerRadius::same(2), icon_col);

    painter.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(center.x + 4.5, center.y - 2.5),
            Pos2::new(center.x + 9.5, center.y - 5.5),
            Pos2::new(center.x + 9.5, center.y + 5.5),
            Pos2::new(center.x + 4.5, center.y + 2.5),
        ],
        icon_col,
        Stroke::NONE,
    ));

    if is_off {
        painter.line_segment([Pos2::new(center.x - 8.0, center.y - 8.0), Pos2::new(center.x + 8.0, center.y + 8.0)], Stroke::new(2.4, Color32::WHITE));
    }
}

fn draw_vector_screen(painter: &egui::Painter, center: Pos2, icon_col: Color32) {
    let screen = Rect::from_center_size(Pos2::new(center.x, center.y - 2.0), Vec2::new(17.0, 12.0));
    painter.rect_stroke(screen, CornerRadius::same(2), Stroke::new(1.8, icon_col), egui::StrokeKind::Inside);
    painter.line_segment([Pos2::new(center.x, center.y + 4.0), Pos2::new(center.x, center.y + 7.5)], Stroke::new(1.8, icon_col));
    painter.line_segment([Pos2::new(center.x - 5.0, center.y + 7.5), Pos2::new(center.x + 5.0, center.y + 7.5)], Stroke::new(1.8, icon_col));
}

fn draw_vector_hangup(painter: &egui::Painter, center: Pos2) {
    // Трубка телефона, обращенная вниз (call_end)
    let p_left = Pos2::new(center.x - 7.5, center.y + 1.0);
    let p_mid = Pos2::new(center.x, center.y - 3.5);
    let p_right = Pos2::new(center.x + 7.5, center.y + 1.0);
    painter.line_segment([p_left, p_mid], Stroke::new(2.4, Color32::WHITE));
    painter.line_segment([p_mid, p_right], Stroke::new(2.4, Color32::WHITE));
    painter.rect_filled(Rect::from_center_size(Pos2::new(p_left.x - 1.0, p_left.y + 2.0), Vec2::new(4.0, 3.5)), CornerRadius::same(1), Color32::WHITE);
    painter.rect_filled(Rect::from_center_size(Pos2::new(p_right.x + 1.0, p_right.y + 2.0), Vec2::new(4.0, 3.5)), CornerRadius::same(1), Color32::WHITE);
}

/// Векторная сова для темы "OLED Midnight" (выразительные глаза и ушки)
fn draw_vector_owl(painter: &egui::Painter, center: Pos2, color: Color32) {
    let ear_l = Pos2::new(center.x - 5.5, center.y - 6.5);
    let ear_r = Pos2::new(center.x + 5.5, center.y - 6.5);

    // Силуэт головы совы
    let head_rect = Rect::from_center_size(Pos2::new(center.x, center.y), Vec2::new(13.0, 11.0));
    painter.rect_filled(head_rect, CornerRadius::same(4), color);

    // Ушки-перышки
    painter.add(egui::Shape::convex_polygon(
        vec![Pos2::new(center.x - 6.0, center.y - 2.0), ear_l, Pos2::new(center.x - 2.0, center.y - 5.0)],
        color,
        Stroke::NONE,
    ));
    painter.add(egui::Shape::convex_polygon(
        vec![Pos2::new(center.x + 2.0, center.y - 5.0), ear_r, Pos2::new(center.x + 6.0, center.y - 2.0)],
        color,
        Stroke::NONE,
    ));

    // Янтарно-желтые круглые глаза
    let eye_bg = Color32::from_rgb(255, 220, 80);
    let pupil = Color32::from_rgb(18, 18, 22);
    painter.circle_filled(Pos2::new(center.x - 3.0, center.y - 0.5), 2.5, eye_bg);
    painter.circle_filled(Pos2::new(center.x + 3.0, center.y - 0.5), 2.5, eye_bg);
    painter.circle_filled(Pos2::new(center.x - 3.0, center.y - 0.5), 1.2, pupil);
    painter.circle_filled(Pos2::new(center.x + 3.0, center.y - 0.5), 1.2, pupil);

    // Клювик оранжевого цвета
    painter.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(center.x - 1.2, center.y + 1.2),
            Pos2::new(center.x + 1.2, center.y + 1.2),
            Pos2::new(center.x, center.y + 3.4),
        ],
        Color32::from_rgb(245, 150, 30),
        Stroke::NONE,
    ));
}

/// Российский флаг (Белый, Синий, Красный)
fn draw_vector_ru_flag(painter: &egui::Painter, rect: Rect) {
    let r = CornerRadius::same(2);
    let h_third = rect.height() / 3.0;
    let top_r = Rect::from_min_max(rect.min, Pos2::new(rect.max.x, rect.min.y + h_third));
    let mid_r = Rect::from_min_max(Pos2::new(rect.min.x, rect.min.y + h_third), Pos2::new(rect.max.x, rect.min.y + 2.0 * h_third));
    let bot_r = Rect::from_min_max(Pos2::new(rect.min.x, rect.min.y + 2.0 * h_third), rect.max);
    painter.rect_filled(top_r, CornerRadius { nw: 2, ne: 2, sw: 0, se: 0 }, Color32::from_rgb(255, 255, 255));
    painter.rect_filled(mid_r, CornerRadius::ZERO, Color32::from_rgb(0, 57, 166));
    painter.rect_filled(bot_r, CornerRadius { nw: 0, ne: 0, sw: 2, se: 2 }, Color32::from_rgb(213, 43, 30));
    painter.rect_stroke(rect, r, Stroke::new(1.0, Color32::from_rgba_unmultiplied(0, 0, 0, 45)), egui::StrokeKind::Inside);
}

/// Американский флаг (Красно-белые полосы и синий кантон со звездой)
fn draw_vector_us_flag(painter: &egui::Painter, rect: Rect) {
    let r = CornerRadius::same(2);
    painter.rect_filled(rect, r, Color32::from_rgb(178, 34, 52));
    let stripe_h = rect.height() / 7.0;
    for &i in &[1.0, 3.0, 5.0] {
        let s_rect = Rect::from_min_max(
            Pos2::new(rect.min.x, rect.min.y + i * stripe_h),
            Pos2::new(rect.max.x, rect.min.y + (i + 1.0) * stripe_h),
        );
        painter.rect_filled(s_rect, CornerRadius::ZERO, Color32::WHITE);
    }
    let canton = Rect::from_min_max(
        rect.min,
        Pos2::new(rect.min.x + rect.width() * 0.45, rect.min.y + stripe_h * 4.0),
    );
    painter.rect_filled(canton, CornerRadius { nw: 2, ne: 0, sw: 0, se: 0 }, Color32::from_rgb(60, 59, 110));
    painter.circle_filled(canton.center(), 1.2, Color32::WHITE);
    painter.rect_stroke(rect, r, Stroke::new(1.0, Color32::from_rgba_unmultiplied(0, 0, 0, 45)), egui::StrokeKind::Inside);
}

/// Тройной сегментный тумблер тем оформления (Солнышко, Ночь, Сова)
fn render_theme_triple_toggle(
    ui: &mut egui::Ui,
    current_theme: &mut AppTheme,
    applied_theme: &mut Option<AppTheme>,
    th: &ThemeColors,
) {
    let pill_w = 98.0;
    let pill_h = 28.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(pill_w, pill_h), egui::Sense::hover());
    let painter = ui.painter();

    painter.rect_filled(rect, CornerRadius::same(14), th.bg_card);
    painter.rect_stroke(rect, CornerRadius::same(14), Stroke::new(1.0, th.border), egui::StrokeKind::Inside);

    let seg_w = (rect.width() - 4.0) / 3.0;
    let themes = [
        (AppTheme::LightClean, "tip_theme_light"),
        (AppTheme::DarkDiscord, "tip_theme_discord"),
        (AppTheme::OledMidnight, "tip_theme_oled"),
    ];

    for (idx, (theme, tip_key)) in themes.iter().enumerate() {
        let seg_rect = Rect::from_min_size(
            Pos2::new(rect.left() + 2.0 + idx as f32 * seg_w, rect.top() + 2.0),
            Vec2::new(seg_w, rect.height() - 4.0),
        );
        let is_selected = *current_theme == *theme;
        let resp = ui.interact(seg_rect, egui::Id::new(format!("theme_seg_{}", idx)), egui::Sense::click());

        if is_selected {
            let active_bg = match theme {
                AppTheme::LightClean => Color32::from_rgb(255, 255, 255),
                AppTheme::DarkDiscord => Color32::from_rgb(88, 101, 242),
                AppTheme::OledMidnight => Color32::from_rgb(28, 30, 40),
            };
            let active_border = match theme {
                AppTheme::LightClean => Color32::from_rgb(195, 205, 225),
                AppTheme::DarkDiscord => Color32::from_rgb(114, 137, 218),
                AppTheme::OledMidnight => Color32::from_rgb(120, 95, 220),
            };
            painter.rect_filled(seg_rect, CornerRadius::same(12), active_bg);
            painter.rect_stroke(seg_rect, CornerRadius::same(12), Stroke::new(1.0, active_border), egui::StrokeKind::Inside);
        } else if resp.hovered() {
            painter.rect_filled(seg_rect, CornerRadius::same(12), Color32::from_rgba_unmultiplied(128, 128, 128, 30));
        }

        let center = seg_rect.center();
        match theme {
            AppTheme::LightClean => {
                let sun_col = if is_selected {
                    Color32::from_rgb(235, 140, 10)
                } else {
                    th.text_sub
                };
                painter.text(center, egui::Align2::CENTER_CENTER, ICON_SUN, egui::FontId::proportional(15.0), sun_col);
            }
            AppTheme::DarkDiscord => {
                let moon_col = if is_selected {
                    Color32::WHITE
                } else {
                    th.text_sub
                };
                painter.text(center, egui::Align2::CENTER_CENTER, ICON_MOON, egui::FontId::proportional(15.0), moon_col);
            }
            AppTheme::OledMidnight => {
                let owl_col = if is_selected {
                    Color32::from_rgb(170, 150, 255)
                } else {
                    th.text_sub
                };
                draw_vector_owl(painter, center, owl_col);
            }
        }

        if resp.clicked() && !is_selected {
            *current_theme = *theme;
            apply_theme_mode(ui.ctx(), *theme);
            *applied_theme = Some(*theme);
        }
        resp.on_hover_text(format!("{} / {}", crate::i18n::t(tip_key, AppLanguage::Ru), crate::i18n::t(tip_key, AppLanguage::En)));
    }
}

/// Сегментный тумблер выбора языка интерфейса (Русский / English)
fn render_language_toggle(
    ui: &mut egui::Ui,
    current_lang: &mut AppLanguage,
    th: &ThemeColors,
) {
    let pill_w = 94.0;
    let pill_h = 28.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(pill_w, pill_h), egui::Sense::hover());
    let painter = ui.painter();

    painter.rect_filled(rect, CornerRadius::same(14), th.bg_card);
    painter.rect_stroke(rect, CornerRadius::same(14), Stroke::new(1.0, th.border), egui::StrokeKind::Inside);

    let seg_w = (rect.width() - 4.0) / 2.0;
    let langs = [
        (AppLanguage::Ru, "RU", "tip_lang_ru"),
        (AppLanguage::En, "EN", "tip_lang_en"),
    ];

    for (idx, (lang, label, tip_key)) in langs.iter().enumerate() {
        let seg_rect = Rect::from_min_size(
            Pos2::new(rect.left() + 2.0 + idx as f32 * seg_w, rect.top() + 2.0),
            Vec2::new(seg_w, rect.height() - 4.0),
        );
        let is_selected = *current_lang == *lang;
        let resp = ui.interact(seg_rect, egui::Id::new(format!("lang_seg_{}", idx)), egui::Sense::click());

        if is_selected {
            let active_bg = if th.border.r() > 100 && th.bg_app.r() > 200 {
                Color32::from_rgb(255, 255, 255)
            } else {
                Color32::from_rgb(45, 52, 68)
            };
            painter.rect_filled(seg_rect, CornerRadius::same(12), active_bg);
            painter.rect_stroke(seg_rect, CornerRadius::same(12), Stroke::new(1.0, th.border), egui::StrokeKind::Inside);
        } else if resp.hovered() {
            painter.rect_filled(seg_rect, CornerRadius::same(12), Color32::from_rgba_unmultiplied(128, 128, 128, 25));
        }

        let flag_rect = Rect::from_center_size(
            Pos2::new(seg_rect.left() + 11.0, seg_rect.center().y),
            Vec2::new(14.0, 10.0),
        );
        match lang {
            AppLanguage::Ru => draw_vector_ru_flag(painter, flag_rect),
            AppLanguage::En => draw_vector_us_flag(painter, flag_rect),
        }

        let text_col = if is_selected {
            th.text_main
        } else {
            th.text_sub
        };
        painter.text(
            Pos2::new(seg_rect.left() + 22.0, seg_rect.center().y),
            egui::Align2::LEFT_CENTER,
            *label,
            egui::FontId::proportional(11.0),
            text_col,
        );

        if resp.clicked() && !is_selected {
            *current_lang = *lang;
        }
        resp.on_hover_text(crate::i18n::t(tip_key, *current_lang));
    }
}

fn draw_vector_expand(painter: &egui::Painter, center: Pos2, icon_col: Color32) {
    let d = 5.0;
    // Верхний левый угол
    painter.line_segment([Pos2::new(center.x - d, center.y - d + 3.0), Pos2::new(center.x - d, center.y - d)], Stroke::new(1.6, icon_col));
    painter.line_segment([Pos2::new(center.x - d, center.y - d), Pos2::new(center.x - d + 3.0, center.y - d)], Stroke::new(1.6, icon_col));
    // Верхний правый угол
    painter.line_segment([Pos2::new(center.x + d, center.y - d + 3.0), Pos2::new(center.x + d, center.y - d)], Stroke::new(1.6, icon_col));
    painter.line_segment([Pos2::new(center.x + d, center.y - d), Pos2::new(center.x + d - 3.0, center.y - d)], Stroke::new(1.6, icon_col));
    // Нижний левый угол
    painter.line_segment([Pos2::new(center.x - d, center.y + d - 3.0), Pos2::new(center.x - d, center.y + d)], Stroke::new(1.6, icon_col));
    painter.line_segment([Pos2::new(center.x - d, center.y + d), Pos2::new(center.x - d + 3.0, center.y + d)], Stroke::new(1.6, icon_col));
    // Нижний правый угол
    painter.line_segment([Pos2::new(center.x + d, center.y + d - 3.0), Pos2::new(center.x + d, center.y + d)], Stroke::new(1.6, icon_col));
    painter.line_segment([Pos2::new(center.x + d, center.y + d), Pos2::new(center.x + d - 3.0, center.y + d)], Stroke::new(1.6, icon_col));
}

fn draw_vector_avatar(painter: &egui::Painter, center: Pos2, radius: f32, bg_col: Color32, border_col: Color32, figure_col: Color32) {
    painter.circle_filled(center, radius, bg_col);
    painter.circle_stroke(center, radius, Stroke::new(1.5, border_col));

    let head_r = radius * 0.36;
    let head_c = Pos2::new(center.x, center.y - radius * 0.22);
    painter.circle_filled(head_c, head_r, figure_col);

    let shoulder_w = radius * 1.1;
    let shoulder_h = radius * 0.6;
    let shoulder_rect = Rect::from_center_size(
        Pos2::new(center.x, center.y + radius * 0.52),
        Vec2::new(shoulder_w, shoulder_h),
    );
    painter.with_clip_rect(Rect::from_center_size(center, Vec2::splat(radius * 1.95)))
        .rect_filled(shoulder_rect, CornerRadius::same(head_r as u8), figure_col);
}

impl NanocordApp {
    #[inline]
    pub fn t(&self, key: &'static str) -> &'static str {
        crate::i18n::t(key, self.app_language)
    }

    #[inline]
    pub fn tr(&self, ru: &'static str, en: &'static str) -> &'static str {
        match self.app_language {
            AppLanguage::Ru => ru,
            AppLanguage::En => en,
        }
    }

    /// Начальный экран (Lobby): запуск P2P-сервера со случайным кодом, подключение по IPv6/IPv4, буфер обмена
    fn render_lobby(&mut self, ui: &mut egui::Ui) {
        let th = self.app_theme.colors();

        // Отрисовка силуэта Майкла Джексона на заднем плане в левой части экрана с диммированием
        if let Some(ref tex) = self.hero_mj_texture {
            let screen_rect = ui.max_rect();
            let h = screen_rect.height().max(600.0);
            let w = h * (516.0 / 885.0);
            let mj_rect = Rect::from_min_size(
                Pos2::new(screen_rect.left(), screen_rect.top()),
                Vec2::new(w, h),
            );
            // Полупрозрачный диммированный силуэт (~14% непрозрачности)
            let dimmed_tint = Color32::from_rgba_unmultiplied(255, 255, 255, 35);
            ui.painter().image(
                tex.id(),
                mj_rect,
                Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                dimmed_tint,
            );
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let avail_w = ui.available_width();
                let content_w = avail_w.min(840.0).max(320.0);
                let pad_x = ((avail_w - content_w) * 0.5).max(0.0);

                ui.horizontal(|ui| {
                    if pad_x > 0.0 {
                        ui.add_space(pad_x);
                    }

                    ui.vertical(|ui| {
                        ui.set_width(content_w);

                        // Верхний заголовок экрана
                        ui.add_space(14.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                egui::RichText::new(self.t("lobby_title"))
                                    .size(22.0)
                                    .strong()
                                    .color(th.text_main),
                            );
                            ui.add_space(3.0);
                            ui.label(
                                egui::RichText::new(self.t("lobby_subtitle"))
                                    .size(12.5)
                                    .color(th.text_sub),
                            );
                        });
                        ui.add_space(16.0);

                        // Всплывающее уведомление (Toast) о копировании / вставке
                        if let Some((ref msg, time)) = self.copy_toast {
                            if time.elapsed() < Duration::from_secs(3) {
                                ui.vertical_centered(|ui| {
                                    egui::Frame::new()
                                        .fill(Color32::from_rgb(35, 165, 90))
                                        .corner_radius(CornerRadius::same(6))
                                        .inner_margin(egui::Margin::symmetric(16, 6))
                                        .show(ui, |ui| {
                                            ui.label(egui::RichText::new(msg).strong().color(Color32::WHITE));
                                        });
                                });
                                ui.add_space(10.0);
                            } else {
                                self.copy_toast = None;
                            }
                        }

                        // 2 Равные колонки с гарантией отсутствия горизонтального переполнения и дрейфа
                        ui.columns(2, |cols| {
                            // ==========================================
                            // КАРТОЧКА 1: СОЗДАТЬ СЕРВЕР (ХОСТ)
                            // ==========================================
                            egui::Frame::new()
                                .fill(th.bg_card)
                                .stroke(Stroke::new(1.0, th.border))
                                .corner_radius(CornerRadius::same(12))
                                .inner_margin(16.0)
                                .show(&mut cols[0], |ui| {
                                    let col_w = ui.available_width();

                                    ui.horizontal(|ui| {
                                        ui.label(egui::RichText::new(self.t("host_badge")).strong().size(12.0).color(Color32::from_rgb(88, 101, 242)));
                                        ui.label(
                                            egui::RichText::new(self.t("host_card_title"))
                                                .size(15.0)
                                                .strong()
                                                .color(th.text_main),
                                        );
                                    });
                                    ui.add_space(2.0);
                                    ui.label(
                                        egui::RichText::new(self.t("host_card_desc"))
                                            .size(11.0)
                                            .color(th.text_sub),
                                    );
                                    ui.add_space(12.0);

                                    // Поле: Порт
                                    ui.horizontal(|ui| {
                                        ui.label(egui::RichText::new(self.t("port_label")).size(12.5).color(th.text_main));
                                        ui.add(egui::TextEdit::singleline(&mut self.server_port_str).desired_width(70.0));
                                        ui.label(egui::RichText::new("(IPv6 + IPv4)").weak().size(11.0).color(th.text_sub));
                                    });
                                    ui.add_space(6.0);

                                    // Поле: Случайный код [A-z0-9]
                                    ui.horizontal(|ui| {
                                        ui.label(egui::RichText::new(self.t("access_code_label")).size(12.5).color(th.text_main));
                                        ui.add(egui::TextEdit::singleline(&mut self.server_code_str).desired_width(60.0));
                                        if ui
                                            .button(self.t("random_code_btn"))
                                            .on_hover_text(self.t("random_code_tip"))
                                            .clicked()
                                        {
                                            self.server_code_str = crate::network::generate_random_room_code();
                                        }
                                    });
                                    ui.add_space(12.0);

                                    // Плашка готового адреса для приглашения
                                    let invite_link_v4 = format!(
                                        "{}:{}#{}",
                                        self.local_ipv4_hint,
                                        self.server_port_str.trim(),
                                        self.server_code_str.trim()
                                    );
                                    let maybe_pub_ip = self.public_ip.read().ok().and_then(|g| g.clone());
                                    let maybe_invite_pub = maybe_pub_ip.as_ref().map(|pub_ip| {
                                        format!("{}:{}#{}", pub_ip, self.server_port_str.trim(), self.server_code_str.trim())
                                    });

                                    egui::Frame::new()
                                        .fill(th.bg_card_secondary)
                                        .stroke(Stroke::new(1.0, th.border_faint))
                                        .corner_radius(CornerRadius::same(8))
                                        .inner_margin(10.0)
                                        .show(ui, |ui| {
                                            ui.label(
                                                egui::RichText::new(self.t("address_for_peer"))
                                                    .size(10.5)
                                                    .color(th.text_sub),
                                            );
                                            ui.label(
                                                egui::RichText::new(format!("🏠 LAN: {}", invite_link_v4))
                                                    .strong()
                                                    .size(12.0)
                                                    .color(Color32::from_rgb(88, 101, 242)),
                                            );
                                            if let Some(ref pub_link) = maybe_invite_pub {
                                                ui.label(
                                                    egui::RichText::new(format!("🌐 WAN: {}", pub_link))
                                                        .strong()
                                                        .size(12.0)
                                                        .color(Color32::from_rgb(35, 165, 90)),
                                                );
                                            } else if self.is_fetching_ip.load(Ordering::Relaxed) {
                                                ui.label(
                                                    egui::RichText::new(self.t("fetching_ip"))
                                                        .size(11.0)
                                                        .color(th.text_sub),
                                                );
                                            }
                                        });

                                    ui.add_space(10.0);

                                    // Кнопка 1: Скопировать локальный IP
                                    if ui
                                        .add_sized(
                                            [col_w, 34.0],
                                            egui::Button::new(
                                                egui::RichText::new(self.t("copy_local_ip_btn"))
                                                    .strong()
                                                    .size(12.0)
                                                    .color(Color32::WHITE),
                                            )
                                            .fill(Color32::from_rgb(88, 101, 242))
                                            .corner_radius(CornerRadius::same(8)),
                                        )
                                        .clicked()
                                    {
                                        ui.ctx().copy_text(invite_link_v4.clone());
                                        crate::network::set_clipboard_text(&invite_link_v4);
                                        self.copy_toast = Some((
                                            format!("{}: {}", self.t("copied_local_ip_toast"), invite_link_v4),
                                            Instant::now(),
                                        ));
                                    }

                                    ui.add_space(6.0);

                                    // Кнопка 2: Скопировать глобальный IP
                                    let is_fetching = self.is_fetching_ip.load(Ordering::Relaxed);
                                    let global_btn_label = if let Some(ref pub_ip) = maybe_pub_ip {
                                        format!("{} ({})", self.t("copy_global_ip_btn"), pub_ip)
                                    } else if is_fetching {
                                        self.t("fetching_ip").to_string()
                                    } else {
                                        self.t("copy_global_ip_btn").to_string()
                                    };

                                    let global_btn = egui::Button::new(
                                        egui::RichText::new(&global_btn_label)
                                            .strong()
                                             .size(12.0)
                                            .color(Color32::WHITE),
                                    )
                                    .fill(Color32::from_rgb(35, 165, 90))
                                    .corner_radius(CornerRadius::same(8));

                                    if ui.add_sized([col_w, 34.0], global_btn).clicked() {
                                        if let Some(ref pub_link) = maybe_invite_pub {
                                            ui.ctx().copy_text(pub_link.clone());
                                            crate::network::set_clipboard_text(pub_link);
                                            self.copy_toast = Some((
                                                format!("{}: {}", self.t("copied_global_ip_toast"), pub_link),
                                                Instant::now(),
                                            ));
                                        } else if !is_fetching {
                                            let ip_clone = self.public_ip.clone();
                                            let is_fetching_clone = self.is_fetching_ip.clone();
                                            is_fetching_clone.store(true, Ordering::Relaxed);
                                            std::thread::spawn(move || {
                                                if let Some(ip) = crate::network::fetch_real_public_ip() {
                                                    if let Ok(mut lock) = ip_clone.write() {
                                                        *lock = Some(ip);
                                                    }
                                                }
                                                is_fetching_clone.store(false, Ordering::Relaxed);
                                            });
                                        }
                                    }

                                    ui.add_space(10.0);

                                    // Кнопка: Запустить / Остановить сервер
                                    let is_srv_running = self
                                        .p2p_server
                                        .as_ref()
                                        .map(|s| s.is_running.load(Ordering::Relaxed))
                                        .unwrap_or(false);
                                    let is_srv_connected = self
                                        .p2p_server
                                        .as_ref()
                                        .map(|s| s.is_connected.load(Ordering::Relaxed))
                                        .unwrap_or(false);

                                    if !is_srv_running {
                                        if ui
                                            .add_sized(
                                                [col_w, 38.0],
                                                egui::Button::new(
                                                    egui::RichText::new(self.t("start_server_btn"))
                                                        .strong()
                                                        .size(12.5)
                                                        .color(Color32::WHITE),
                                                )
                                                .fill(Color32::from_rgb(35, 165, 90))
                                                .corner_radius(CornerRadius::same(8)),
                                            )
                                            .clicked()
                                        {
                                            let port = self
                                                .server_port_str
                                                .trim()
                                                .parse::<u16>()
                                                .unwrap_or(48120);
                                            let code = self.server_code_str.trim().to_string();
                                            self.p2p_server =
                                                Some(crate::network::P2PNetworkServer::start_with_config(port, code, self.encoder_config.clone()));
                                        }
                                    } else {
                                        if ui
                                            .add_sized(
                                                [col_w, 38.0],
                                                egui::Button::new(
                                                    egui::RichText::new(self.t("stop_server_btn"))
                                                        .strong()
                                                        .size(12.5)
                                                        .color(Color32::WHITE),
                                                )
                                                .fill(Color32::from_rgb(218, 55, 60))
                                                .corner_radius(CornerRadius::same(8)),
                                            )
                                            .clicked()
                                        {
                                            if let Some(srv) = self.p2p_server.take() {
                                                srv.stop();
                                            }
                                        }
                                    }

                                    ui.add_space(8.0);

                                    // Статус сервера
                                    if let Some(ref srv) = self.p2p_server {
                                        let st = srv.status.read().unwrap().clone();
                                        let peer = srv.peer_addr.read().unwrap().clone();

                                        if is_srv_connected {
                                            ui.label(
                                                egui::RichText::new(format!("{}: {}", self.t("server_online_peer"), peer))
                                                    .color(Color32::from_rgb(35, 165, 90))
                                                    .strong()
                                                    .size(11.5),
                                            );
                                            ui.add_space(6.0);
                                            if ui
                                                .add_sized(
                                                    [col_w, 30.0],
                                                    egui::Button::new(
                                                        egui::RichText::new(self.t("go_to_call_btn"))
                                                            .strong()
                                                            .size(11.5)
                                                            .color(Color32::WHITE),
                                                    )
                                                    .fill(Color32::from_rgb(88, 101, 242))
                                                    .corner_radius(CornerRadius::same(6)),
                                                )
                                                .clicked()
                                            {
                                                self.active_tab = Tab::CallAndStream;
                                            }
                                        } else {
                                            ui.label(
                                                egui::RichText::new(format!("{}: {}", self.t("waiting_badge"), st))
                                                    .color(Color32::from_rgb(234, 179, 8))
                                                    .size(11.0),
                                            );
                                        }
                                    } else {
                                        ui.label(egui::RichText::new(self.t("server_not_running")).weak().size(11.0).color(th.text_sub));
                                    }
                                });

                            // ==========================================
                            // КАРТОЧКА 2: ПОДКЛЮЧИТЬСЯ К КОМНАТЕ (КЛИЕНТ)
                            // ==========================================
                            egui::Frame::new()
                                .fill(th.bg_card)
                                .stroke(Stroke::new(1.0, th.border))
                                .corner_radius(CornerRadius::same(12))
                                .inner_margin(16.0)
                                .show(&mut cols[1], |ui| {
                                    let col_w = ui.available_width();

                                    ui.horizontal(|ui| {
                                        ui.label(egui::RichText::new(self.t("client_badge")).strong().size(12.0).color(Color32::from_rgb(35, 165, 90)));
                                        ui.label(
                                            egui::RichText::new(self.t("client_card_title"))
                                                .size(15.0)
                                                .strong()
                                                .color(th.text_main),
                                        );
                                    });
                                    ui.add_space(2.0);
                                    ui.label(
                                        egui::RichText::new(self.t("client_card_desc"))
                                            .size(11.0)
                                            .color(th.text_sub),
                                    );
                                    ui.add_space(12.0);

                                    // Поле ввода: Адрес сервера + код
                                    ui.label(
                                        egui::RichText::new(self.t("client_target_label"))
                                            .size(12.0)
                                            .color(th.text_main),
                                    );
                                    ui.add_space(4.0);
                                    let client_hint = self.t("client_target_hint");
                                    ui.add_sized(
                                        [col_w, 24.0],
                                        egui::TextEdit::singleline(&mut self.client_target_str)
                                            .hint_text(client_hint),
                                    );

                                    ui.add_space(12.0);

                                    // Кнопка: Подключиться из буфера обмена
                                    if ui
                                        .add_sized(
                                            [col_w, 36.0],
                                            egui::Button::new(
                                                egui::RichText::new(self.t("paste_and_connect_btn"))
                                                    .strong()
                                                    .size(11.5)
                                                    .color(Color32::WHITE),
                                            )
                                            .fill(Color32::from_rgb(88, 101, 242))
                                            .corner_radius(CornerRadius::same(8)),
                                        )
                                        .clicked()
                                    {
                                        if let Some(clip) = crate::network::get_clipboard_text() {
                                            if let Some((host, port, code)) =
                                                crate::network::parse_room_target(&clip)
                                            {
                                                self.client_target_str =
                                                    format!("{}:{}#{}", host, port, code);
                                                self.copy_toast = Some((
                                                    format!("{}: {}:{}#{}", self.t("pasted_toast"), host, port, code),
                                                    Instant::now(),
                                                ));
                                                self.connect_client(host, port, code);
                                            } else {
                                                self.copy_toast = Some((
                                                    self.t("clipboard_parse_error").to_string(),
                                                    Instant::now(),
                                                ));
                                            }
                                        } else {
                                            self.copy_toast =
                                                Some((self.t("clipboard_empty").to_string(), Instant::now()));
                                        }
                                    }

                                    ui.add_space(10.0);

                                    // Кнопка: Подключиться
                                    let is_cli_connected = self
                                        .p2p_client
                                        .as_ref()
                                        .map(|c| c.is_connected.load(Ordering::Relaxed))
                                        .unwrap_or(false);
                                    let is_cli_running = self
                                        .p2p_client
                                        .as_ref()
                                        .map(|c| c.is_running.load(Ordering::Relaxed))
                                        .unwrap_or(false);

                                    if !is_cli_connected && !is_cli_running {
                                        if ui
                                            .add_sized(
                                                [col_w, 38.0],
                                                egui::Button::new(
                                                    egui::RichText::new(self.t("connect_btn"))
                                                        .strong()
                                                        .size(12.5)
                                                        .color(Color32::WHITE),
                                                )
                                                .fill(Color32::from_rgb(35, 165, 90))
                                                .corner_radius(CornerRadius::same(8)),
                                            )
                                            .clicked()
                                        {
                                            if let Some((host, port, code)) =
                                                crate::network::parse_room_target(&self.client_target_str)
                                            {
                                                self.connect_client(host, port, code);
                                            } else {
                                                self.client_status_msg =
                                                    self.t("invalid_address_error").to_string();
                                            }
                                        }
                                    } else {
                                        if ui
                                            .add_sized(
                                                [col_w, 38.0],
                                                egui::Button::new(
                                                    egui::RichText::new(self.t("disconnect_btn"))
                                                        .strong()
                                                        .size(12.5)
                                                        .color(Color32::WHITE),
                                                )
                                                .fill(Color32::from_rgb(218, 55, 60))
                                                .corner_radius(CornerRadius::same(8)),
                                            )
                                            .clicked()
                                        {
                                            if let Some(cli) = self.p2p_client.take() {
                                                cli.stop();
                                            }
                                            self.client_status_msg = self.t("disconnected_status").to_string();
                                        }
                                    }

                                    ui.add_space(8.0);

                                    // Статус клиента
                                    if let Some(ref cli) = self.p2p_client {
                                        let st = cli.status.read().unwrap().clone();
                                        if is_cli_connected {
                                            ui.label(
                                                egui::RichText::new(format!("{} {}", self.t("client_online_badge"), st))
                                                    .color(Color32::from_rgb(35, 165, 90))
                                                    .strong()
                                                    .size(11.5),
                                            );
                                            ui.add_space(6.0);
                                            if ui
                                                .add_sized(
                                                    [col_w, 30.0],
                                                    egui::Button::new(
                                                        egui::RichText::new(self.t("go_to_call_btn"))
                                                            .strong()
                                                            .size(11.5)
                                                            .color(Color32::WHITE),
                                                    )
                                                    .fill(Color32::from_rgb(88, 101, 242))
                                                    .corner_radius(CornerRadius::same(6)),
                                                )
                                                .clicked()
                                            {
                                                self.active_tab = Tab::CallAndStream;
                                            }
                                        } else {
                                            ui.label(
                                                egui::RichText::new(format!("{} {}", self.t("client_connecting_badge"), st))
                                                    .color(Color32::from_rgb(234, 179, 8))
                                                    .size(11.0),
                                            );
                                        }
                                    } else if !self.client_status_msg.is_empty() {
                                        let col = if self.client_status_msg.starts_with("Ошибка") || self.client_status_msg.starts_with("Error") {
                                            Color32::from_rgb(218, 55, 60)
                                        } else {
                                            th.text_sub
                                        };
                                        let display_msg = if self.client_status_msg == "Не подключен" {
                                            self.t("client_not_connected")
                                        } else {
                                            &self.client_status_msg
                                        };
                                        ui.label(
                                            egui::RichText::new(display_msg)
                                                .color(col)
                                                .size(11.0),
                                        );
                                    }
                                });
                        });

                        ui.add_space(20.0);
                    });
                });
            });
    }

    fn connect_client(&mut self, host: String, port: u16, code: String) {
        if let Some(old) = self.p2p_client.take() {
            old.stop();
        }
        self.p2p_client = Some(crate::network::P2PNetworkClient::connect_with_config(host, port, code, self.encoder_config.clone()));
    }
    /// Отрисовка высококонтрастной бирки навигации с динамической адаптацией к выбранной теме
    fn render_nav_tab_pill(ui: &mut egui::Ui, current: &mut Tab, tab: Tab, label: &str, th: &ThemeColors) {
        let is_selected = *current == tab;
        let galley = ui.painter().layout_no_wrap(label.to_string(), egui::FontId::proportional(12.0), th.text_main);
        let pill_w = galley.size().x + 14.0; // аккуратная компактная ширина строго по размеру текста
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(pill_w, 25.0), egui::Sense::click());
        if resp.clicked() {
            *current = tab;
        }
        let is_hovered = resp.hovered();

        let (bg_color, text_color, stroke) = if is_selected {
            // Выбранная бирка
            (th.pill_active_bg, th.pill_active_fg, Stroke::new(1.0, th.pill_active_bg))
        } else if is_hovered {
            // При наведении
            let hov_bg = th.pill_active_bg.linear_multiply(0.35);
            (hov_bg, th.pill_active_fg, Stroke::new(1.0, th.border))
        } else {
            // Неактивная бирка
            (th.pill_inactive_bg, th.pill_inactive_fg, Stroke::new(1.0, th.border))
        };

        ui.painter().rect_filled(rect, CornerRadius::same(6), bg_color);
        ui.painter().rect_stroke(rect, CornerRadius::same(6), stroke, egui::StrokeKind::Inside);
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(12.0),
            text_color,
        );
    }

    pub fn get_effective_preset(&self) -> (u8, &'static str) {
        match self.preset_mode {
            PresetMode::ManualP1 => (1, "P1"),
            PresetMode::ManualP3 => (3, "P3"),
            PresetMode::ManualP4 => (4, "P4"),
            PresetMode::ManualP5 => (5, "P5"),
            PresetMode::ManualP6 => (6, "P6"),
            PresetMode::ManualP7 => (7, "P7"),
            PresetMode::AutoSmart => {
                let (w, _, _) = self.available_resolutions[self.selected_res_idx];
                if self.target_fps <= 24 {
                    if w <= 854 { (7, "P7") } else if w <= 1280 { (6, "P6") } else { (4, "P4") }
                } else if self.target_fps <= 60 {
                    if w <= 854 { (6, "P6") } else if w <= 1280 { (4, "P4") } else { (3, "P3") }
                } else {
                    (1, "P1")
                }
            }
        }
    }

    pub fn estimate_latency_ms(&self) -> f32 {
        // Measured RTT beats a table; the matrix below is a pre-connect estimate.
        let measured = self
            .p2p_server
            .as_ref()
            .and_then(|s| s.telemetry.read().ok().map(|t| t.latency_ms))
            .or_else(|| {
                self.p2p_client
                    .as_ref()
                    .and_then(|c| c.telemetry.read().ok().map(|t| t.latency_ms))
            })
            .unwrap_or(0.0);
        if measured > 0.0 {
            return measured;
        }

        let (p_id, _) = self.get_effective_preset();
        let (w, _, _) = self.available_resolutions[self.selected_res_idx];
        if let Some(results) = &self.benchmark_results {
            if let Some(r) = results.iter().find(|r| r.width == w && r.preset_id == p_id) {
                return r.avg_latency_ms;
            }
        }
        match (w, p_id) {
            (854, 1) => 2.97,
            (854, 3) => 3.60,
            (854, 4) => 3.61,
            (854, 5) => 3.82,
            (854, 6) => 4.12,
            (854, 7) => 4.58,
            (1280, 1) => 5.08,
            (1280, 3) => 6.62,
            (1280, 4) => 6.96,
            (1280, 5) => 7.42,
            (1280, 6) => 8.15,
            (1280, 7) => 9.35,
            (1920, 1) => 10.90,
            (1920, 3) => 13.85,
            (1920, 4) => 14.29,
            (1920, 5) => 15.30,
            (1920, 6) => 17.10,
            (1920, 7) => 19.80,
            _ => 14.0,
        }
    }
}

impl eframe::App for NanocordApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Гарантированная синхронизация темы оформления
        if self.applied_theme != Some(self.app_theme) {
            apply_theme_mode(ctx, self.app_theme);
            self.applied_theme = Some(self.app_theme);
        }

        // Ползунок fps управляет захватом напрямую, без перезапуска.
        self.target_fps_shared
            .store(self.target_fps.clamp(1, 120), Ordering::Relaxed);

        // Проверяем статус P2P подключения
        let is_srv_connected = self.p2p_server.as_ref().map(|s| s.is_connected.load(Ordering::Relaxed)).unwrap_or(false);
        let is_cli_connected = self.p2p_client.as_ref().map(|c| c.is_connected.load(Ordering::Relaxed)).unwrap_or(false);
        let is_peer_connected = is_srv_connected || is_cli_connected;

        // Ленивая инициализация текстуры героя Майкла Джексона
        if self.hero_mj_texture.is_none() {
            if let Ok(img) = image::load_from_memory(HERO_MJ_BYTES) {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                let color_image = egui::ColorImage::from_rgba_unmultiplied(
                    [w as usize, h as usize],
                    rgba.as_raw(),
                );
                self.hero_mj_texture = Some(ctx.load_texture("hero_mj", color_image, egui::TextureOptions::LINEAR));
            }
        }

        // 0. Динамическое переключение микрофона при выборе в выпадающем списке
        if let Some(dev) = self.input_devices.get(self.selected_input_device_idx) {
            if dev.id != self.applied_input_device_id {
                self.mic_is_running.store(false, Ordering::Relaxed);
                let new_run = Arc::new(AtomicBool::new(true));
                self.mic_is_running = new_run.clone();
                let target_dev = if dev.id == "default" { None } else { Some(dev.id.clone()) };
                self.mic_audio_rx = Some(crate::audio::start_wasapi_audio_worker(false, target_dev, new_run));
                self.applied_input_device_id = dev.id.clone();
            }
        }

        // Динамическое переключение устройства воспроизведения при выборе в выпадающем списке
        if let Some(dev) = self.output_devices.get(self.selected_output_device_idx) {
            if dev.id != self.applied_output_device_id {
                self.audio_player.is_running.store(false, Ordering::Relaxed);
                let target_dev = if dev.id == "default" { None } else { Some(dev.id.clone()) };
                self.audio_player = Arc::new(crate::audio::WasapiAudioPlayer::with_device(target_dev));
                self.applied_output_device_id = dev.id.clone();
            }
        }

        // Горячие клавиши (M - микрофон, D - звук, если пользователь не вводит текст в поле ввода)
        if !ctx.wants_keyboard_input() {
            if ctx.input(|i| i.key_pressed(egui::Key::M)) {
                self.is_mic_muted = !self.is_mic_muted;
            }
            if ctx.input(|i| i.key_pressed(egui::Key::D)) {
                self.is_deafened = !self.is_deafened;
            }
        }

        // 1. Приём настоящих кадров захвата экрана
        if let Some(ref slot) = self.preview_rx {
            while let Some(frame) = slot.take() {
                // Если собеседник подключен и стрим включен, отправляем захваченный кадр экрана в P2P
                if is_peer_connected && self.is_streaming.load(Ordering::Relaxed) {
                    let net_frame = crate::network::OutgoingFrame {
                        width: frame.width,
                        height: frame.height,
                        rgba: std::sync::Arc::new(frame.rgba.clone()),
                    };
                    if let Some(ref srv) = self.p2p_server {
                        srv.outbox_desktop.put(net_frame.clone());
                    }
                    if let Some(ref cli) = self.p2p_client {
                        cli.outbox_desktop.put(net_frame);
                    }
                }

                let color_image = egui::ColorImage::from_rgba_unmultiplied(
                    [frame.width as usize, frame.height as usize],
                    &frame.rgba,
                );
                match &mut self.desktop_texture {
                    Some(tex) => tex.set(color_image, egui::TextureOptions::LINEAR),
                    None => {
                        self.desktop_texture = Some(ctx.load_texture(
                            "desktop_live_preview",
                            color_image,
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                }
            }
        }

        // 2. Приём настоящих кадров с веб-камеры (nokhwa)
        if let Some(ref rx) = self.camera_rx {
            while let Ok(frame) = rx.try_recv() {
                if !self.is_camera_on {
                    continue;
                }
                // Если собеседник подключен и камера включена, отправляем кадр веб-камеры
                if is_peer_connected {
                    let net_frame = crate::network::OutgoingFrame {
                        width: frame.width,
                        height: frame.height,
                        rgba: std::sync::Arc::new(frame.rgba.clone()),
                    };
                    if let Some(ref srv) = self.p2p_server {
                        srv.outbox_camera.put(net_frame.clone());
                    }
                    if let Some(ref cli) = self.p2p_client {
                        cli.outbox_camera.put(net_frame);
                    }
                }

                let color_image = egui::ColorImage::from_rgba_unmultiplied(
                    [frame.width as usize, frame.height as usize],
                    &frame.rgba,
                );
                match &mut self.camera_texture {
                    Some(tex) => tex.set(color_image, egui::TextureOptions::LINEAR),
                    None => {
                        self.camera_texture = Some(ctx.load_texture(
                            "camera_live_preview",
                            color_image,
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                }
            }
        }

        // 3. Обновление двухканальной звуковой волны (10 сек буфер) из WASAPI и отправка в P2P
        self.mic_waveform.is_muted = self.is_mic_muted;
        if let Some(ref rx) = self.mic_audio_rx {
            while let Ok(pkt) = rx.try_recv() {
                self.mic_waveform.push_real_sample(pkt.min_sample, pkt.max_sample, pkt.rms, pkt.peak_db);

                // Реальное аппаратное масштабирование громкости микрофона с soft-saturation лимитером
                let mic_vol = if self.is_mic_muted { 0.0 } else { self.mic_waveform.volume };
                let mut processed_pcm = pkt.pcm_chunk.clone();
                if (mic_vol - 1.0).abs() > 0.001 || self.is_mic_muted {
                    for s in &mut processed_pcm {
                        *s = crate::audio::apply_gain_soft_clip(*s, mic_vol);
                    }
                }

                // Самопрослушивание своего микрофона (Sidetone test)
                if self.listen_to_mic_sidetone && !self.is_mic_muted {
                    self.audio_player.play_samples(processed_pcm.clone());
                }

                // Автоматическая нормализация громкости: тихий микрофон
                // подтягивается к комфортному уровню, громкий — приглушается.
                if !self.is_mic_muted {
                    self.mic_agc.apply(&mut processed_pcm);
                }

                // Кодирование и отправка звука микрофона в P2P через Opus (VoIP VBR 64 kbps + FEC + DTX)
                if is_peer_connected && !self.is_mic_muted {
                    let opus_packets = self.mic_opus_encoder.encode_chunk(&processed_pcm);
                    for opus_data in opus_packets {
                        let seq = self.mic_opus_encoder.next_seq();
                        let net_audio = crate::network::NetworkAudioPacket::with_stream(
                            0, // Mic
                            seq,
                            (pkt.peak_db + 20.0 * mic_vol.max(0.01).log10()).clamp(-60.0, 0.0),
                            pkt.min_sample * mic_vol,
                            pkt.max_sample * mic_vol,
                            pkt.rms * mic_vol,
                            pkt.bins,
                            opus_data,
                        );
                        if let Some(ref srv) = self.p2p_server {
                            let _ = srv.outgoing_audio_tx.try_send(net_audio.clone());
                        }
                        if let Some(ref cli) = self.p2p_client {
                            let _ = cli.outgoing_audio_tx.try_send(net_audio);
                        }
                    }
                }
            }
        }

        self.game_waveform.is_muted = self.is_deafened;
        if let Some(ref rx) = self.game_audio_rx {
            while let Ok(pkt) = rx.try_recv() {
                self.game_waveform.push_real_sample(pkt.min_sample, pkt.max_sample, pkt.rms, pkt.peak_db);

                let loop_vol = if self.is_deafened { 0.0 } else { self.game_waveform.volume };
                let mut processed_pcm = pkt.pcm_chunk.clone();
                if (loop_vol - 1.0).abs() > 0.001 || self.is_deafened {
                    for s in &mut processed_pcm {
                        *s = crate::audio::apply_gain_soft_clip(*s, loop_vol);
                    }
                }
                if is_peer_connected && !self.is_deafened {
                    let opus_packets = self.game_opus_encoder.encode_chunk(&processed_pcm);
                    for opus_data in opus_packets {
                        let seq = self.game_opus_encoder.next_seq();
                        let net_audio = crate::network::NetworkAudioPacket::with_stream(
                            1, // Desktop loopback
                            seq,
                            (pkt.peak_db + 20.0 * loop_vol.max(0.01).log10()).clamp(-60.0, 0.0),
                            pkt.min_sample * loop_vol,
                            pkt.max_sample * loop_vol,
                            pkt.rms * loop_vol,
                            pkt.bins,
                            opus_data,
                        );
                        if let Some(ref srv) = self.p2p_server {
                            let _ = srv.outgoing_audio_tx.try_send(net_audio.clone());
                        }
                        if let Some(ref cli) = self.p2p_client {
                            let _ = cli.outgoing_audio_tx.try_send(net_audio);
                        }
                    }
                }
            }
        }

        // 4. Приём входящих кадров и аудио от P2P Сервера или Клиента (IPv6 + IPv4)
        let mut latest_remote_dt = None;
        let mut latest_remote_cam = None;

        if let Some(ref srv) = self.p2p_server {
            while let Ok(frame) = srv.desktop_rx.try_recv() {
                if frame.keepalive {
                    self.last_remote_desktop_time = Some(Instant::now());
                } else {
                    latest_remote_dt = Some(frame);
                }
            }
            while let Ok(frame) = srv.camera_rx.try_recv() {
                if frame.keepalive {
                    self.last_remote_camera_time = Some(Instant::now());
                } else {
                    latest_remote_cam = Some(frame);
                }
            }
            while let Ok(audio) = srv.audio_rx.try_recv() {
                if !self.is_deafened {
                    let mut samples = if audio.stream_type == 0 {
                        self.peer_mic_opus_decoder.decode_packet_with_fec(audio.seq, &audio.opus_data)
                    } else {
                        self.peer_game_opus_decoder.decode_packet_with_fec(audio.seq, &audio.opus_data)
                    };
                    if !samples.is_empty() {
                        let out_vol = self.game_waveform.volume;
                        if (out_vol - 1.0).abs() > 0.001 {
                            for s in &mut samples {
                                *s = crate::audio::apply_gain_soft_clip(*s, out_vol);
                            }
                        }
                        self.audio_player.play_samples(samples);
                    }
                }
            }
        }
        if let Some(ref cli) = self.p2p_client {
            while let Ok(frame) = cli.desktop_rx.try_recv() {
                if frame.keepalive {
                    self.last_remote_desktop_time = Some(Instant::now());
                } else {
                    latest_remote_dt = Some(frame);
                }
            }
            while let Ok(frame) = cli.camera_rx.try_recv() {
                if frame.keepalive {
                    self.last_remote_camera_time = Some(Instant::now());
                } else {
                    latest_remote_cam = Some(frame);
                }
            }
            while let Ok(audio) = cli.audio_rx.try_recv() {
                if !self.is_deafened {
                    let mut samples = if audio.stream_type == 0 {
                        self.peer_mic_opus_decoder.decode_packet_with_fec(audio.seq, &audio.opus_data)
                    } else {
                        self.peer_game_opus_decoder.decode_packet_with_fec(audio.seq, &audio.opus_data)
                    };
                    if !samples.is_empty() {
                        let out_vol = self.game_waveform.volume;
                        if (out_vol - 1.0).abs() > 0.001 {
                            for s in &mut samples {
                                *s = crate::audio::apply_gain_soft_clip(*s, out_vol);
                            }
                        }
                        self.audio_player.play_samples(samples);
                    }
                }
            }
        }

        if let Some(frame) = latest_remote_dt {
            self.last_remote_desktop_time = Some(Instant::now());
            let color_image = egui::ColorImage::from_rgba_unmultiplied(
                [frame.width as usize, frame.height as usize],
                &frame.rgba,
            );
            match &mut self.remote_desktop_texture {
                Some(tex) => tex.set(color_image, egui::TextureOptions::LINEAR),
                None => {
                    self.remote_desktop_texture = Some(ctx.load_texture(
                        "remote_desktop_live",
                        color_image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
            }
        }

        if let Some(frame) = latest_remote_cam {
            self.last_remote_camera_time = Some(Instant::now());
            let color_image = egui::ColorImage::from_rgba_unmultiplied(
                [frame.width as usize, frame.height as usize],
                &frame.rgba,
            );
            match &mut self.remote_camera_texture {
                Some(tex) => tex.set(color_image, egui::TextureOptions::LINEAR),
                None => {
                    self.remote_camera_texture = Some(ctx.load_texture(
                        "remote_camera_live",
                        color_image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
            }
        }

        // Защита от замерзания кадров: если стрим прекратился или собеседник отключился, очищаем текстуру
        let stream_timeout = Duration::from_millis(2200);
        if !is_peer_connected || self.last_remote_desktop_time.map_or(true, |t| t.elapsed() > stream_timeout) {
            self.remote_desktop_texture = None;
        }
        if !is_peer_connected || self.last_remote_camera_time.map_or(true, |t| t.elapsed() > stream_timeout) {
            self.remote_camera_texture = None;
        }

        // Авто-переход между "Комната" и "Звонок и Стрим" в зависимости от активности P2P комнаты
        let is_srv_conn = self.p2p_server.as_ref().map(|s| s.is_connected.load(Ordering::Relaxed)).unwrap_or(false);
        let is_cli_conn = self.p2p_client.as_ref().map(|c| c.is_connected.load(Ordering::Relaxed)).unwrap_or(false);
        let is_srv_running = self.p2p_server.as_ref().map(|s| s.is_running.load(Ordering::Relaxed)).unwrap_or(false);
        let is_in_room = is_srv_running || is_srv_conn || is_cli_conn;

        if is_in_room && self.active_tab == Tab::Lobby {
            self.active_tab = Tab::CallAndStream;
            self.active_stage = StageSource::RemoteDesktop;
        } else if !is_in_room && self.active_tab == Tab::CallAndStream {
            self.active_tab = Tab::Lobby;
        }

        // 5. Приём результатов бенчмарка
        if let Some(rx) = &self.bench_rx {
            if let Ok(results) = rx.try_recv() {
                let matrix = build_benchmark_matrix(&results, self.native_width, self.native_height, self.native_hz);
                self.benchmark_matrix = Some(matrix);
                self.benchmark_results = Some(results);
                self.is_benchmarking = false;
                self.bench_rx = None;
            }
        }

        // 6. Клавиши управления (Esc, F11)
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && self.is_fullscreen {
            self.is_fullscreen = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F11)) {
            self.is_fullscreen = !self.is_fullscreen;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.is_fullscreen));
        }

        let is_active = is_peer_connected || self.is_streaming.load(Ordering::Relaxed) || self.is_camera_on;
        if is_active {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(60));
        }

        // ЕСЛИ ВКЛЮЧЕН ПОЛНОЭКРАННЫЙ РЕЖИМ (FULLSCREEN)
        if self.is_fullscreen {
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(Color32::from_rgb(10, 12, 16)).inner_margin(0.0))
                .show(ctx, |ui| {
                    self.render_fullscreen_stage(ui);
                });
            return;
        }

        let th = self.app_theme.colors();

        // ОБЫЧНЫЙ РЕЖИМ: ВЕРХНЯЯ НАВИГАЦИОННАЯ ПАНЕЛЬ
        egui::TopBottomPanel::top("top_nav")
            .frame(egui::Frame::new().fill(th.bg_top_nav).stroke(Stroke::new(1.0, th.border)).inner_margin(egui::Margin::symmetric(14, 8)))
            .show(ctx, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // 1. Тройной тумблер тем оформления (Солнышко, Ночь, Сова)
                    render_theme_triple_toggle(ui, &mut self.app_theme, &mut self.applied_theme, &th);

                    ui.add_space(8.0);

                    // 2. Тумблер выбора языка (Русский и Американский флаги)
                    render_language_toggle(ui, &mut self.app_language, &th);

                    ui.add_space(8.0);

                    let is_srv_conn = self.p2p_server.as_ref().map(|s| s.is_connected.load(Ordering::Relaxed)).unwrap_or(false);
                    let is_cli_conn = self.p2p_client.as_ref().map(|c| c.is_connected.load(Ordering::Relaxed)).unwrap_or(false);
                    let is_srv_running = self.p2p_server.as_ref().map(|s| s.is_running.load(Ordering::Relaxed)).unwrap_or(false);

                    if is_srv_conn || is_cli_conn {
                        ui.label(egui::RichText::new(self.t("top_online_p2p")).color(Color32::from_rgb(35, 165, 90)).strong().size(12.0));
                    } else if is_srv_running {
                        ui.label(egui::RichText::new(self.t("top_server_listening")).color(Color32::from_rgb(234, 179, 8)).strong().size(12.0));
                    } else if self.is_streaming.load(Ordering::Relaxed) {
                        ui.label(egui::RichText::new(self.t("top_live_60fps")).color(Color32::from_rgb(220, 38, 38)).strong().size(12.0));
                    } else if self.is_benchmarking {
                        ui.label(egui::RichText::new(self.t("top_benchmarking")).color(Color32::from_rgb(217, 119, 6)).strong().size(12.0));
                    }

                    ui.add_space(10.0);

                    // Слева направо: логотип и вкладки
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new("NANOCORD").strong().size(17.0).color(Color32::from_rgb(88, 101, 242)));
                        ui.label(egui::RichText::new("v0.1.0 • P2P Ultra-Low Latency").weak().size(11.5).color(th.text_sub));

                        ui.add_space(14.0);

                        let tab_lobby_title = if is_in_room {
                            self.t("nav_call_and_stream")
                        } else {
                            self.t("nav_lobby")
                        };
                        let tab_bench_title = self.t("nav_benchmark_matrix");
                        let tab_audio_title = self.t("nav_audio_spectrum");
                        let tab_nvenc_title = self.t("nav_encoder_settings");

                        if is_in_room {
                            Self::render_nav_tab_pill(ui, &mut self.active_tab, Tab::CallAndStream, tab_lobby_title, &th);
                        } else {
                            Self::render_nav_tab_pill(ui, &mut self.active_tab, Tab::Lobby, tab_lobby_title, &th);
                        }
                        Self::render_nav_tab_pill(ui, &mut self.active_tab, Tab::BenchmarkMatrix, tab_bench_title, &th);
                        Self::render_nav_tab_pill(ui, &mut self.active_tab, Tab::AudioAndSpectrum, tab_audio_title, &th);
                        Self::render_nav_tab_pill(ui, &mut self.active_tab, Tab::EncoderSettings, tab_nvenc_title, &th);
                    });
                });
            });

        // ЦЕНТРАЛЬНАЯ РАБОЧАЯ ОБЛАСТЬ
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(th.bg_app).inner_margin(8.0))
            .show(ctx, |ui| {
                match self.active_tab {
                    Tab::Lobby => self.render_lobby(ui),
                    Tab::CallAndStream => self.render_call_and_stream(ui),
                    Tab::BenchmarkMatrix => self.render_benchmark_matrix(ui),
                    Tab::AudioAndSpectrum => self.render_audio_and_spectrum(ui),
                    Tab::EncoderSettings => self.render_encoder_settings(ui),
                }
            });
    }
}

impl NanocordApp {
    /// Отрисовка нижней плавающей панели управления звонком (Dock с иконками Material Icons и кнопкой Завершить)
    fn render_call_dock(&mut self, ui: &mut egui::Ui, center_pos: Pos2) {
        let th = self.app_theme.colors();
        let dock_pill_w = 368.0_f32;
        let dock_pill_rect = Rect::from_center_size(center_pos, Vec2::new(dock_pill_w, 52.0));

        let painter = ui.painter();
        let (dock_bg, dock_border) = if self.is_fullscreen {
            (Color32::from_rgba_unmultiplied(18, 22, 30, 230), Color32::from_rgba_unmultiplied(255, 255, 255, 28))
        } else {
            (th.bg_dock, th.border)
        };
        painter.rect_filled(dock_pill_rect, CornerRadius::same(26), dock_bg);
        painter.rect_stroke(dock_pill_rect, CornerRadius::same(26), Stroke::new(1.0, dock_border), egui::StrokeKind::Inside);

        let (dock_btn_bg, dock_btn_border, dock_icon_color) = if self.is_fullscreen {
            (Color32::from_rgb(32, 36, 46), Color32::from_rgb(52, 58, 72), Color32::WHITE)
        } else if self.app_theme == AppTheme::LightClean {
            (Color32::from_rgb(228, 233, 243), Color32::from_rgb(205, 212, 226), Color32::from_rgb(32, 38, 52))
        } else if self.app_theme == AppTheme::OledMidnight {
            (Color32::from_rgb(26, 26, 32), Color32::from_rgb(50, 54, 64), Color32::WHITE)
        } else {
            (Color32::from_rgb(45, 48, 56), Color32::from_rgb(60, 64, 76), Color32::WHITE)
        };

        let is_stream = self.is_streaming.load(Ordering::Relaxed);
        let mut dock_ui = ui.new_child(egui::UiBuilder::new().max_rect(dock_pill_rect.shrink2(Vec2::new(12.0, 4.0))));
        dock_ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 0.0);

            // 1. Иконка Микрофона с круглым звуковым ореолом (без артефактов и обрезки)
            let (mic_rect, mic_resp) = ui.allocate_exact_size(Vec2::new(42.0, 42.0), egui::Sense::click());
            let mic_center = mic_rect.center();
            let mic_radius = 21.0;
            let mic_painter = ui.painter();

            if self.is_mic_muted {
                mic_painter.circle_filled(mic_center, mic_radius, Color32::from_rgb(218, 55, 60));
                mic_painter.circle_stroke(mic_center, mic_radius, Stroke::new(1.0, Color32::from_rgb(240, 80, 85)));
                mic_painter.text(mic_center, egui::Align2::CENTER_CENTER, ICON_MIC_OFF, egui::FontId::proportional(20.0), Color32::WHITE);
            } else {
                mic_painter.circle_filled(mic_center, mic_radius, dock_btn_bg);
                mic_painter.circle_stroke(mic_center, mic_radius, Stroke::new(1.0, dock_btn_border));

                let vol_norm = (self.mic_waveform.smoothed_rms * 3.5).clamp(0.0, 1.0);
                if vol_norm > 0.03 {
                    let halo_r = mic_radius + 2.0 * vol_norm;
                    let alpha = (110.0 + 145.0 * vol_norm) as u8;
                    mic_painter.circle_stroke(
                        mic_center,
                        halo_r,
                        Stroke::new(1.5 + 1.5 * vol_norm, Color32::from_rgba_unmultiplied(35, 165, 90, alpha)),
                    );
                }

                if mic_resp.hovered() {
                    mic_painter.circle_stroke(mic_center, mic_radius, Stroke::new(1.5, Color32::from_rgb(88, 101, 242)));
                }

                let mic_col = if vol_norm > 0.05 { Color32::from_rgb(35, 165, 90) } else { dock_icon_color };
                mic_painter.text(mic_center, egui::Align2::CENTER_CENTER, ICON_MIC, egui::FontId::proportional(20.0), mic_col);
            }

            if mic_resp.clicked() {
                self.is_mic_muted = !self.is_mic_muted;
            }
            mic_resp.on_hover_text(if self.is_mic_muted { self.t("dock_unmute_mic") } else { self.t("dock_mute_mic") });

            // 2. Иконка Звука системы (Deafen)
            let (deaf_rect, deaf_resp) = ui.allocate_exact_size(Vec2::new(42.0, 42.0), egui::Sense::click());
            let deaf_center = deaf_rect.center();
            let deaf_radius = 21.0;
            let deaf_painter = ui.painter();

            if self.is_deafened {
                deaf_painter.circle_filled(deaf_center, deaf_radius, Color32::from_rgb(218, 55, 60));
                deaf_painter.circle_stroke(deaf_center, deaf_radius, Stroke::new(1.0, Color32::from_rgb(240, 80, 85)));
                deaf_painter.text(deaf_center, egui::Align2::CENTER_CENTER, ICON_HEADSET_OFF, egui::FontId::proportional(20.0), Color32::WHITE);
            } else {
                deaf_painter.circle_filled(deaf_center, deaf_radius, dock_btn_bg);
                deaf_painter.circle_stroke(deaf_center, deaf_radius, Stroke::new(1.0, if deaf_resp.hovered() { Color32::from_rgb(88, 101, 242) } else { dock_btn_border }));
                deaf_painter.text(deaf_center, egui::Align2::CENTER_CENTER, ICON_HEADSET, egui::FontId::proportional(20.0), dock_icon_color);
            }

            if deaf_resp.clicked() {
                self.is_deafened = !self.is_deafened;
            }
            deaf_resp.on_hover_text(if self.is_deafened { self.t("dock_unmute_audio") } else { self.t("dock_deafen_audio") });

            // 3. Иконка Веб-камеры
            let (cam_rect, cam_resp) = ui.allocate_exact_size(Vec2::new(42.0, 42.0), egui::Sense::click());
            let cam_center = cam_rect.center();
            let cam_radius = 21.0;
            let cam_painter = ui.painter();

            if !self.is_camera_on {
                cam_painter.circle_filled(cam_center, cam_radius, Color32::from_rgb(218, 55, 60));
                cam_painter.circle_stroke(cam_center, cam_radius, Stroke::new(1.0, Color32::from_rgb(240, 80, 85)));
                cam_painter.text(cam_center, egui::Align2::CENTER_CENTER, ICON_VIDEOCAM_OFF, egui::FontId::proportional(20.0), Color32::WHITE);
            } else {
                cam_painter.circle_filled(cam_center, cam_radius, dock_btn_bg);
                cam_painter.circle_stroke(cam_center, cam_radius, Stroke::new(1.0, if cam_resp.hovered() { Color32::from_rgb(88, 101, 242) } else { dock_btn_border }));
                cam_painter.text(cam_center, egui::Align2::CENTER_CENTER, ICON_VIDEOCAM, egui::FontId::proportional(20.0), dock_icon_color);
            }

            if cam_resp.clicked() {
                self.is_camera_on = !self.is_camera_on;
                self.is_camera_on_flag.store(self.is_camera_on, Ordering::Relaxed);
            }
            cam_resp.on_hover_text(if self.is_camera_on { self.t("dock_turn_off_cam") } else { self.t("dock_turn_on_cam") });

            // 4. Иконка Стрима экрана (по умолчанию серая, подсвечивается только при активности)
            let (scr_rect, scr_resp) = ui.allocate_exact_size(Vec2::new(42.0, 42.0), egui::Sense::click());
            let scr_center = scr_rect.center();
            let scr_radius = 21.0;
            let scr_painter = ui.painter();

            if is_stream {
                scr_painter.circle_filled(scr_center, scr_radius, Color32::from_rgb(88, 101, 242));
                scr_painter.circle_stroke(scr_center, scr_radius, Stroke::new(1.0, Color32::from_rgb(114, 137, 218)));
                scr_painter.text(scr_center, egui::Align2::CENTER_CENTER, ICON_SCREEN_SHARE, egui::FontId::proportional(20.0), Color32::WHITE);
            } else {
                scr_painter.circle_filled(scr_center, scr_radius, dock_btn_bg);
                scr_painter.circle_stroke(scr_center, scr_radius, Stroke::new(1.0, if scr_resp.hovered() { Color32::from_rgb(88, 101, 242) } else { dock_btn_border }));
                scr_painter.text(scr_center, egui::Align2::CENTER_CENTER, ICON_SCREEN_SHARE, egui::FontId::proportional(20.0), dock_icon_color);
            }

            if scr_resp.clicked() {
                self.is_streaming.store(!is_stream, Ordering::Relaxed);
            }
            scr_resp.on_hover_text(if is_stream { self.t("dock_stop_screen") } else { self.t("dock_start_screen") });

            // 5. Кнопка "Завершить звонок" (красный pill с иконкой трубки и текстом)
            let hang_text = self.t("dock_end_call_btn");
            let (hang_rect, hang_resp) = ui.allocate_exact_size(Vec2::new(144.0, 42.0), egui::Sense::click());
            let hang_painter = ui.painter();

            let hang_bg = if hang_resp.hovered() {
                Color32::from_rgb(240, 70, 75)
            } else {
                Color32::from_rgb(218, 55, 60)
            };
            hang_painter.rect_filled(hang_rect, CornerRadius::same(21), hang_bg);
            hang_painter.rect_stroke(
                hang_rect,
                CornerRadius::same(21),
                Stroke::new(1.0, Color32::from_rgb(255, 110, 115)),
                egui::StrokeKind::Inside,
            );

            // Иконка трубки (Material Icon call_end) + текст "Завершить звонок"
            let icon_pos = Pos2::new(hang_rect.left() + 20.0, hang_rect.center().y);
            hang_painter.text(
                icon_pos,
                egui::Align2::CENTER_CENTER,
                ICON_CALL_END,
                egui::FontId::proportional(19.0),
                Color32::WHITE,
            );

            let label_pos = Pos2::new(hang_rect.left() + 36.0, hang_rect.center().y);
            hang_painter.text(
                label_pos,
                egui::Align2::LEFT_CENTER,
                hang_text,
                egui::FontId::proportional(12.0),
                Color32::WHITE,
            );

            if hang_resp.clicked() {
                self.is_streaming.store(false, Ordering::Relaxed);
                if let Some(cli) = self.p2p_client.take() {
                    cli.stop();
                }
                if let Some(srv) = self.p2p_server.take() {
                    srv.stop();
                }
                if self.is_fullscreen {
                    self.is_fullscreen = false;
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                }
                self.active_tab = Tab::Lobby;
            }
            hang_resp.on_hover_text(self.t("dock_end_call_tip"));
        });
    }

    /// Отрисовка видео-экрана во весь размер окна (Fullscreen) в кинематографичном темном стиле
    fn render_fullscreen_stage(&mut self, ui: &mut egui::Ui) {
        let full_rect = ui.available_rect_before_wrap();

        // 1. Чистый кинематографичный черный фон для всего экрана
        ui.painter().rect_filled(full_rect, CornerRadius::ZERO, Color32::from_rgb(10, 12, 16));

        // 2. Расчет 16:9 области сцены
        let stage_rect = if self.fit_aspect_ratio {
            aspect_fit(full_rect, 16.0, 9.0)
        } else {
            full_rect
        };

        // 3. Отрисовка видеопотока или заглушки на сцене
        {
            let painter = ui.painter();
            self.render_stream_surface(painter, stage_rect, self.active_stage);
        }

        // 4. Плавающая верхняя статусная плашка HUD (компактный glass pill)
        let top_hud_w = (stage_rect.width() - 40.0).clamp(320.0, 780.0);
        let top_hud_rect = Rect::from_center_size(
            Pos2::new(stage_rect.center().x, full_rect.top() + 30.0),
            Vec2::new(top_hud_w, 38.0),
        );

        ui.painter().rect_filled(top_hud_rect, CornerRadius::same(19), Color32::from_rgba_unmultiplied(18, 22, 30, 225));
        ui.painter().rect_stroke(top_hud_rect, CornerRadius::same(19), Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 30)), egui::StrokeKind::Inside);

        let is_stream = self.is_streaming.load(Ordering::Relaxed);
        let mut hud_ui = ui.new_child(egui::UiBuilder::new().max_rect(top_hud_rect.shrink2(Vec2::new(14.0, 0.0))));
        hud_ui.horizontal_centered(|ui| {
            // Точка статуса (красная если стрим, зеленая если готов)
            let (dot_rect, _) = ui.allocate_exact_size(Vec2::new(10.0, 10.0), egui::Sense::hover());
            let dot_col = if is_stream { Color32::from_rgb(220, 38, 38) } else { Color32::from_rgb(35, 165, 90) };
            ui.painter().circle_filled(dot_rect.center(), 4.5, dot_col);

            ui.add_space(4.0);
            let stage_name = match self.active_stage {
                StageSource::RemoteDesktop => self.t("feed_peer_screen"),
                StageSource::RemoteCamera => self.t("feed_peer_camera"),
                StageSource::LocalDesktop => self.t("feed_local_screen"),
                StageSource::LocalCamera => self.t("feed_local_camera"),
            };
            ui.label(egui::RichText::new(stage_name).color(Color32::WHITE).strong().size(13.0));

            let (w, h, res_name) = self.available_resolutions[self.selected_res_idx];
            let (_, p_name) = self.get_effective_preset();
            let live = self
                .p2p_server
                .as_ref()
                .and_then(|s| s.telemetry.read().ok().map(|t| t.clone()))
                .or_else(|| {
                    self.p2p_client
                        .as_ref()
                        .and_then(|c| c.telemetry.read().ok().map(|t| t.clone()))
                });

            // Numbers below are measured, not configured: a still screen honestly
            // reports 0 fps instead of the value chosen in settings.
            let status_text = match live {
                Some(t) if t.is_connected => format!(
                    "• {} • {:.0} fps • {:.1} Mbps • cq{} • {:.0} ms • {} {:.0}fps",
                    res_name,
                    t.fps,
                    t.bitrate_mbps,
                    t.quality,
                    t.latency_ms,
                    self.preview_stats.source().label(),
                    self.preview_stats.fps()
                ),
                _ => format!(
                    "• {} • до {} FPS • {} • {:.1} ms",
                    res_name,
                    self.target_fps,
                    p_name,
                    self.estimate_latency_ms()
                ),
            };
            ui.label(egui::RichText::new(status_text)
                .color(Color32::from_rgb(140, 185, 255))
                .size(11.5));

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Кнопка выхода из fullscreen
                let exit_label = format!("{} {}", ICON_FULLSCREEN_EXIT, self.t("stage_exit_fullscreen"));
                let exit_btn = egui::Button::new(
                    egui::RichText::new(exit_label).size(11.0).strong().color(Color32::WHITE)
                )
                .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 22))
                .stroke(Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 40)))
                .corner_radius(CornerRadius::same(6));

                if ui.add(exit_btn).clicked() {
                    self.is_fullscreen = false;
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                }

                // Переключатель пропорций
                let aspect_label = if self.fit_aspect_ratio { "16:9" } else { self.t("stage_fill") };
                let aspect_btn = egui::Button::new(
                    egui::RichText::new(aspect_label).size(11.0).strong().color(Color32::from_rgb(200, 215, 245))
                )
                .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 14))
                .stroke(Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 30)))
                .corner_radius(CornerRadius::same(6));

                if ui.add(aspect_btn).clicked() {
                    self.fit_aspect_ratio = !self.fit_aspect_ratio;
                }
            });
        });

        // 5. Плавающий нижний Dock (5 круглых векторных кнопок)
        let dock_center = Pos2::new(stage_rect.center().x, full_rect.bottom() - 40.0);
        self.render_call_dock(ui, dock_center);
    }

    /// Главный экран: Большой экран сцены слева + управление и карточки участников справа (на всю высоту окна)
    /// Главный экран: Профессиональный интерфейс звонка и стрима в стиле Discord
    /// - Полное использование всей площади экрана без пустых полос и белых зон
    /// - Сцена слева с идеальным 16:9 отображением без искажений
    /// - Стеклянные плашки статистики (1080p60, P3, latency) и управления поверх видео
    /// - Сайдбар справа с полноразмерными 16:9 карточками участников и подсветкой фокуса
    /// - Нижний плавающий Dock управления звонком (Микрофон, Звук, Камера, Стрим, Завершить)
    fn render_call_and_stream(&mut self, ui: &mut egui::Ui) {
        let avail_rect = ui.available_rect_before_wrap();
        let total_w = avail_rect.width();
        let total_h = avail_rect.height();

        // Высота нижней плавающей панели управления (Dock)
        let dock_h = 56.0;
        let main_h = (total_h - dock_h - 10.0).max(180.0);

        // Ширина сайдбара участников
        let right_w = if self.is_sidebar_collapsed {
            0.0
        } else {
            310.0_f32.min(total_w * 0.28).max(220.0)
        };
        let left_w = if self.is_sidebar_collapsed {
            total_w
        } else {
            (total_w - right_w - 10.0).max(200.0)
        };

        // Координаты областей
        let stage_container_rect = Rect::from_min_size(
            avail_rect.min,
            Vec2::new(left_w, main_h),
        );
        let sidebar_rect = Rect::from_min_size(
            Pos2::new(avail_rect.left() + left_w + 10.0, avail_rect.top()),
            Vec2::new(right_w, main_h),
        );
        let dock_rect = Rect::from_min_size(
            Pos2::new(avail_rect.left(), avail_rect.top() + main_h + 10.0),
            Vec2::new(total_w, dock_h),
        );

        let painter = ui.painter().clone();
        let th = self.app_theme.colors();
        let is_srv_conn = self.p2p_server.as_ref().map(|s| s.is_connected.load(Ordering::Relaxed)).unwrap_or(false);
        let is_cli_conn = self.p2p_client.as_ref().map(|c| c.is_connected.load(Ordering::Relaxed)).unwrap_or(false);
        let is_peer_connected = is_srv_conn || is_cli_conn;

        // ==========================================
        // 1. ОСНОВНАЯ СЦЕНА (STAGE VIEW)
        // ==========================================
        painter.rect_filled(stage_container_rect, CornerRadius::same(10), th.bg_stage);
        painter.rect_stroke(stage_container_rect, CornerRadius::same(10), Stroke::new(1.0, th.border), egui::StrokeKind::Inside);

        // Расчет 16:9 прямоугольника видео внутри сцены
        let video_rect = if self.fit_aspect_ratio {
            aspect_fit(stage_container_rect.shrink(6.0), 16.0, 9.0)
        } else {
            stage_container_rect.shrink(6.0)
        };

        // Отрисовка видеопотока
        self.render_stream_surface(&painter, video_rect, self.active_stage);

        // Двойной клик по сцене переключает полноэкранный режим
        let stage_resp = ui.interact(video_rect, egui::Id::new("stage_interaction"), egui::Sense::click());
        if stage_resp.double_clicked() {
            self.is_fullscreen = !self.is_fullscreen;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.is_fullscreen));
        }

        // Верхний правый угол: Кнопки управления сценой (Масштаб, Fullscreen)
        let ctrl_w = 195.0;
        let overlay_ctrl_rect = Rect::from_min_size(
            Pos2::new(video_rect.right() - ctrl_w - 12.0, video_rect.top() + 12.0),
            Vec2::new(ctrl_w, 30.0),
        );
        let mut ctrl_ui = ui.new_child(egui::UiBuilder::new().max_rect(overlay_ctrl_rect));
        ctrl_ui.horizontal_centered(|ui| {
            let aspect_btn = if self.fit_aspect_ratio { "16:9" } else { self.t("stage_fill") };
            let (btn_fill, btn_col) = if self.app_theme == AppTheme::LightClean {
                (Color32::from_rgba_unmultiplied(255, 255, 255, 230), Color32::from_rgb(24, 28, 38))
            } else {
                (Color32::from_rgba_unmultiplied(20, 24, 32, 210), Color32::WHITE)
            };
            if ui.add(egui::Button::new(egui::RichText::new(aspect_btn).size(11.0).color(btn_col))
                .fill(btn_fill)).clicked()
            {
                self.fit_aspect_ratio = !self.fit_aspect_ratio;
            }

            let fs_fill = if self.app_theme == AppTheme::LightClean {
                Color32::from_rgb(88, 101, 242)
            } else {
                Color32::from_rgba_unmultiplied(40, 48, 64, 230)
            };
            let fs_label = format!("{} {}", ICON_FULLSCREEN, self.t("stage_fullscreen"));
            if ui.add(egui::Button::new(egui::RichText::new(fs_label).strong().size(11.0).color(Color32::WHITE))
                .fill(fs_fill)).clicked()
            {
                self.is_fullscreen = true;
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
            }
        });

        // ==========================================
        // 2. САЙДБАР УЧАСТНИКОВ И ПОТОКОВ
        // ==========================================
        if !self.is_sidebar_collapsed {
            painter.rect_filled(sidebar_rect, CornerRadius::same(10), th.bg_card);
            painter.rect_stroke(sidebar_rect, CornerRadius::same(10), Stroke::new(1.0, th.border), egui::StrokeKind::Inside);

            let mut sb_ui = ui.new_child(egui::UiBuilder::new().max_rect(sidebar_rect.shrink(10.0)));
            sb_ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(self.t("sidebar_title")).strong().size(11.5).color(th.text_sub));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let room_label = if !self.server_code_str.is_empty() {
                            format!("{}: {}", self.t("code_prefix"), self.server_code_str)
                        } else {
                            self.t("room_p2p_default").to_string()
                        };
                        ui.label(egui::RichText::new(room_label).size(10.5).color(th.text_sub));
                    });
                });
                ui.add_space(8.0);

                let scroll_h = ui.available_height();
                egui::ScrollArea::vertical()
                    .max_height(scroll_h)
                    .show(ui, |ui| {
                        let dt_status = if self.remote_desktop_texture.is_some() {
                            self.t("status_live_1080p")
                        } else if is_peer_connected {
                            self.t("status_stream_stopped")
                        } else {
                            self.t("status_waiting_call")
                        };
                        self.render_feed_card(ui, self.t("feed_peer_screen"), dt_status, StageSource::RemoteDesktop);
                        ui.add_space(8.0);

                        let cam_status = if self.remote_camera_texture.is_some() {
                            self.t("status_live_720p")
                        } else if is_peer_connected {
                            self.t("status_camera_off")
                        } else {
                            self.t("status_waiting_call")
                        };
                        self.render_feed_card(ui, self.t("feed_peer_camera"), cam_status, StageSource::RemoteCamera);
                        ui.add_space(8.0);
                        let is_stream = self.is_streaming.load(Ordering::Relaxed);
                        let dt_status = if is_stream { self.t("feed_screen_live") } else { self.t("feed_screen_idle") };
                        self.render_feed_card(ui, self.t("feed_local_screen"), dt_status, StageSource::LocalDesktop);
                        ui.add_space(8.0);
                        self.render_feed_card(ui, self.t("feed_local_camera"), "BisonCam • 720p 30 FPS", StageSource::LocalCamera);
                    });
            });
        }

        // ==========================================
        // 3. ПЛАВАЮЩИЙ НИЖНИЙ DOCK УПРАВЛЕНИЯ ЗВОНКОМ (MATERIAL DESIGN ICON STYLE)
        // ==========================================
        let dock_center = dock_rect.center();
        self.render_call_dock(ui, dock_center);
    }

    fn render_stream_surface(&self, painter: &egui::Painter, rect: Rect, source: StageSource) {
        let th = self.app_theme.colors();
        let is_thumb = rect.height() < 220.0;
        let is_srv_conn = self.p2p_server.as_ref().map(|s| s.is_connected.load(Ordering::Relaxed)).unwrap_or(false);
        let is_cli_conn = self.p2p_client.as_ref().map(|c| c.is_connected.load(Ordering::Relaxed)).unwrap_or(false);
        let is_peer_connected = is_srv_conn || is_cli_conn;

        let surface_placeholder_bg = if self.is_fullscreen {
            Color32::from_rgb(14, 17, 24)
        } else {
            th.bg_stage
        };
        let surface_avatar_bg = if self.is_fullscreen {
            Color32::from_rgb(26, 31, 42)
        } else {
            th.bg_card_secondary
        };
        let surface_avatar_stroke = if self.is_fullscreen {
            Color32::from_rgb(45, 53, 70)
        } else {
            th.border
        };
        let t_main_color = if self.is_fullscreen {
            Color32::from_rgb(230, 235, 245)
        } else {
            th.text_main
        };
        let t_sub_color = if self.is_fullscreen {
            Color32::from_rgb(140, 155, 175)
        } else {
            th.text_sub
        };
        let mon_stroke_col = if self.is_fullscreen {
            Color32::from_rgb(80, 100, 135)
        } else if self.app_theme == AppTheme::LightClean {
            Color32::from_rgb(140, 150, 170)
        } else {
            Color32::from_rgb(70, 85, 115)
        };

        match source {
            StageSource::LocalDesktop => {
                let is_stream = self.is_streaming.load(Ordering::Relaxed);
                if let Some(tex) = self.desktop_texture.as_ref() {
                    let draw_rect = if self.fit_aspect_ratio {
                        let [tw, th_sz] = tex.size();
                        aspect_fit(rect, tw as f32, th_sz as f32)
                    } else {
                        rect
                    };
                    if draw_rect != rect {
                        painter.rect_filled(rect, CornerRadius::same(6), Color32::from_rgb(12, 14, 18));
                    }

                    // При отключенной демонстрации экран слегка диммируется
                    let tint = if is_stream {
                        Color32::WHITE
                    } else {
                        Color32::from_rgba_unmultiplied(120, 125, 140, 130)
                    };

                    painter.image(
                        tex.id(),
                        draw_rect,
                        Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                        tint,
                    );

                    if !is_stream {
                        // Затемняющий оверлей и аккуратная плашка статуса паузы
                        painter.rect_filled(draw_rect, CornerRadius::same(6), Color32::from_rgba_unmultiplied(10, 12, 18, 110));
                        let badge_w = if is_thumb { 104.0 } else { 190.0 };
                        let badge_h = if is_thumb { 22.0 } else { 30.0 };
                        let badge_rect = Rect::from_center_size(draw_rect.center(), Vec2::new(badge_w, badge_h));
                        let rad = (badge_h * 0.5) as u8;
                        painter.rect_filled(badge_rect, CornerRadius::same(rad), Color32::from_rgba_unmultiplied(22, 26, 36, 215));
                        painter.rect_stroke(badge_rect, CornerRadius::same(rad), Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 30)), egui::StrokeKind::Inside);
                        painter.text(
                            badge_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            if is_thumb { self.t("badge_demo_paused_short") } else { self.t("badge_demo_paused_full") },
                            egui::FontId::proportional(if is_thumb { 10.0 } else { 12.0 }),
                            Color32::from_rgb(220, 225, 235),
                        );
                    }
                } else {
                    painter.rect_filled(rect, CornerRadius::same(6), surface_placeholder_bg);
                    if !is_thumb {
                        painter.text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            self.t("surface_capture_connecting"),
                            egui::FontId::proportional(14.0),
                            t_main_color,
                        );
                    }
                }
            }
            StageSource::LocalCamera => {
                if let Some(tex) = self.camera_texture.as_ref() {
                    let draw_rect = if self.fit_aspect_ratio {
                        let [tw, th_sz] = tex.size();
                        aspect_fit(rect, tw as f32, th_sz as f32)
                    } else {
                        rect
                    };
                    if draw_rect != rect {
                        painter.rect_filled(rect, CornerRadius::same(6), Color32::from_rgb(12, 14, 18));
                    }
                    painter.image(
                        tex.id(),
                        draw_rect,
                        Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                } else {
                    painter.rect_filled(rect, CornerRadius::same(6), surface_placeholder_bg);
                    let center = rect.center();
                    let avatar_c = Pos2::new(center.x, center.y - 12.0);
                    painter.circle_filled(avatar_c, 20.0, surface_avatar_bg);
                    painter.circle_stroke(avatar_c, 20.0, Stroke::new(1.5, surface_avatar_stroke));
                    let cam_icon_col = if self.app_theme == AppTheme::LightClean { Color32::from_rgb(80, 92, 114) } else { Color32::WHITE };
                    draw_vector_camera(painter, avatar_c, cam_icon_col, !self.is_camera_on);
                    painter.text(
                        Pos2::new(center.x, center.y + 16.0),
                        egui::Align2::CENTER_CENTER,
                        if self.is_camera_on { self.t("surface_camera_loading") } else { self.t("surface_camera_off") },
                        egui::FontId::proportional(11.0),
                        t_sub_color,
                    );
                }
            }
            StageSource::RemoteDesktop => {
                if let Some(tex) = self.remote_desktop_texture.as_ref() {
                    let draw_rect = if self.fit_aspect_ratio {
                        let [tw, th_sz] = tex.size();
                        aspect_fit(rect, tw as f32, th_sz as f32)
                    } else {
                        rect
                    };
                    if draw_rect != rect {
                        painter.rect_filled(rect, CornerRadius::same(6), Color32::from_rgb(12, 14, 18));
                    }
                    painter.image(
                        tex.id(),
                        draw_rect,
                        Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                } else {
                    painter.rect_filled(rect, CornerRadius::same(6), surface_placeholder_bg);
                    if is_thumb {
                        let center = rect.center();
                        let avatar_c = Pos2::new(center.x, center.y - 12.0);
                        painter.circle_filled(avatar_c, 20.0, surface_avatar_bg);
                        painter.circle_stroke(avatar_c, 20.0, Stroke::new(1.5, Color32::from_rgb(88, 101, 242)));
                        draw_vector_screen(painter, avatar_c, Color32::from_rgb(88, 101, 242));
                        painter.text(
                            Pos2::new(center.x, center.y + 16.0),
                            egui::Align2::CENTER_CENTER,
                            if is_peer_connected { self.t("surface_stream_stopped_thumb") } else { self.t("surface_waiting_stream_thumb") },
                            egui::FontId::proportional(11.0),
                            t_sub_color,
                        );
                    } else {
                        let center = rect.center();
                        // Стилизованный контур монитора
                        let mon_w = 90.0;
                        let mon_h = 56.0;
                        let mon_rect = Rect::from_center_size(Pos2::new(center.x, center.y - 45.0), Vec2::new(mon_w, mon_h));
                        painter.rect_stroke(mon_rect, CornerRadius::same(5), Stroke::new(2.0, mon_stroke_col), egui::StrokeKind::Inside);
                        painter.line_segment(
                            [Pos2::new(center.x, mon_rect.bottom()), Pos2::new(center.x, mon_rect.bottom() + 10.0)],
                            Stroke::new(2.0, mon_stroke_col),
                        );
                        painter.line_segment(
                            [Pos2::new(center.x - 20.0, mon_rect.bottom() + 10.0), Pos2::new(center.x + 20.0, mon_rect.bottom() + 10.0)],
                            Stroke::new(2.0, mon_stroke_col),
                        );

                        let (t_main, t_sub) = if is_peer_connected {
                            (self.t("surface_stream_stopped_title"), self.t("surface_stream_stopped_desc"))
                        } else {
                            (self.t("surface_waiting_stream_title"), self.t("surface_waiting_stream_desc"))
                        };

                        painter.text(
                            Pos2::new(center.x, center.y + 20.0),
                            egui::Align2::CENTER_CENTER,
                            t_main,
                            egui::FontId::proportional(15.0),
                            t_main_color,
                        );
                        painter.text(
                            Pos2::new(center.x, center.y + 44.0),
                            egui::Align2::CENTER_CENTER,
                            t_sub,
                            egui::FontId::proportional(12.0),
                            t_sub_color,
                        );
                    }
                }
            }
            StageSource::RemoteCamera => {
                if let Some(tex) = self.remote_camera_texture.as_ref() {
                    let draw_rect = if self.fit_aspect_ratio {
                        let [tw, th_sz] = tex.size();
                        aspect_fit(rect, tw as f32, th_sz as f32)
                    } else {
                        rect
                    };
                    if draw_rect != rect {
                        painter.rect_filled(rect, CornerRadius::same(6), Color32::from_rgb(12, 14, 18));
                    }
                    painter.image(
                        tex.id(),
                        draw_rect,
                        Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                } else {
                    painter.rect_filled(rect, CornerRadius::same(6), surface_placeholder_bg);
                    let figure_col = if self.app_theme == AppTheme::LightClean {
                        Color32::from_rgb(115, 128, 150)
                    } else {
                        Color32::from_rgb(175, 185, 205)
                    };

                    if is_thumb {
                        let center = rect.center();
                        let avatar_c = Pos2::new(center.x, center.y - 12.0);
                        draw_vector_avatar(painter, avatar_c, 20.0, surface_avatar_bg, surface_avatar_stroke, figure_col);
                        painter.text(
                            Pos2::new(center.x, center.y + 16.0),
                            egui::Align2::CENTER_CENTER,
                            self.t("surface_camera_off"),
                            egui::FontId::proportional(11.0),
                            t_sub_color,
                        );
                    } else {
                        let center = rect.center();
                        let avatar_center = Pos2::new(center.x, center.y - 45.0);
                        draw_vector_avatar(painter, avatar_center, 34.0, surface_avatar_bg, surface_avatar_stroke, figure_col);

                        let (t_main, t_sub) = if is_peer_connected {
                            (self.t("surface_peer_cam_off_title"), self.t("surface_peer_cam_off_desc"))
                        } else {
                            (self.t("surface_peer_cam_off_title"), self.t("surface_peer_cam_waiting_desc"))
                        };

                        painter.text(
                            Pos2::new(center.x, center.y + 20.0),
                            egui::Align2::CENTER_CENTER,
                            t_main,
                            egui::FontId::proportional(15.0),
                            t_main_color,
                        );
                        painter.text(
                            Pos2::new(center.x, center.y + 44.0),
                            egui::Align2::CENTER_CENTER,
                            t_sub,
                            egui::FontId::proportional(12.0),
                            t_sub_color,
                        );
                    }
                }
            }
        }
    }

    /// Полноразмерный прямоугольный видео-тайл участника с кнопками при наведении (hover controls)
    fn render_feed_card(&mut self, ui: &mut egui::Ui, title: &str, subtitle: &str, source: StageSource) {
        let th = self.app_theme.colors();
        let is_active_on_stage = self.active_stage == source;
        let card_w = (ui.available_width() - 4.0).max(100.0);
        let card_h = (card_w * (9.0 / 16.0)).clamp(96.0, 155.0);

        // 1. Прямоугольник видео-экрана карточки
        let (rect, response) = ui.allocate_exact_size(Vec2::new(card_w, card_h), egui::Sense::click());
        let is_hovered = response.hovered();

        let painter = ui.painter();

        // Отрисовка видеопотока
        self.render_stream_surface(&painter, rect, source);

        // Рамка активности
        let border_stroke = if is_active_on_stage {
            Stroke::new(2.5, Color32::from_rgb(88, 101, 242))
        } else if is_hovered {
            Stroke::new(1.5, Color32::from_rgb(114, 137, 218))
        } else {
            Stroke::new(1.0, th.border)
        };
        painter.rect_stroke(rect, CornerRadius::same(8), border_stroke, egui::StrokeKind::Inside);

        // Иконка "Во весь экран" в правом нижнем углу карточки
        let fs_icon_rect = Rect::from_min_size(
            Pos2::new(rect.right() - 26.0, rect.bottom() - 26.0),
            Vec2::new(22.0, 22.0),
        );
        let fs_icon_hovered = ui.rect_contains_pointer(fs_icon_rect);
        painter.rect_filled(
            fs_icon_rect,
            CornerRadius::same(4),
            if fs_icon_hovered { Color32::from_rgba_unmultiplied(88, 101, 242, 230) } else { Color32::from_rgba_unmultiplied(18, 20, 26, 200) },
        );
        draw_vector_expand(&painter, fs_icon_rect.center(), Color32::WHITE);

        // Двойной клик -> Во весь экран! Одиночный клик -> Выбор источника на сцену
        if response.double_clicked() || (response.clicked() && fs_icon_hovered) {
            self.active_stage = source;
            self.is_fullscreen = !self.is_fullscreen;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.is_fullscreen));
        } else if response.clicked() {
            self.active_stage = source;
        }

        // 2. Подписи у превью ПОД ЭКРАНОМ
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(title).strong().size(12.5).color(th.text_main));
            if is_active_on_stage {
                ui.label(egui::RichText::new(self.t("status_in_focus")).strong().size(10.5).color(Color32::from_rgb(88, 101, 242)));
            }
        });
        ui.label(egui::RichText::new(subtitle).size(10.5).color(th.text_sub));
        ui.add_space(8.0);
    }

    /// Вкладка: Независимые аудио каналы + 10-секундная звуковая волна на чёрном фоне
    fn render_audio_and_spectrum(&mut self, ui: &mut egui::Ui) {
        let th = self.app_theme.colors();
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.heading(egui::RichText::new(self.t("audio_heading")).color(th.text_main).strong());
                ui.label(egui::RichText::new(self.t("audio_subheading")).color(th.text_sub));
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let is_srv_conn = self.p2p_server.as_ref().map(|s| s.is_connected.load(Ordering::Relaxed)).unwrap_or(false);
                let is_cli_conn = self.p2p_client.as_ref().map(|c| c.is_connected.load(Ordering::Relaxed)).unwrap_or(false);
                if is_srv_conn || is_cli_conn {
                    ui.label(egui::RichText::new(self.t("audio_p2p_active")).color(Color32::from_rgb(35, 165, 90)).strong());
                }
            });
        });

        ui.add_space(12.0);

        // ПАНЕЛЬ НАСТРОЕК АУДИОУСТРОЙСТВ (WASAPI DROP-DOWN PICKERS & SIDETONE)
        egui::Frame::new()
            .fill(th.bg_card)
            .stroke(Stroke::new(1.0, th.border))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(12.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(self.t("audio_devices_title")).strong().size(13.0).color(th.text_main));
                    if ui.button(self.t("btn_refresh")).clicked() {
                        self.input_devices = crate::audio::enumerate_audio_devices(true);
                        self.output_devices = crate::audio::enumerate_audio_devices(false);
                    }
                    let sidetone_label = self.t("audio_sidetone_check");
                    let sidetone_tip = self.t("audio_sidetone_tip");
                    let downmix_label = self.t("audio_downmix_toggle");
                    let downmix_tip = self.t("audio_downmix_tip");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.checkbox(&mut self.listen_to_mic_sidetone, sidetone_label)
                            .on_hover_text(sidetone_tip);
                        ui.add_space(8.0);
                        ui.checkbox(&mut self.downmix_to_mono, downmix_label)
                            .on_hover_text(downmix_tip);
                    });
                });
                ui.add_space(8.0);
                let default_mic_text = self.t("audio_default_mic");
                let default_spk_text = self.t("audio_default_speakers");
                ui.columns(2, |cols| {
                    // Колонка 1: Устройство ввода (Микрофон)
                    cols[0].label(egui::RichText::new(self.t("audio_input_device")).size(12.0).color(th.text_main));
                    let in_curr = if self.input_devices.is_empty() {
                        default_mic_text.to_string()
                    } else {
                        let idx = self.selected_input_device_idx.min(self.input_devices.len() - 1);
                        self.input_devices[idx].name.clone()
                    };
                    egui::ComboBox::from_id_salt("input_audio_device_combo")
                        .selected_text(in_curr)
                        .width(cols[0].available_width() - 10.0)
                        .show_ui(&mut cols[0], |ui| {
                            if self.input_devices.is_empty() {
                                ui.selectable_value(&mut self.selected_input_device_idx, 0, default_mic_text);
                            } else {
                                for (i, dev) in self.input_devices.iter().enumerate() {
                                    ui.selectable_value(&mut self.selected_input_device_idx, i, &dev.name);
                                }
                            }
                        });

                    // Колонка 2: Устройство вывода (Динамики/Наушники)
                    cols[1].label(egui::RichText::new(self.t("audio_output_device")).size(12.0).color(th.text_main));
                    let out_curr = if self.output_devices.is_empty() {
                        default_spk_text.to_string()
                    } else {
                        let idx = self.selected_output_device_idx.min(self.output_devices.len() - 1);
                        self.output_devices[idx].name.clone()
                    };
                    egui::ComboBox::from_id_salt("output_audio_device_combo")
                        .selected_text(out_curr)
                        .width(cols[1].available_width() - 10.0)
                        .show_ui(&mut cols[1], |ui| {
                            if self.output_devices.is_empty() {
                                ui.selectable_value(&mut self.selected_output_device_idx, 0, default_spk_text);
                            } else {
                                for (i, dev) in self.output_devices.iter().enumerate() {
                                    ui.selectable_value(&mut self.selected_output_device_idx, i, &dev.name);
                                }
                            }
                        });
                });
            });

        ui.add_space(8.0);

        // БЕЙДЖ СТАТУСА OPUS RFC 6716 VBR КОДЕКА
        egui::Frame::new()
            .fill(th.bg_card_secondary)
            .stroke(Stroke::new(1.0, th.border))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(egui::Margin::symmetric(12, 7))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("🎙 OPUS RFC 6716 VBR").strong().size(12.0).color(Color32::from_rgb(88, 101, 242)));
                    ui.separator();
                    ui.label(egui::RichText::new(self.t("audio_opus_badge")).size(11.0).color(th.text_sub));
                });
            });

        ui.add_space(10.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            // ИСТОЧНИК 1: ЗВУКОВАЯ ВОЛНА МИКРОФОНА
            let mic_title = self.t("audio_mic_panel_title");
            self.render_waveform_panel(
                ui,
                mic_title,
                true,
            );

            ui.add_space(16.0);

            // ИСТОЧНИК 2: ЗВУКОВАЯ ВОЛНА СИСТЕМЫ / РАБОЧЕГО СТОЛА / МУЗЫКИ
            let dt_title = self.t("audio_desktop_panel_title");
            self.render_waveform_panel(
                ui,
                dt_title,
                false,
            );
        });
    }

    /// Отрисовка отдельной панели с 10-секундной звуковой волной на ЧЁРНОМ фоне и ползунком на пол-экрана
    fn render_waveform_panel(
        &mut self,
        ui: &mut egui::Ui,
        title: &str,
        is_mic: bool,
    ) {
        let th = self.app_theme.colors();
        egui::Frame::new()
            .fill(th.bg_card)
            .stroke(Stroke::new(1.0, th.border))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(14.0)
            .show(ui, |ui| {
                let (peak_db, is_muted) = if is_mic {
                    (self.mic_waveform.peak_db, self.is_mic_muted)
                } else {
                    (self.game_waveform.peak_db, self.is_deafened)
                };

                // Шапка панели
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(title).strong().size(14.0).color(th.text_main));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let status_str = if is_muted {
                            self.t("audio_status_muted").to_string()
                        } else {
                            format!("{}: {:.1} dB", self.t("audio_peak"), peak_db)
                        };
                        let col = if is_muted { th.text_sub } else { Color32::from_rgb(35, 165, 90) };
                        ui.label(egui::RichText::new(status_str).size(12.5).color(col).strong());
                    });
                });
                ui.add_space(8.0);

                // ЧЁРНЫЙ ХОЛСТ 10-СЕКУНДНОЙ ЗВУКОВОЙ ВОЛНЫ
                let canvas_height = 160.0;
                let canvas_width = ui.available_width();
                let (rect, _) = ui.allocate_exact_size(Vec2::new(canvas_width, canvas_height), egui::Sense::hover());

                let painter = ui.painter();
                // Чистый черный фон
                painter.rect_filled(rect, CornerRadius::same(6), Color32::BLACK);
                painter.rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, Color32::from_rgb(40, 44, 52)), egui::StrokeKind::Inside);

                let center_y = rect.center().y;
                let half_h = (rect.height() - 20.0) * 0.5;

                // Горизонтальные опорные линии амплитуды
                painter.line_segment(
                    [Pos2::new(rect.left(), center_y), Pos2::new(rect.right(), center_y)],
                    Stroke::new(1.0, Color32::from_rgb(45, 50, 60)),
                );
                painter.line_segment(
                    [Pos2::new(rect.left(), center_y - half_h), Pos2::new(rect.right(), center_y - half_h)],
                    Stroke::new(0.5, Color32::from_rgb(28, 32, 40)),
                );
                painter.line_segment(
                    [Pos2::new(rect.left(), center_y + half_h), Pos2::new(rect.right(), center_y + half_h)],
                    Stroke::new(0.5, Color32::from_rgb(28, 32, 40)),
                );

                // Вертикальные временные шкалы: -10s, -8s, -6s, -4s, -2s, 0s
                let time_markers = [
                    (0.0, self.t("audio_time_minus_10s")),
                    (0.2, self.t("audio_time_minus_8s")),
                    (0.4, self.t("audio_time_minus_6s")),
                    (0.6, self.t("audio_time_minus_4s")),
                    (0.8, self.t("audio_time_minus_2s")),
                    (1.0, self.t("audio_time_now")),
                ];
                for &(frac, label) in &time_markers {
                    let tx = rect.left() + frac * rect.width();
                    painter.line_segment(
                        [Pos2::new(tx, rect.top()), Pos2::new(tx, rect.bottom())],
                        Stroke::new(0.5, Color32::from_rgb(32, 36, 44)),
                    );
                    let (pos, align) = if frac >= 0.95 {
                        (Pos2::new(rect.right() - 6.0, rect.top() + 10.0), egui::Align2::RIGHT_CENTER)
                    } else if frac <= 0.05 {
                        (Pos2::new(rect.left() + 6.0, rect.top() + 10.0), egui::Align2::LEFT_CENTER)
                    } else {
                        (Pos2::new(tx + 4.0, rect.top() + 10.0), egui::Align2::LEFT_CENTER)
                    };
                    painter.text(
                        pos,
                        align,
                        label,
                        egui::FontId::proportional(9.0),
                        Color32::from_rgb(140, 145, 160),
                    );
                }

                // Отрисовка самой звуковой волны (500 срезов за 10 секунд)
                let wave_history = if is_mic {
                    &self.mic_waveform.history
                } else {
                    &self.game_waveform.history
                };

                let n_pts = wave_history.len();
                let wave_color = if is_mic {
                    Color32::from_rgb(35, 165, 90) // Неоновый зеленый (микрофон)
                } else {
                    Color32::from_rgb(0, 176, 244)  // Неоновый циан/синий (музыка/система)
                };
                let crest_color = if is_mic {
                    Color32::from_rgb(85, 230, 140)
                } else {
                    Color32::from_rgb(120, 215, 255)
                };

                for (i, pt) in wave_history.iter().enumerate() {
                    let x = rect.left() + (i as f32 / n_pts as f32) * rect.width();

                    // Идеально симметричная огибающая волны относительно центральной оси
                    let amp = pt.max_val.abs().max(pt.min_val.abs()).max(pt.rms * 1.5).clamp(0.0, 1.0);
                    let h = amp * (half_h - 4.0);

                    let y1 = (center_y - h).clamp(rect.top() + 4.0, rect.bottom() - 4.0);
                    let y2 = (center_y + h).clamp(rect.top() + 4.0, rect.bottom() - 4.0);

                    if h >= 1.5 {
                        painter.line_segment(
                            [Pos2::new(x, y1), Pos2::new(x, y2)],
                            Stroke::new(1.8, wave_color),
                        );
                        // Симметричные неоновые гребни волны сверху и снизу
                        painter.circle_filled(Pos2::new(x, y1), 1.0, crest_color);
                        painter.circle_filled(Pos2::new(x, y2), 1.0, crest_color);
                    } else {
                        // Чистая фоновая линия тишины строго по центру
                        painter.circle_filled(Pos2::new(x, center_y), 0.7, Color32::from_rgb(50, 56, 68));
                    }
                }

                ui.add_space(10.0);

                // Ползунок громкости, растянутый в половину экрана
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(self.t("audio_volume_label")).color(th.text_main));
                    ui.spacing_mut().slider_width = (ui.available_width() * 0.50).clamp(240.0, 700.0);
                    if is_mic {
                        let btn_text = if self.is_mic_muted { self.t("audio_unmute_mic") } else { self.t("audio_mute_mic") };
                        if ui.button(btn_text).clicked() {
                            self.is_mic_muted = !self.is_mic_muted;
                        }
                        ui.add(egui::Slider::new(&mut self.mic_waveform.volume, 0.0..=2.0).suffix("x"));
                    } else {
                        let btn_text = if self.is_deafened { self.t("audio_unmute_sound") } else { self.t("audio_mute_sound") };
                        if ui.button(btn_text).clicked() {
                            self.is_deafened = !self.is_deafened;
                        }
                        ui.add(egui::Slider::new(&mut self.game_waveform.volume, 0.0..=2.0).suffix("x"));
                    }
                });
            });
    }

    /// Вкладка: Компактная матрица бенчмарка Resolution × FPS Target
    fn render_benchmark_matrix(&mut self, ui: &mut egui::Ui) {
        let th = self.app_theme.colors();
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.heading(egui::RichText::new(self.t("bench_heading")).color(th.text_main).strong());
                ui.label(egui::RichText::new(format!("{}: {}x{} @ {} {}", self.t("bench_desc_prefix"), self.native_width, self.native_height, self.native_hz, self.t("bench_hz_suffix"))).color(th.text_sub));
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.is_benchmarking {
                    ui.spinner();
                    ui.label(egui::RichText::new(self.t("bench_probing")).color(th.text_main));
                } else {
                    if ui.button(egui::RichText::new(self.t("bench_rerun_btn")).color(th.text_main)).clicked() {
                        self.start_benchmark();
                    }
                }
            });
        });

        ui.add_space(12.0);

        if let Some(matrix) = self.benchmark_matrix.clone() {
            let mut picked_cell = None;
            let fps_columns = crate::benchmark::compute_fps_targets(self.native_hz);

            egui::Frame::new()
                .fill(th.bg_card)
                .stroke(Stroke::new(1.0, th.border))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(14.0)
                .show(ui, |ui| {
                    egui::Grid::new("matrix_grid")
                        .striped(true)
                        .min_col_width(120.0)
                        .spacing([12.0, 10.0])
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(self.t("bench_table_resolution")).strong().size(13.0).color(th.text_main));
                            for &fps in &fps_columns {
                                let budget = 1000.0 / fps as f32;
                                let header = if fps == self.native_hz && fps > 60 {
                                    format!("{} FPS (Max, {:.1}ms)", fps, budget)
                                } else {
                                    format!("{} FPS ({:.1}ms)", fps, budget)
                                };
                                ui.label(egui::RichText::new(header).strong().size(13.0).color(th.text_main));
                            }
                            ui.end_row();

                            for (r_idx, row) in matrix.iter().enumerate() {
                                ui.label(egui::RichText::new(&row.res_label).strong().size(13.0).color(th.text_main));

                                for cell in &row.cells {
                                    let (bg_color, border_color, text_color, label_text) = if cell.is_viable {
                                        if self.app_theme == AppTheme::LightClean {
                                            (
                                                Color32::from_rgb(228, 248, 236),
                                                Color32::from_rgb(52, 199, 89),
                                                Color32::from_rgb(18, 120, 56),
                                                format!("{} ({:.1} ms)", cell.best_preset_name, cell.latency_ms),
                                            )
                                        } else {
                                            (
                                                Color32::from_rgb(24, 46, 32),
                                                Color32::from_rgb(35, 165, 90),
                                                Color32::WHITE,
                                                format!("{} ({:.1} ms)", cell.best_preset_name, cell.latency_ms),
                                            )
                                        }
                                    } else {
                                        if self.app_theme == AppTheme::LightClean {
                                            (
                                                Color32::from_rgb(254, 236, 238),
                                                Color32::from_rgb(240, 80, 85),
                                                Color32::from_rgb(185, 25, 30),
                                                format!("{} ({:.1} ms)", self.t("bench_limit"), cell.latency_ms),
                                            )
                                        } else {
                                            (
                                                Color32::from_rgb(48, 24, 28),
                                                Color32::from_rgb(218, 55, 60),
                                                Color32::WHITE,
                                                format!("{} ({:.1} ms)", self.t("bench_limit"), cell.latency_ms),
                                            )
                                        }
                                    };

                                    let btn = egui::Button::new(
                                        egui::RichText::new(label_text).color(text_color).strong().size(12.0),
                                    )
                                    .fill(bg_color)
                                    .stroke(Stroke::new(1.0, border_color))
                                    .corner_radius(CornerRadius::same(6));

                                    if ui.add(btn).on_hover_text(format!("{} {}x{} @ {} FPS {} {}", self.t("bench_click_apply"), cell.res_width, cell.res_height, cell.fps, self.t("bench_with_preset"), cell.best_preset_name)).clicked() {
                                        picked_cell = Some((r_idx, cell.fps, cell.best_preset_id));
                                    }
                                }
                                ui.end_row();
                            }
                        });
                });

            ui.add_space(14.0);

            // Информационная карточка аппаратной архитектуры NVENC для заполнения пространства
            egui::Frame::new()
                .fill(th.bg_card)
                .stroke(Stroke::new(1.0, th.border))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(16.0)
                .show(ui, |ui| {
                    ui.label(egui::RichText::new(self.t("bench_analysis_title")).strong().size(13.5).color(th.text_main));
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(self.t("bench_analysis_body")).size(11.5).color(th.text_sub));
                });

            if let Some((r_idx, fps, preset_id)) = picked_cell {
                self.selected_res_idx = r_idx;
                self.target_fps = fps;
                self.preset_mode = match preset_id {
                    1 => PresetMode::ManualP1,
                    3 => PresetMode::ManualP3,
                    4 => PresetMode::ManualP4,
                    5 => PresetMode::ManualP5,
                    6 => PresetMode::ManualP6,
                    7 => PresetMode::ManualP7,
                    _ => PresetMode::AutoSmart,
                };
                self.active_tab = Tab::CallAndStream;
            }
        }
    }

    /// Вкладка: Тонкие настройки энкодера NVENC
    fn render_encoder_settings(&mut self, ui: &mut egui::Ui) {
        let th = self.app_theme.colors();
        ui.heading(egui::RichText::new(self.t("nvenc_heading")).color(th.text_main).strong());
        ui.label(egui::RichText::new(self.t("nvenc_subheading")).color(th.text_sub));
        ui.add_space(12.0);

        egui::Frame::new()
            .fill(th.bg_card)
            .stroke(Stroke::new(1.0, th.border))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(16.0)
            .show(ui, |ui| {
                ui.label(egui::RichText::new(self.t("nvenc_preset_mode")).strong().size(13.0).color(th.text_main));
                ui.add_space(6.0);

                let presets = [
                    (PresetMode::AutoSmart, "Auto (Smart)"),
                    (PresetMode::ManualP1, "P1 (Fastest)"),
                    (PresetMode::ManualP3, "P3"),
                    (PresetMode::ManualP4, "P4"),
                    (PresetMode::ManualP5, "P5"),
                    (PresetMode::ManualP6, "P6"),
                    (PresetMode::ManualP7, "P7 (Max Quality)"),
                ];

                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                    for (mode, label) in presets {
                        let is_active = self.preset_mode == mode;
                        let fill = if is_active {
                            Color32::from_rgb(88, 101, 242)
                        } else {
                            th.pill_inactive_bg
                        };
                        let text_col = if is_active {
                            Color32::WHITE
                        } else {
                            th.text_main
                        };
                        let btn = egui::Button::new(egui::RichText::new(label).size(12.0).strong().color(text_col))
                            .fill(fill)
                            .stroke(Stroke::new(1.0, if is_active { Color32::from_rgb(71, 82, 196) } else { th.border }))
                            .corner_radius(CornerRadius::same(6));
                        if ui.add(btn).clicked() {
                            self.preset_mode = mode;
                        }
                    }
                });

                ui.add_space(16.0);
                ui.separator();
                ui.add_space(14.0);

                ui.label(egui::RichText::new(format!("{} {}", self.t("nvenc_cq_title"), self.cq_quality)).strong().size(13.0).color(th.text_main));
                ui.label(egui::RichText::new(self.t("nvenc_cq_desc")).size(11.0).color(th.text_sub));
                ui.add_space(4.0);
                ui.spacing_mut().slider_width = (ui.available_width() * 0.65).clamp(240.0, 700.0);
                ui.add(egui::Slider::new(&mut self.cq_quality, 16..=32));

                ui.add_space(16.0);
                ui.separator();
                ui.add_space(14.0);

                ui.label(egui::RichText::new(format!("{} {} Mbps", self.t("nvenc_bitrate_title"), self.max_bitrate_mbps)).strong().size(13.0).color(th.text_main));
                ui.label(egui::RichText::new(self.t("nvenc_bitrate_desc")).size(11.0).color(th.text_sub));
                ui.add_space(4.0);
                if ui.add(egui::Slider::new(&mut self.max_bitrate_mbps, 5..=60).suffix(" Mbps")).changed() {
                    if let Ok(mut cfg) = self.encoder_config.write() {
                        cfg.max_bitrate_mbps = self.max_bitrate_mbps;
                    }
                }

                if let Ok(mut cfg) = self.encoder_config.write() {
                    cfg.cq_quality = self.cq_quality;
                    cfg.max_bitrate_mbps = self.max_bitrate_mbps;
                    cfg.preset_index = match self.preset_mode {
                        PresetMode::AutoSmart | PresetMode::ManualP1 => 1,
                        PresetMode::ManualP3 => 3,
                        PresetMode::ManualP4 => 4,
                        PresetMode::ManualP5 => 5,
                        PresetMode::ManualP6 => 6,
                        PresetMode::ManualP7 => 7,
                    };
                }

                ui.add_space(16.0);
                ui.separator();
                ui.add_space(12.0);

                ui.label(egui::RichText::new(self.t("nvenc_arch_title")).strong().size(13.0).color(th.text_main));
                ui.add_space(4.0);
                ui.label(egui::RichText::new(self.t("nvenc_arch_b1")).size(11.5).color(th.text_sub));
                ui.label(egui::RichText::new(self.t("nvenc_arch_b2")).size(11.5).color(th.text_sub));
                ui.label(egui::RichText::new(self.t("nvenc_arch_b3")).size(11.5).color(th.text_sub));
                ui.label(egui::RichText::new(self.t("nvenc_arch_b4")).size(11.5).color(th.text_sub));
            });
    }

    fn start_benchmark(&mut self) {
        self.is_benchmarking = true;
        let (tx, rx): (Sender<Vec<BenchmarkResult>>, Receiver<Vec<BenchmarkResult>>) = channel();
        self.bench_rx = Some(rx);

        thread::spawn(move || {
            let results = run_full_benchmark();
            let _ = tx.send(results);
        });
    }
}

fn build_default_matrix(native_w: u32, native_h: u32, native_hz: u32) -> Vec<MatrixRow> {
    let mock_results = vec![
        BenchmarkResult { resolution: "480p".into(), width: 854, height: 480, preset_name: "P1".into(), preset_id: 1, avg_latency_ms: 2.97, min_latency_ms: 2.77, max_fps: 337, idr_size_kb: 0.1, p_frame_size_kb: 0.1 },
        BenchmarkResult { resolution: "480p".into(), width: 854, height: 480, preset_name: "P3".into(), preset_id: 3, avg_latency_ms: 3.60, min_latency_ms: 3.32, max_fps: 278, idr_size_kb: 0.1, p_frame_size_kb: 0.1 },
        BenchmarkResult { resolution: "480p".into(), width: 854, height: 480, preset_name: "P4".into(), preset_id: 4, avg_latency_ms: 3.61, min_latency_ms: 3.50, max_fps: 276, idr_size_kb: 0.1, p_frame_size_kb: 0.1 },
        BenchmarkResult { resolution: "480p".into(), width: 854, height: 480, preset_name: "P5".into(), preset_id: 5, avg_latency_ms: 3.82, min_latency_ms: 3.65, max_fps: 261, idr_size_kb: 0.1, p_frame_size_kb: 0.1 },
        BenchmarkResult { resolution: "480p".into(), width: 854, height: 480, preset_name: "P6".into(), preset_id: 6, avg_latency_ms: 4.12, min_latency_ms: 3.95, max_fps: 242, idr_size_kb: 0.1, p_frame_size_kb: 0.1 },
        BenchmarkResult { resolution: "480p".into(), width: 854, height: 480, preset_name: "P7".into(), preset_id: 7, avg_latency_ms: 4.58, min_latency_ms: 4.31, max_fps: 218, idr_size_kb: 0.1, p_frame_size_kb: 0.1 },

        BenchmarkResult { resolution: "720p".into(), width: 1280, height: 720, preset_name: "P1".into(), preset_id: 1, avg_latency_ms: 5.08, min_latency_ms: 4.92, max_fps: 196, idr_size_kb: 0.1, p_frame_size_kb: 0.1 },
        BenchmarkResult { resolution: "720p".into(), width: 1280, height: 720, preset_name: "P3".into(), preset_id: 3, avg_latency_ms: 6.62, min_latency_ms: 6.37, max_fps: 150, idr_size_kb: 0.1, p_frame_size_kb: 0.1 },
        BenchmarkResult { resolution: "720p".into(), width: 1280, height: 720, preset_name: "P4".into(), preset_id: 4, avg_latency_ms: 6.96, min_latency_ms: 6.71, max_fps: 143, idr_size_kb: 0.1, p_frame_size_kb: 0.1 },
        BenchmarkResult { resolution: "720p".into(), width: 1280, height: 720, preset_name: "P5".into(), preset_id: 5, avg_latency_ms: 7.42, min_latency_ms: 7.15, max_fps: 134, idr_size_kb: 0.1, p_frame_size_kb: 0.1 },
        BenchmarkResult { resolution: "720p".into(), width: 1280, height: 720, preset_name: "P6".into(), preset_id: 6, avg_latency_ms: 8.15, min_latency_ms: 7.82, max_fps: 122, idr_size_kb: 0.1, p_frame_size_kb: 0.1 },
        BenchmarkResult { resolution: "720p".into(), width: 1280, height: 720, preset_name: "P7".into(), preset_id: 7, avg_latency_ms: 9.35, min_latency_ms: 8.92, max_fps: 106, idr_size_kb: 0.1, p_frame_size_kb: 0.1 },

        BenchmarkResult { resolution: "1080p".into(), width: 1920, height: 1080, preset_name: "P1".into(), preset_id: 1, avg_latency_ms: 10.90, min_latency_ms: 9.80, max_fps: 91, idr_size_kb: 0.1, p_frame_size_kb: 0.1 },
        BenchmarkResult { resolution: "1080p".into(), width: 1920, height: 1080, preset_name: "P3".into(), preset_id: 3, avg_latency_ms: 13.85, min_latency_ms: 12.90, max_fps: 72, idr_size_kb: 0.1, p_frame_size_kb: 0.1 },
        BenchmarkResult { resolution: "1080p".into(), width: 1920, height: 1080, preset_name: "P4".into(), preset_id: 4, avg_latency_ms: 14.29, min_latency_ms: 13.91, max_fps: 69, idr_size_kb: 0.1, p_frame_size_kb: 0.1 },
        BenchmarkResult { resolution: "1080p".into(), width: 1920, height: 1080, preset_name: "P5".into(), preset_id: 5, avg_latency_ms: 15.30, min_latency_ms: 14.80, max_fps: 65, idr_size_kb: 0.1, p_frame_size_kb: 0.1 },
        BenchmarkResult { resolution: "1080p".into(), width: 1920, height: 1080, preset_name: "P6".into(), preset_id: 6, avg_latency_ms: 17.10, min_latency_ms: 16.50, max_fps: 58, idr_size_kb: 0.1, p_frame_size_kb: 0.1 },
        BenchmarkResult { resolution: "1080p".into(), width: 1920, height: 1080, preset_name: "P7".into(), preset_id: 7, avg_latency_ms: 19.80, min_latency_ms: 19.10, max_fps: 50, idr_size_kb: 0.1, p_frame_size_kb: 0.1 },
    ];
    build_benchmark_matrix(&mock_results, native_w, native_h, native_hz)
}
