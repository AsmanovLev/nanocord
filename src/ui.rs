#![allow(dead_code, unused_variables, unused_imports)]

use crate::audio::{start_wasapi_audio_worker, AudioSpectrumPacket, AUDIO_SPECTROGRAM_BINS};
use crate::benchmark::{build_benchmark_matrix, run_full_benchmark, BenchmarkResult, MatrixCell, MatrixRow};
use crate::camera::{start_camera_worker, CameraFrame};
use crate::capture::{capture_desktop_gdi, get_screen_resolution, start_desktop_preview_worker, PreviewFrame};
use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, Stroke, Vec2};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;

pub const WAVEFORM_SAMPLES: usize = 500; // 500 срезов истории (~10 секунд)

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
            let vol = self.volume.clamp(0.0, 1.5);
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
    MockSource,
}

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Tab {
    CallAndStream,
    BenchmarkMatrix,
    AudioAndSpectrum,
    EncoderSettings,
}

pub struct NanocordApp {
    // Вкладка
    pub active_tab: Tab,

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
    pub is_camera_pip: bool, // Запекание веб-камеры в рабочий стол (Picture-in-Picture)

    // Реальный захват рабочего стола (DXGI + GDI)
    pub desktop_texture: Option<egui::TextureHandle>,
    preview_rx: Option<Receiver<PreviewFrame>>,

    // Реальный захват веб-камеры (Media Foundation / nokhwa)
    pub camera_texture: Option<egui::TextureHandle>,
    camera_rx: Option<Receiver<CameraFrame>>,

    // Реальный захват аудио (Windows WASAPI)
    mic_audio_rx: Option<Receiver<AudioSpectrumPacket>>,
    game_audio_rx: Option<Receiver<AudioSpectrumPacket>>,

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

    // Двухканальная звуковая волна (10 секунд истории)
    pub mic_waveform: AudioWaveform,
    pub game_waveform: AudioWaveform,
    pub anim_time: f32,

    // Сетевой P2P-приёмник от nanocord_mock.exe
    pub p2p_receiver: Option<crate::network::P2PReceiver>,
    pub remote_desktop_texture: Option<egui::TextureHandle>,
    pub remote_camera_texture: Option<egui::TextureHandle>,

    // Встроенный мок-источник (Test Pattern + BGM)
    pub mock_source: Option<crate::network::InternalMockSource>,
    pub is_mock_active: bool,
    pub mock_desktop_texture: Option<egui::TextureHandle>,
    pub mock_camera_texture: Option<egui::TextureHandle>,

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

        // 1. Запуск настоящего фонового захвата экрана в полном нативном разрешении (четкий 1080p без мыла)
        let rx_desktop = start_desktop_preview_worker(native_w, native_h);

        // 2. Запуск настоящего фонового захвата веб-камеры (BisonCam,NB Pro)
        let rx_camera = start_camera_worker();

        // 3. Запуск реального захвата аудио через Windows WASAPI (микрофон и loopback системы)
        let rx_mic = start_wasapi_audio_worker(false);
        let rx_game = start_wasapi_audio_worker(true);

        // 4. Сохранение контрольного скриншота экрана для инспекции
        if let Some(rgba) = capture_desktop_gdi(native_w, native_h) {
            let path = "C:\\Users\\User\\.gemini\\antigravity\\brain\\7c6de5f1-98be-4f2e-bcbc-6dffdeafac75\\screenshot.png";
            let _ = image::save_buffer(path, &rgba, native_w, native_h, image::ColorType::Rgba8);
        }

        Self {
            active_tab: Tab::CallAndStream,
            is_fullscreen: false,
            is_sidebar_collapsed: false,
            fit_aspect_ratio: true,

            active_stage: StageSource::LocalDesktop,
            is_streaming: Arc::new(AtomicBool::new(false)),
            is_mic_muted: false,
            is_deafened: false,
            is_camera_on: true,
            is_camera_pip: false,

            desktop_texture: None,
            preview_rx: Some(rx_desktop),

            camera_texture: None,
            camera_rx: Some(rx_camera),

            mic_audio_rx: Some(rx_mic),
            game_audio_rx: Some(rx_game),

            native_width: native_w,
            native_height: native_h,
            native_hz,
            available_resolutions: available,
            selected_res_idx: default_res_idx,

            target_fps: 60,
            preset_mode: PresetMode::AutoSmart,
            manual_preset: 3,
            cq_quality: 22,
            max_bitrate_mbps: 30,

            mic_waveform: AudioWaveform::new(),
            game_waveform: AudioWaveform::new(),
            anim_time: 0.0,

            p2p_receiver: Some(crate::network::P2PReceiver::start()),
            remote_desktop_texture: None,
            remote_camera_texture: None,

            mock_source: Some(crate::network::InternalMockSource::start()),
            is_mock_active: true,
            mock_desktop_texture: None,
            mock_camera_texture: None,

            benchmark_results: None,
            benchmark_matrix: Some(default_matrix),
            is_benchmarking: false,
            bench_rx: None,
            screenshot_taken: true,
        }
    }
}

/// Чистая системная светлая тема (белая, контрастная, аккуратная)
pub fn apply_custom_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();

    visuals.dark_mode = true;
    visuals.override_text_color = Some(Color32::from_rgb(242, 243, 245));
    visuals.panel_fill = Color32::from_rgb(17, 18, 20); // Discord Deep Dark
    visuals.window_fill = Color32::from_rgb(22, 24, 28);
    visuals.faint_bg_color = Color32::from_rgb(30, 31, 34);
    visuals.extreme_bg_color = Color32::from_rgb(12, 13, 15);

    // Неактивные элементы
    visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(30, 31, 34);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(45, 48, 54));
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(8);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(215, 220, 228));

    // Обычные кнопки
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(43, 45, 49);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(56, 58, 64));
    visuals.widgets.inactive.corner_radius = CornerRadius::same(8);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(242, 243, 245));

    // Кнопка при наведении
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(60, 63, 69);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(88, 101, 242));
    visuals.widgets.hovered.corner_radius = CornerRadius::same(8);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);

    // Активная/нажатая кнопка
    visuals.widgets.active.bg_fill = Color32::from_rgb(88, 101, 242);
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, Color32::from_rgb(71, 82, 196));
    visuals.widgets.active.corner_radius = CornerRadius::same(8);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);

    ctx.set_visuals(visuals);
}

impl NanocordApp {
    /// Отрисовка высококонтрастной бирки навигации (белый читабельный текст по умолчанию, белый фон + темный текст при выборе/наведении)
    fn render_nav_tab_pill(ui: &mut egui::Ui, current: &mut Tab, tab: Tab, label: &str) {
        let is_selected = *current == tab;
        let galley = ui.painter().layout_no_wrap(label.to_string(), egui::FontId::proportional(12.0), Color32::WHITE);
        let pill_w = galley.size().x + 14.0; // аккуратная компактная ширина строго по размеру текста
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(pill_w, 25.0), egui::Sense::click());
        if resp.clicked() {
            *current = tab;
        }
        let is_hovered = resp.hovered();

        let (bg_color, text_color, stroke) = if is_selected {
            // Выбран: белый фон, тёмный читабельный текст
            (Color32::WHITE, Color32::from_rgb(18, 20, 26), Stroke::new(1.0, Color32::WHITE))
        } else if is_hovered {
            // При наведении: белый/светлый фон, тёмный текст
            (Color32::from_rgb(230, 235, 245), Color32::from_rgb(18, 20, 26), Stroke::new(1.0, Color32::from_rgb(230, 235, 245)))
        } else {
            // По умолчанию: тёмная плашка, чистый белый читабельный текст (инвертировано)
            (Color32::from_rgb(45, 48, 56), Color32::WHITE, Stroke::new(1.0, Color32::from_rgb(60, 64, 76)))
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
        // 1. Приём настоящих кадров захвата экрана
        if let Some(ref rx) = self.preview_rx {
            while let Ok(frame) = rx.try_recv() {
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

        // 3. Обновление двухканальной звуковой волны (10 сек буфер) из WASAPI
        self.mic_waveform.is_muted = self.is_mic_muted;
        if let Some(ref rx) = self.mic_audio_rx {
            while let Ok(pkt) = rx.try_recv() {
                self.mic_waveform.push_real_sample(pkt.min_sample, pkt.max_sample, pkt.rms, pkt.peak_db);
            }
        }

        self.game_waveform.is_muted = self.is_deafened;
        if let Some(ref rx) = self.game_audio_rx {
            while let Ok(pkt) = rx.try_recv() {
                self.game_waveform.push_real_sample(pkt.min_sample, pkt.max_sample, pkt.rms, pkt.peak_db);
            }
        }

        // 4. Приём кадров удаленного собеседника из P2P сервера (nanocord_mock.exe)
        if let Some(ref rx) = self.p2p_receiver {
            let mut latest_desktop = None;
            while let Ok(frame) = rx.desktop_rx.try_recv() {
                latest_desktop = Some(frame);
            }
            if let Some(frame) = latest_desktop {
                let color_image = egui::ColorImage::from_rgba_unmultiplied(
                    [frame.width as usize, frame.height as usize],
                    &frame.rgba,
                );
                match &mut self.remote_desktop_texture {
                    Some(tex) => tex.set(color_image, egui::TextureOptions::LINEAR),
                    None => {
                        self.remote_desktop_texture = Some(ctx.load_texture(
                            "remote_desktop_preview",
                            color_image,
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                }
            }

            let mut latest_camera = None;
            while let Ok(frame) = rx.camera_rx.try_recv() {
                latest_camera = Some(frame);
            }
            if let Some(frame) = latest_camera {
                let color_image = egui::ColorImage::from_rgba_unmultiplied(
                    [frame.width as usize, frame.height as usize],
                    &frame.rgba,
                );
                match &mut self.remote_camera_texture {
                    Some(tex) => tex.set(color_image, egui::TextureOptions::LINEAR),
                    None => {
                        self.remote_camera_texture = Some(ctx.load_texture(
                            "remote_camera_preview",
                            color_image,
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                }
            }
        }

        // 5. Приём кадров от встроенного мок-источника
        if let Some(ref mock) = self.mock_source {
            let mut latest_mock_dt = None;
            while let Ok(frame) = mock.desktop_rx.try_recv() {
                latest_mock_dt = Some(frame);
            }
            if let Some(frame) = latest_mock_dt {
                let color_image = egui::ColorImage::from_rgba_unmultiplied(
                    [frame.width as usize, frame.height as usize],
                    &frame.rgba,
                );
                match &mut self.mock_desktop_texture {
                    Some(tex) => tex.set(color_image, egui::TextureOptions::LINEAR),
                    None => {
                        self.mock_desktop_texture = Some(ctx.load_texture(
                            "mock_desktop_live",
                            color_image,
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                }
            }

            let mut latest_mock_cam = None;
            while let Ok(frame) = mock.camera_rx.try_recv() {
                latest_mock_cam = Some(frame);
            }
            if let Some(frame) = latest_mock_cam {
                let color_image = egui::ColorImage::from_rgba_unmultiplied(
                    [frame.width as usize, frame.height as usize],
                    &frame.rgba,
                );
                match &mut self.mock_camera_texture {
                    Some(tex) => tex.set(color_image, egui::TextureOptions::LINEAR),
                    None => {
                        self.mock_camera_texture = Some(ctx.load_texture(
                            "mock_camera_live",
                            color_image,
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                }
            }
        }

        // 6. Приём результатов бенчмарка
        if let Some(rx) = &self.bench_rx {
            if let Ok(results) = rx.try_recv() {
                let matrix = build_benchmark_matrix(&results, self.native_width, self.native_height, self.native_hz);
                self.benchmark_matrix = Some(matrix);
                self.benchmark_results = Some(results);
                self.is_benchmarking = false;
                self.bench_rx = None;
            }
        }

        // 5. Клавиши управления (Esc, F11, F12)
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && self.is_fullscreen {
            self.is_fullscreen = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F11)) {
            self.is_fullscreen = !self.is_fullscreen;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.is_fullscreen));
        }

        ctx.request_repaint_after(std::time::Duration::from_millis(16));

        // ЕСЛИ ВКЛЮЧЕН ПОЛНОЭКРАННЫЙ РЕЖИМ (FULLSCREEN)
        if self.is_fullscreen {
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(Color32::from_rgb(12, 14, 18)).inner_margin(0.0))
                .show(ctx, |ui| {
                    self.render_fullscreen_stage(ui);
                });
            return;
        }

        // ОБЫЧНЫЙ РЕЖИМ: ВЕРХНЯЯ НАВИГАЦИОННАЯ ПАНЕЛЬ
        egui::TopBottomPanel::top("top_nav")
            .frame(egui::Frame::new().fill(Color32::from_rgb(30, 31, 34)).stroke(Stroke::new(1.0, Color32::from_rgb(45, 48, 54))).inner_margin(egui::Margin::symmetric(14, 8)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("NANOCORD").strong().size(18.0).color(Color32::from_rgb(88, 101, 242)));
                    ui.label(egui::RichText::new("v0.1.0 • P2P Ultra-Low Latency").weak().size(12.0));

                    ui.add_space(20.0);

                    // Высококонтрастные бирки вкладок (белый текст по умолчанию, инвертированный белый фон при выборе/наведении)
                    Self::render_nav_tab_pill(ui, &mut self.active_tab, Tab::CallAndStream, "Звонок и Стрим");
                    Self::render_nav_tab_pill(ui, &mut self.active_tab, Tab::BenchmarkMatrix, "Матрица профилей");
                    Self::render_nav_tab_pill(ui, &mut self.active_tab, Tab::AudioAndSpectrum, "Аудио и Спектрограмма");
                    Self::render_nav_tab_pill(ui, &mut self.active_tab, Tab::EncoderSettings, "Настройки NVENC");

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.is_streaming.load(Ordering::Relaxed) {
                            ui.label(egui::RichText::new("В ЭФИРЕ (60 FPS)").color(Color32::from_rgb(220, 38, 38)).strong());
                        } else if self.is_benchmarking {
                            ui.label(egui::RichText::new("ТЕСТ КРЕМНИЯ...").color(Color32::from_rgb(217, 119, 6)).strong());
                        } else {
                            ui.label(egui::RichText::new("ГОТОВ К ЗВОНКУ").color(Color32::from_rgb(22, 163, 74)).strong());
                        }
                    });
                });
            });

        // ЦЕНТРАЛЬНАЯ РАБОЧАЯ ОБЛАСТЬ
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Color32::from_rgb(17, 18, 20)).inner_margin(8.0))
            .show(ctx, |ui| {
                match self.active_tab {
                    Tab::CallAndStream => self.render_call_and_stream(ui),
                    Tab::BenchmarkMatrix => self.render_benchmark_matrix(ui),
                    Tab::AudioAndSpectrum => self.render_audio_and_spectrum(ui),
                    Tab::EncoderSettings => self.render_encoder_settings(ui),
                }
            });
    }
}

impl NanocordApp {
    /// Отрисовка видео-экрана во весь размер окна (Fullscreen) с идеальным соотношением сторон
    fn render_fullscreen_stage(&mut self, ui: &mut egui::Ui) {
        let full_rect = ui.available_rect_before_wrap();

        // Фон видео-экрана и настоящий видеопоток
        {
            let painter = ui.painter();
            painter.rect_filled(full_rect, CornerRadius::ZERO, Color32::from_rgb(12, 14, 18));
            self.render_stream_surface(painter, full_rect, self.active_stage);
        }

        // Плавающая верхняя плашка управления поверх видео
        let overlay_rect = Rect::from_min_size(
            Pos2::new(full_rect.left() + 20.0, full_rect.top() + 16.0),
            Vec2::new(full_rect.width() - 40.0, 44.0),
        );

        ui.painter().rect_filled(overlay_rect, CornerRadius::same(8), Color32::from_rgba_unmultiplied(20, 24, 32, 210));
        ui.painter().rect_stroke(overlay_rect, CornerRadius::same(8), Stroke::new(1.0, Color32::from_rgb(60, 68, 86)), egui::StrokeKind::Inside);

        let mut overlay_ui = ui.new_child(egui::UiBuilder::new().max_rect(overlay_rect));
        overlay_ui.horizontal(|ui| {
            ui.add_space(12.0);
            let stage_name = match self.active_stage {
                StageSource::RemoteDesktop => "Рабочий стол собеседника",
                StageSource::RemoteCamera => "Камера собеседника",
                StageSource::LocalDesktop => "Ваш рабочий стол",
                StageSource::LocalCamera => "Ваша веб-камера",
                StageSource::MockSource => "Мок-источник (60 FPS Test Pattern + BGM)",
            };
            ui.label(egui::RichText::new(stage_name).color(Color32::WHITE).strong().size(14.0));

            let (w, h, res_name) = self.available_resolutions[self.selected_res_idx];
            let (_, p_name) = self.get_effective_preset();
            ui.label(egui::RichText::new(format!("{} • {} FPS • HEVC {} • {:.1} ms", res_name, self.target_fps, p_name, self.estimate_latency_ms()))
                .color(Color32::from_rgb(130, 180, 255))
                .size(12.0));

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);
                if ui.button("Выйти из полноэкранного режима (Esc)").clicked() {
                    self.is_fullscreen = false;
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                }

                let aspect_btn = if self.fit_aspect_ratio { "Масштаб: 16:9" } else { "Масштаб: Заполнить" };
                if ui.button(aspect_btn).clicked() {
                    self.fit_aspect_ratio = !self.fit_aspect_ratio;
                }

                let mic_text = if self.is_mic_muted { "Включить микрофон" } else { "Выключить микрофон" };
                if ui.button(mic_text).clicked() {
                    self.is_mic_muted = !self.is_mic_muted;
                }

                let deaf_text = if self.is_deafened { "Включить звук" } else { "Выключить звук" };
                if ui.button(deaf_text).clicked() {
                    self.is_deafened = !self.is_deafened;
                }
            });
        });
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

        // ==========================================
        // 1. ОСНОВНАЯ СЦЕНА (STAGE VIEW)
        // ==========================================
        painter.rect_filled(stage_container_rect, CornerRadius::same(10), Color32::from_rgb(18, 20, 24));
        painter.rect_stroke(stage_container_rect, CornerRadius::same(10), Stroke::new(1.0, Color32::from_rgb(38, 41, 48)), egui::StrokeKind::Inside);

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

        // --- ОВЕРЛЕЙ-БЕЙДЖИ ПРЯМО ПОВЕРХ ВИДЕОПОТОКА (КАК В TWITCH / DISCORD) ---
        // Верхний левый угол: Статус стрима и битрейт/задержка
        let (w, h, res_name) = self.available_resolutions[self.selected_res_idx];
        let (_, p_name) = self.get_effective_preset();
        let status_str = if self.is_streaming.load(Ordering::Relaxed) {
            format!("🔴 LIVE • {} • {} FPS • HEVC {} • {:.1} ms", res_name, self.target_fps, p_name, self.estimate_latency_ms())
        } else {
            format!("🟢 ГОТОВ К ЭФИРУ • {} • {} FPS • HEVC {} • {:.1} ms", res_name, self.target_fps, p_name, self.estimate_latency_ms())
        };
        let status_w = (status_str.len() as f32 * 7.5 + 24.0).clamp(240.0, 360.0);
        let badge_top_left = Rect::from_min_size(
            Pos2::new(video_rect.left() + 14.0, video_rect.top() + 14.0),
            Vec2::new(status_w, 28.0),
        );
        painter.rect_filled(badge_top_left, CornerRadius::same(6), Color32::from_rgba_unmultiplied(12, 14, 18, 225));
        painter.rect_stroke(badge_top_left, CornerRadius::same(6), Stroke::new(1.0, Color32::from_rgb(52, 58, 70)), egui::StrokeKind::Inside);
        painter.text(badge_top_left.center(), egui::Align2::CENTER_CENTER, status_str, egui::FontId::proportional(11.0), Color32::WHITE);

        // Верхний правый угол: Кнопки управления сценой (PiP, Масштаб, Fullscreen)
        let ctrl_w = 330.0;
        let overlay_ctrl_rect = Rect::from_min_size(
            Pos2::new(video_rect.right() - ctrl_w - 14.0, video_rect.top() + 14.0),
            Vec2::new(ctrl_w, 30.0),
        );
        let mut ctrl_ui = ui.new_child(egui::UiBuilder::new().max_rect(overlay_ctrl_rect));
        ctrl_ui.horizontal_centered(|ui| {
            let pip_btn = if self.is_camera_pip { "PiP Камера: ВКЛ" } else { "PiP Камера: ВЫКЛ" };
            let pip_color = if self.is_camera_pip { Color32::from_rgb(35, 165, 90) } else { Color32::from_rgb(180, 185, 195) };
            if ui.add(egui::Button::new(egui::RichText::new(pip_btn).strong().size(11.0).color(pip_color))
                .fill(Color32::from_rgba_unmultiplied(20, 24, 32, 210))).clicked()
            {
                self.is_camera_pip = !self.is_camera_pip;
            }

            let aspect_btn = if self.fit_aspect_ratio { "16:9" } else { "Заполнить" };
            if ui.add(egui::Button::new(egui::RichText::new(aspect_btn).size(11.0))
                .fill(Color32::from_rgba_unmultiplied(20, 24, 32, 210))).clicked()
            {
                self.fit_aspect_ratio = !self.fit_aspect_ratio;
            }

            if ui.add(egui::Button::new(egui::RichText::new("Во весь экран (F11)").strong().size(11.0).color(Color32::WHITE))
                .fill(Color32::from_rgba_unmultiplied(40, 48, 64, 230))).clicked()
            {
                self.is_fullscreen = true;
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
            }
        });

        // Нижний левый угол: Название активного источника
        let stage_title = match self.active_stage {
            StageSource::RemoteDesktop => "Рабочий стол собеседника (Ожидание потока)",
            StageSource::RemoteCamera => "Камера собеседника (Выключена)",
            StageSource::LocalDesktop => "Ваш рабочий стол (DirectX 11 Live Capture)",
            StageSource::LocalCamera => "Ваша веб-камера (BisonCam,NB Pro Live)",
            StageSource::MockSource => "Мок-источник (60 FPS Test Pattern • BGM Direct)",
        };
        let src_badge_rect = Rect::from_min_size(
            Pos2::new(video_rect.left() + 14.0, video_rect.bottom() - 36.0),
            Vec2::new(320.0, 26.0),
        );
        painter.rect_filled(src_badge_rect, CornerRadius::same(5), Color32::from_rgba_unmultiplied(12, 14, 18, 220));
        painter.text(src_badge_rect.center(), egui::Align2::CENTER_CENTER, stage_title, egui::FontId::proportional(11.0), Color32::from_rgb(220, 225, 235));

        // ==========================================
        // 2. САЙДБАР УЧАСТНИКОВ И ПОТОКОВ
        // ==========================================
        if !self.is_sidebar_collapsed {
            painter.rect_filled(sidebar_rect, CornerRadius::same(10), Color32::from_rgb(26, 28, 32));
            painter.rect_stroke(sidebar_rect, CornerRadius::same(10), Stroke::new(1.0, Color32::from_rgb(45, 48, 56)), egui::StrokeKind::Inside);

            let mut sb_ui = ui.new_child(egui::UiBuilder::new().max_rect(sidebar_rect.shrink(10.0)));
            sb_ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("УЧАСТНИКИ И ПОТОКИ (5)").strong().size(11.5).color(Color32::from_rgb(148, 155, 164)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new("Комната: NC-7829-X").size(10.5).weak());
                    });
                });
                ui.add_space(8.0);

                let scroll_h = ui.available_height();
                egui::ScrollArea::vertical()
                    .max_height(scroll_h)
                    .show(ui, |ui| {
                        let mock_sub = if self.is_mock_active { "60 FPS • BGM Активен" } else { "60 FPS • Синтетический" };
                        self.render_feed_card(ui, "Мок-источник (P2P Test)", mock_sub, StageSource::MockSource);
                        ui.add_space(8.0);
                        self.render_feed_card(ui, "Экран собеседника", "1080p • Ожидание", StageSource::RemoteDesktop);
                        ui.add_space(8.0);
                        self.render_feed_card(ui, "Камера собеседника", "720p • Выключена", StageSource::RemoteCamera);
                        ui.add_space(8.0);
                        self.render_feed_card(ui, "Ваш рабочий стол", "Live DXGI / GDI", StageSource::LocalDesktop);
                        ui.add_space(8.0);
                        self.render_feed_card(ui, "Ваша веб-камера", "BisonCam • 720p 30 FPS", StageSource::LocalCamera);
                    });
            });
        }

        // ==========================================
        // 3. ПЛАВАЮЩИЙ НИЖНИЙ DOCK УПРАВЛЕНИЯ ЗВОНКОМ (MATERIAL DESIGN ICON STYLE)
        // ==========================================
        let dock_center = dock_rect.center();
        let dock_pill_w = (490.0_f32).min(total_w - 20.0);
        let dock_pill_rect = Rect::from_center_size(dock_center, Vec2::new(dock_pill_w, 52.0));

        painter.rect_filled(dock_pill_rect, CornerRadius::same(26), Color32::from_rgb(30, 31, 36));
        painter.rect_stroke(dock_pill_rect, CornerRadius::same(26), Stroke::new(1.0, Color32::from_rgb(50, 54, 64)), egui::StrokeKind::Inside);

        let mut dock_ui = ui.new_child(egui::UiBuilder::new().max_rect(dock_pill_rect.shrink2(Vec2::new(12.0, 4.0))));
        dock_ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 0.0);

            // 1. Иконка Микрофона (MD круг 42px с динамическим жидким пузырьком громкости)
            let (mic_rect, mic_resp) = ui.allocate_exact_size(Vec2::new(42.0, 42.0), egui::Sense::click());
            let mic_center = mic_rect.center();
            let mic_radius = 21.0;
            let mic_painter = ui.painter();

            if self.is_mic_muted {
                // Выключен: красный круг
                mic_painter.circle_filled(mic_center, mic_radius, Color32::from_rgb(218, 55, 60));
                mic_painter.circle_stroke(mic_center, mic_radius, Stroke::new(1.0, Color32::from_rgb(240, 80, 85)));
                mic_painter.text(mic_center, egui::Align2::CENTER_CENTER, "🔇", egui::FontId::proportional(16.0), Color32::WHITE);
            } else {
                // По умолчанию: серый круг в цвет остальных кнопок дока
                mic_painter.circle_filled(mic_center, mic_radius, Color32::from_rgb(45, 48, 56));
                mic_painter.circle_stroke(mic_center, mic_radius, Stroke::new(1.0, Color32::from_rgb(60, 64, 76)));

                // Жидкий зелёный пузырек, поднимающийся снизу вверх от уровня громкости
                let vol_norm = (self.mic_waveform.smoothed_rms * 3.0).clamp(0.0, 1.0);
                if vol_norm > 0.02 {
                    let fill_h = mic_rect.height() * vol_norm;
                    let fill_clip = Rect::from_min_max(
                        Pos2::new(mic_rect.left(), mic_rect.bottom() - fill_h),
                        mic_rect.max,
                    );
                    mic_painter.with_clip_rect(fill_clip).circle_filled(mic_center, mic_radius, Color32::from_rgb(35, 165, 90));

                    // Светящийся гребень пузырька на границе уровня
                    let crest_y = mic_rect.bottom() - fill_h;
                    mic_painter.line_segment(
                        [Pos2::new(mic_rect.left() + 4.0, crest_y), Pos2::new(mic_rect.right() - 4.0, crest_y)],
                        Stroke::new(1.5, Color32::from_rgb(95, 235, 150)),
                    );
                }

                if mic_resp.hovered() {
                    mic_painter.circle_stroke(mic_center, mic_radius, Stroke::new(1.5, Color32::from_rgb(88, 101, 242)));
                }

                mic_painter.text(mic_center, egui::Align2::CENTER_CENTER, "🎤", egui::FontId::proportional(16.0), Color32::WHITE);
            }

            if mic_resp.clicked() {
                self.is_mic_muted = !self.is_mic_muted;
            }
            mic_resp.on_hover_text(if self.is_mic_muted { "Включить микрофон (M)" } else { "Выключить микрофон (M)" });

            // 2. Иконка Звука системы (Deafen)
            let deaf_icon = if self.is_deafened { "🔈" } else { "🎧" };
            let deaf_bg = if self.is_deafened { Color32::from_rgb(218, 55, 60) } else { Color32::from_rgb(45, 48, 56) };
            if ui.add(egui::Button::new(egui::RichText::new(deaf_icon).size(16.0).color(Color32::WHITE))
                .fill(deaf_bg)
                .min_size(Vec2::new(42.0, 42.0))
                .corner_radius(CornerRadius::same(21)))
                .on_hover_text(if self.is_deafened { "Включить звук системы" } else { "Заглушить звук системы" })
                .clicked()
            {
                self.is_deafened = !self.is_deafened;
            }

            // 3. Иконка Веб-камеры
            let cam_icon = if self.is_camera_on { "📹" } else { "📷" };
            let cam_bg = if self.is_camera_on { Color32::from_rgb(45, 48, 56) } else { Color32::from_rgb(218, 55, 60) };
            if ui.add(egui::Button::new(egui::RichText::new(cam_icon).size(16.0).color(Color32::WHITE))
                .fill(cam_bg)
                .min_size(Vec2::new(42.0, 42.0))
                .corner_radius(CornerRadius::same(21)))
                .on_hover_text(if self.is_camera_on { "Выключить веб-камеру" } else { "Включить веб-камеру" })
                .clicked()
            {
                self.is_camera_on = !self.is_camera_on;
            }

            // 4. Иконка Стрима экрана
            let is_stream = self.is_streaming.load(Ordering::Relaxed);
            let stream_bg = if is_stream { Color32::from_rgb(218, 55, 60) } else { Color32::from_rgb(88, 101, 242) };
            if ui.add(egui::Button::new(egui::RichText::new("🖥").size(16.0).color(Color32::WHITE))
                .fill(stream_bg)
                .min_size(Vec2::new(42.0, 42.0))
                .corner_radius(CornerRadius::same(21)))
                .on_hover_text(if is_stream { "Остановить стрим экрана" } else { "Запустить стрим экрана" })
                .clicked()
            {
                self.is_streaming.store(!is_stream, Ordering::Relaxed);
            }

            // 5. Иконка PiP (Камера в стриме)
            let pip_bg = if self.is_camera_pip { Color32::from_rgb(35, 165, 90) } else { Color32::from_rgb(45, 48, 56) };
            if ui.add(egui::Button::new(egui::RichText::new("🗖").size(16.0).color(Color32::WHITE))
                .fill(pip_bg)
                .min_size(Vec2::new(42.0, 42.0))
                .corner_radius(CornerRadius::same(21)))
                .on_hover_text(if self.is_camera_pip { "Выключить PiP камеру" } else { "Включить PiP камеру в стриме" })
                .clicked()
            {
                self.is_camera_pip = !self.is_camera_pip;
            }

            // 6. Иконка Звуковой волны (10 сек)
            if ui.add(egui::Button::new(egui::RichText::new("〰").size(16.0).color(Color32::WHITE))
                .fill(Color32::from_rgb(45, 48, 56))
                .min_size(Vec2::new(42.0, 42.0))
                .corner_radius(CornerRadius::same(21)))
                .on_hover_text("Открыть звуковую волну (10 сек буфер на черном фоне)")
                .clicked()
            {
                self.active_tab = Tab::AudioAndSpectrum;
            }

            // 7. Иконка P2P Сети и встроенного Мок-источника
            let is_peer_connected = self.p2p_receiver.as_ref().map(|r| r.is_connected.load(Ordering::Relaxed)).unwrap_or(false);
            let net_bg = if is_peer_connected || self.is_mock_active { Color32::from_rgb(35, 165, 90) } else { Color32::from_rgb(45, 48, 56) };
            let telem_info = self.p2p_receiver.as_ref().map(|r| r.telemetry.read().unwrap().clone()).unwrap_or_default();
            let hover_tip = if self.is_mock_active {
                let track = self.mock_source.as_ref().map(|m| m.bgm_player.lock().unwrap().current_track_name()).unwrap_or_default();
                format!("Встроенный Мок-источник активен!\n60 FPS • Без сокетов и сетевых лагов\nТрек: {}\nНажмите для включения/выключения", track)
            } else if is_peer_connected {
                format!("Mock-собеседник подключен!\nFPS: {:.1} • Скорость: {:.1} Mbps • Пинг: {:.1} ms\nТрек: {}\nНажмите для перехода на удаленный экран", telem_info.fps, telem_info.bitrate_mbps, telem_info.latency_ms, telem_info.current_track)
            } else {
                "Мок-источник (Тестовый генератор 60 FPS + BGM)\nНажмите для мгновенного запуска встроенного мока\nИли запустите nanocord_mock.exe".to_string()
            };

            if ui.add(egui::Button::new(egui::RichText::new("📡").size(16.0).color(Color32::WHITE))
                .fill(net_bg)
                .min_size(Vec2::new(42.0, 42.0))
                .corner_radius(CornerRadius::same(21)))
                .on_hover_text(hover_tip)
                .clicked()
            {
                if self.is_mock_active {
                    if let Some(mock) = self.mock_source.take() {
                        mock.stop();
                    }
                    self.is_mock_active = false;
                    if self.active_stage == StageSource::MockSource {
                        self.active_stage = StageSource::LocalDesktop;
                    }
                } else {
                    self.mock_source = Some(crate::network::InternalMockSource::start());
                    self.is_mock_active = true;
                    self.active_stage = StageSource::MockSource;
                }
            }

            // 8. Иконка Отключения (красная телефонная трубка)
            if ui.add(egui::Button::new(egui::RichText::new("📞").size(16.0).color(Color32::WHITE))
                .fill(Color32::from_rgb(218, 55, 60))
                .min_size(Vec2::new(42.0, 42.0))
                .corner_radius(CornerRadius::same(21)))
                .on_hover_text("Завершить звонок")
                .clicked()
            {
                self.is_streaming.store(false, Ordering::Relaxed);
                self.active_stage = StageSource::LocalDesktop;
            }
        });
    }

    fn render_stream_surface(&self, painter: &egui::Painter, rect: Rect, source: StageSource) {
        let is_thumb = rect.height() < 220.0;

        match source {
            StageSource::LocalDesktop => {
                if let Some(tex) = self.desktop_texture.as_ref() {
                    let draw_rect = if self.fit_aspect_ratio {
                        let [tw, th] = tex.size();
                        aspect_fit(rect, tw as f32, th as f32)
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

                    // ЗАПЕКАНИЕ ВЕБ-КАМЕРЫ В РАБОЧИЙ СТОЛ (Picture-in-Picture)
                    if self.is_camera_pip && !is_thumb {
                        if let Some(cam_tex) = self.camera_texture.as_ref() {
                            let pip_w = (draw_rect.width() * 0.22).clamp(120.0, 260.0);
                            let pip_h = pip_w * (9.0 / 16.0);
                            let pip_rect = Rect::from_min_size(
                                Pos2::new(draw_rect.right() - pip_w - 12.0, draw_rect.bottom() - pip_h - 12.0),
                                Vec2::new(pip_w, pip_h),
                            );
                            painter.rect_filled(pip_rect, CornerRadius::same(6), Color32::from_rgb(10, 12, 16));
                            painter.rect_stroke(pip_rect, CornerRadius::same(6), Stroke::new(1.5, Color32::from_rgb(88, 101, 242)), egui::StrokeKind::Inside);
                            painter.image(
                                cam_tex.id(),
                                pip_rect,
                                Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                                Color32::WHITE,
                            );

                            let badge_rect = Rect::from_min_size(
                                Pos2::new(pip_rect.left() + 6.0, pip_rect.top() + 6.0),
                                Vec2::new(42.0, 16.0),
                            );
                            painter.rect_filled(badge_rect, CornerRadius::same(3), Color32::from_rgba_unmultiplied(0, 0, 0, 180));
                            painter.text(badge_rect.center(), egui::Align2::CENTER_CENTER, "PiP CAM", egui::FontId::proportional(8.5), Color32::WHITE);
                        }
                    }
                } else {
                    painter.rect_filled(rect, CornerRadius::same(6), Color32::from_rgb(18, 22, 30));
                    if !is_thumb {
                        painter.text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "Захват экрана (Подключение DXGI / GDI)...",
                            egui::FontId::proportional(14.0),
                            Color32::from_rgb(180, 190, 205),
                        );
                    }
                }
            }
            StageSource::LocalCamera => {
                if let Some(tex) = self.camera_texture.as_ref() {
                    let draw_rect = if self.fit_aspect_ratio {
                        let [tw, th] = tex.size();
                        aspect_fit(rect, tw as f32, th as f32)
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
                    painter.rect_filled(rect, CornerRadius::same(6), Color32::from_rgb(20, 22, 28));
                    let center = rect.center();
                    let avatar_c = Pos2::new(center.x, center.y - 12.0);
                    painter.circle_filled(avatar_c, 20.0, Color32::from_rgb(38, 42, 54));
                    painter.circle_stroke(avatar_c, 20.0, Stroke::new(1.5, Color32::from_rgb(90, 105, 138)));
                    painter.text(avatar_c, egui::Align2::CENTER_CENTER, "📹", egui::FontId::proportional(13.0), Color32::WHITE);
                    painter.text(
                        Pos2::new(center.x, center.y + 16.0),
                        egui::Align2::CENTER_CENTER,
                        if self.is_camera_on { "BisonCam: Загрузка..." } else { "Камера выключена" },
                        egui::FontId::proportional(11.0),
                        Color32::from_rgb(150, 160, 180),
                    );
                }
            }
            StageSource::RemoteDesktop => {
                if let Some(tex) = self.remote_desktop_texture.as_ref() {
                    let draw_rect = if self.fit_aspect_ratio {
                        let [tw, th] = tex.size();
                        aspect_fit(rect, tw as f32, th as f32)
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
                    painter.rect_filled(rect, CornerRadius::same(6), Color32::from_rgb(18, 20, 26));
                if is_thumb {
                    let center = rect.center();
                    let avatar_c = Pos2::new(center.x, center.y - 12.0);
                    painter.circle_filled(avatar_c, 20.0, Color32::from_rgb(38, 42, 54));
                    painter.circle_stroke(avatar_c, 20.0, Stroke::new(1.5, Color32::from_rgb(88, 101, 242)));
                    painter.text(avatar_c, egui::Align2::CENTER_CENTER, "NC", egui::FontId::proportional(12.0), Color32::WHITE);
                    painter.text(
                        Pos2::new(center.x, center.y + 16.0),
                        egui::Align2::CENTER_CENTER,
                        "Ожидание стрима",
                        egui::FontId::proportional(11.0),
                        Color32::from_rgb(150, 160, 180),
                    );
                } else {
                    let center = rect.center();
                    // Стилизованный контур монитора
                    let mon_w = 90.0;
                    let mon_h = 56.0;
                    let mon_rect = Rect::from_center_size(Pos2::new(center.x, center.y - 45.0), Vec2::new(mon_w, mon_h));
                    painter.rect_stroke(mon_rect, CornerRadius::same(5), Stroke::new(2.0, Color32::from_rgb(70, 85, 115)), egui::StrokeKind::Inside);
                    painter.line_segment(
                        [Pos2::new(center.x, mon_rect.bottom()), Pos2::new(center.x, mon_rect.bottom() + 10.0)],
                        Stroke::new(2.0, Color32::from_rgb(70, 85, 115)),
                    );
                    painter.line_segment(
                        [Pos2::new(center.x - 20.0, mon_rect.bottom() + 10.0), Pos2::new(center.x + 20.0, mon_rect.bottom() + 10.0)],
                        Stroke::new(2.0, Color32::from_rgb(70, 85, 115)),
                    );

                    painter.text(
                        Pos2::new(center.x, center.y + 20.0),
                        egui::Align2::CENTER_CENTER,
                        "Собеседник ещё не запустил трансляцию экрана",
                        egui::FontId::proportional(15.0),
                        Color32::from_rgb(225, 230, 242),
                    );
                    painter.text(
                        Pos2::new(center.x, center.y + 44.0),
                        egui::Align2::CENTER_CENTER,
                        "Ожидание входящего P2P видеопотока (Комната: NC-7829-X)",
                        egui::FontId::proportional(12.0),
                        Color32::from_rgb(130, 142, 165),
                    );
                }
                }
            }
            StageSource::RemoteCamera => {
                if let Some(tex) = self.remote_camera_texture.as_ref() {
                    let draw_rect = if self.fit_aspect_ratio {
                        let [tw, th] = tex.size();
                        aspect_fit(rect, tw as f32, th as f32)
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
                    painter.rect_filled(rect, CornerRadius::same(6), Color32::from_rgb(20, 22, 28));
                if is_thumb {
                    let center = rect.center();
                    let avatar_c = Pos2::new(center.x, center.y - 12.0);
                    painter.circle_filled(avatar_c, 20.0, Color32::from_rgb(38, 42, 54));
                    painter.circle_stroke(avatar_c, 20.0, Stroke::new(1.5, Color32::from_rgb(90, 105, 138)));
                    painter.text(avatar_c, egui::Align2::CENTER_CENTER, "👤", egui::FontId::proportional(13.0), Color32::WHITE);
                    painter.text(
                        Pos2::new(center.x, center.y + 16.0),
                        egui::Align2::CENTER_CENTER,
                        "Камера выключена",
                        egui::FontId::proportional(11.0),
                        Color32::from_rgb(150, 160, 180),
                    );
                } else {
                    let center = rect.center();
                    let avatar_center = Pos2::new(center.x, center.y - 45.0);
                    painter.circle_filled(avatar_center, 34.0, Color32::from_rgb(32, 38, 54));
                    painter.circle_stroke(avatar_center, 34.0, Stroke::new(1.5, Color32::from_rgb(65, 78, 105)));

                    // Силуэт головы и плеч
                    painter.circle_filled(Pos2::new(avatar_center.x, avatar_center.y - 8.0), 12.0, Color32::from_rgb(90, 105, 138));
                    let shoulder_rect = Rect::from_center_size(Pos2::new(avatar_center.x, avatar_center.y + 16.0), Vec2::new(32.0, 16.0));
                    painter.rect_filled(shoulder_rect, CornerRadius::same(8), Color32::from_rgb(90, 105, 138));

                    painter.text(
                        Pos2::new(center.x, center.y + 20.0),
                        egui::Align2::CENTER_CENTER,
                        "Камера собеседника выключена",
                        egui::FontId::proportional(15.0),
                        Color32::from_rgb(225, 230, 242),
                    );
                    painter.text(
                        Pos2::new(center.x, center.y + 44.0),
                        egui::Align2::CENTER_CENTER,
                        "Ожидание видеосигнала участника звонка",
                        egui::FontId::proportional(12.0),
                        Color32::from_rgb(130, 142, 165),
                    );
                }
                }
            }
            StageSource::MockSource => {
                if let Some(tex) = self.mock_desktop_texture.as_ref() {
                    let draw_rect = if self.fit_aspect_ratio {
                        let [tw, th] = tex.size();
                        aspect_fit(rect, tw as f32, th as f32)
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

                    // Picture-in-Picture для камеры мока
                    if self.is_camera_pip && !is_thumb {
                        if let Some(cam_tex) = self.mock_camera_texture.as_ref() {
                            let pip_w = (draw_rect.width() * 0.22).clamp(120.0, 260.0);
                            let pip_h = pip_w * (9.0 / 16.0);
                            let pip_rect = Rect::from_min_size(
                                Pos2::new(draw_rect.right() - pip_w - 12.0, draw_rect.bottom() - pip_h - 12.0),
                                Vec2::new(pip_w, pip_h),
                            );
                            painter.rect_filled(pip_rect, CornerRadius::same(6), Color32::from_rgb(10, 12, 16));
                            painter.rect_stroke(pip_rect, CornerRadius::same(6), Stroke::new(1.5, Color32::from_rgb(88, 101, 242)), egui::StrokeKind::Inside);
                            painter.image(
                                cam_tex.id(),
                                pip_rect,
                                Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                                Color32::WHITE,
                            );

                            let badge_rect = Rect::from_min_size(
                                Pos2::new(pip_rect.left() + 6.0, pip_rect.top() + 6.0),
                                Vec2::new(60.0, 16.0),
                            );
                            painter.rect_filled(badge_rect, CornerRadius::same(3), Color32::from_rgba_unmultiplied(0, 0, 0, 180));
                            painter.text(badge_rect.center(), egui::Align2::CENTER_CENTER, "MOCK CAM", egui::FontId::proportional(8.5), Color32::WHITE);
                        }
                    }
                } else {
                    painter.rect_filled(rect, CornerRadius::same(6), Color32::from_rgb(16, 20, 28));
                    let center = rect.center();
                    let mon_w = 90.0;
                    let mon_h = 56.0;
                    let mon_rect = Rect::from_center_size(Pos2::new(center.x, center.y - (if is_thumb { 0.0 } else { 35.0 })), Vec2::new(mon_w, mon_h));
                    if !is_thumb {
                        painter.rect_stroke(mon_rect, CornerRadius::same(5), Stroke::new(2.0, Color32::from_rgb(88, 101, 242)), egui::StrokeKind::Inside);
                        painter.text(
                            Pos2::new(center.x, center.y + 20.0),
                            egui::Align2::CENTER_CENTER,
                            "Встроенный мок-источник 60 FPS выключен",
                            egui::FontId::proportional(15.0),
                            Color32::from_rgb(225, 230, 242),
                        );
                        painter.text(
                            Pos2::new(center.x, center.y + 44.0),
                            egui::Align2::CENTER_CENTER,
                            "Нажмите кнопку 📡 в нижнем доке для мгновенного запуска",
                            egui::FontId::proportional(12.0),
                            Color32::from_rgb(130, 142, 165),
                        );
                    } else {
                        painter.text(
                            center,
                            egui::Align2::CENTER_CENTER,
                            "Мок 60 FPS",
                            egui::FontId::proportional(11.0),
                            Color32::from_rgb(150, 160, 180),
                        );
                    }
                }
            }
        }
    }

    /// Полноразмерный прямоугольный видео-тайл участника с кнопками при наведении (hover controls)
    fn render_feed_card(&mut self, ui: &mut egui::Ui, title: &str, subtitle: &str, source: StageSource) {
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
            Stroke::new(1.0, Color32::from_rgb(45, 48, 56))
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
        painter.text(fs_icon_rect.center(), egui::Align2::CENTER_CENTER, "⛶", egui::FontId::proportional(12.0), Color32::WHITE);

        // Двойной клик -> Во весь экран! Одиночный клик -> Выбор источника на сцену
        if response.double_clicked() || (response.clicked() && fs_icon_hovered) {
            if source == StageSource::MockSource && self.mock_source.is_none() {
                self.mock_source = Some(crate::network::InternalMockSource::start());
                self.is_mock_active = true;
            }
            self.active_stage = source;
            self.is_fullscreen = !self.is_fullscreen;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.is_fullscreen));
        } else if response.clicked() {
            if source == StageSource::MockSource && self.mock_source.is_none() {
                self.mock_source = Some(crate::network::InternalMockSource::start());
                self.is_mock_active = true;
            }
            self.active_stage = source;
        }

        // 2. Подписи у превью ПОД ЭКРАНОМ (чистый контрастный белый текст)
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(title).strong().size(12.5).color(Color32::WHITE));
            if is_active_on_stage {
                ui.label(egui::RichText::new("● В фокусе").size(10.5).color(Color32::from_rgb(88, 101, 242)));
            }
        });
        ui.label(egui::RichText::new(subtitle).size(10.5).color(Color32::from_rgb(205, 210, 222)));
        ui.add_space(8.0);
    }

    /// Вкладка: Независимые аудио каналы + 10-секундная звуковая волна на чёрном фоне
    fn render_audio_and_spectrum(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.heading(egui::RichText::new("Осциллограф и уровни звука (10 сек)").color(Color32::WHITE).strong());
                ui.label(egui::RichText::new("Реальная звуковая волна микрофона и рабочего стола на черном фоне с шкалой времени.").color(Color32::from_rgb(210, 215, 225)));
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut now_track = None;
                if let Some(ref mock) = self.mock_source {
                    let t = mock.bgm_player.lock().unwrap().current_track_name();
                    if !t.is_empty() {
                        now_track = Some(t);
                    }
                }
                if now_track.is_none() {
                    if let Some(ref rx) = self.p2p_receiver {
                        let track = rx.telemetry.read().unwrap().current_track.clone();
                        if !track.is_empty() {
                            now_track = Some(track);
                        }
                    }
                }
                if let Some(track) = now_track {
                    ui.label(egui::RichText::new(format!("🎶 Играет: {}", track)).color(Color32::from_rgb(35, 165, 90)).strong());
                }
            });
        });

        ui.add_space(12.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            // ИСТОЧНИК 1: ЗВУКОВАЯ ВОЛНА МИКРОФОНА
            self.render_waveform_panel(
                ui,
                "Микрофон (Host Microphone) — Осциллограмма 10s",
                true,
            );

            ui.add_space(16.0);

            // ИСТОЧНИК 2: ЗВУКОВАЯ ВОЛНА СИСТЕМЫ / РАБОЧЕГО СТОЛА / МУЗЫКИ
            self.render_waveform_panel(
                ui,
                "Звук системы (Desktop Audio Loopback) — Осциллограмма 10s",
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
        egui::Frame::new()
            .fill(Color32::from_rgb(22, 24, 28))
            .stroke(Stroke::new(1.0, Color32::from_rgb(45, 48, 56)))
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
                    ui.label(egui::RichText::new(title).strong().size(14.0).color(Color32::WHITE));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let status_str = if is_muted {
                            "ВЫКЛЮЧЕНО (-inf dB)".to_string()
                        } else {
                            format!("Пик: {:.1} dB", peak_db)
                        };
                        let col = if is_muted { Color32::from_rgb(180, 185, 195) } else { Color32::from_rgb(35, 165, 90) };
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
                    (0.0, "-10 сек"),
                    (0.2, "-8 сек"),
                    (0.4, "-6 сек"),
                    (0.6, "-4 сек"),
                    (0.8, "-2 сек"),
                    (1.0, "СЕЙЧАС (0s)"),
                ];
                for &(frac, label) in &time_markers {
                    let tx = rect.left() + frac * rect.width();
                    painter.line_segment(
                        [Pos2::new(tx, rect.top()), Pos2::new(tx, rect.bottom())],
                        Stroke::new(0.5, Color32::from_rgb(32, 36, 44)),
                    );
                    painter.text(
                        Pos2::new(tx + 4.0, rect.top() + 10.0),
                        egui::Align2::LEFT_CENTER,
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
                    let min_y = center_y + pt.min_val * half_h;
                    let max_y = center_y + pt.max_val * half_h;

                    let y1 = min_y.min(max_y).clamp(rect.top() + 4.0, rect.bottom() - 4.0);
                    let y2 = min_y.max(max_y).clamp(rect.top() + 4.0, rect.bottom() - 4.0);

                    if (y2 - y1) > 1.5 {
                        painter.line_segment(
                            [Pos2::new(x, y1), Pos2::new(x, y2)],
                            Stroke::new(1.8, wave_color),
                        );
                        // Гребни волны
                        painter.circle_filled(Pos2::new(x, y1), 1.0, crest_color);
                        painter.circle_filled(Pos2::new(x, y2), 1.0, crest_color);
                    } else {
                        // Фоновая тишина
                        painter.circle_filled(Pos2::new(x, center_y), 0.7, Color32::from_rgb(50, 56, 68));
                    }
                }

                ui.add_space(10.0);

                // Ползунок громкости, растянутый в половину экрана
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Громкость:").color(Color32::WHITE));
                    ui.spacing_mut().slider_width = (ui.available_width() * 0.50).clamp(240.0, 700.0);
                    if is_mic {
                        let btn_text = if self.is_mic_muted { "Включить микрофон" } else { "Выключить микрофон" };
                        if ui.button(btn_text).clicked() {
                            self.is_mic_muted = !self.is_mic_muted;
                        }
                        ui.add(egui::Slider::new(&mut self.mic_waveform.volume, 0.0..=1.5).suffix("x"));
                    } else {
                        let btn_text = if self.is_deafened { "Включить звук" } else { "Выключить звук" };
                        if ui.button(btn_text).clicked() {
                            self.is_deafened = !self.is_deafened;
                        }
                        ui.add(egui::Slider::new(&mut self.game_waveform.volume, 0.0..=1.5).suffix("x"));
                    }
                });
            });
    }

    /// Вкладка: Компактная матрица бенчмарка Resolution × FPS Target
    fn render_benchmark_matrix(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.heading("Аппаратная матрица кремния NVENC (RTX 3060)");
                ui.label(format!("Тестируются разрешения только до физического экрана: {}x{} @ {} Гц.", self.native_width, self.native_height, self.native_hz));
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.is_benchmarking {
                    ui.spinner();
                    ui.label("Идет замер кремния...");
                } else {
                    if ui.button("Запустить повторный замер").clicked() {
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
                .fill(Color32::from_rgb(26, 28, 34))
                .stroke(Stroke::new(1.0, Color32::from_rgb(45, 48, 56)))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(14.0)
                .show(ui, |ui| {
                    egui::Grid::new("matrix_grid")
                        .striped(true)
                        .min_col_width(120.0)
                        .spacing([12.0, 10.0])
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new("Разрешение").strong().size(13.0));
                            for &fps in &fps_columns {
                                let budget = 1000.0 / fps as f32;
                                let header = if fps == self.native_hz && fps > 60 {
                                    format!("{} FPS (Max, {:.1}ms)", fps, budget)
                                } else {
                                    format!("{} FPS ({:.1}ms)", fps, budget)
                                };
                                ui.label(egui::RichText::new(header).strong().size(13.0));
                            }
                            ui.end_row();

                            for (r_idx, row) in matrix.iter().enumerate() {
                                ui.label(egui::RichText::new(&row.res_label).strong().size(13.0));

                                for cell in &row.cells {
                                    let (bg_color, border_color, text_color, label_text) = if cell.is_viable {
                                        (
                                            Color32::from_rgb(24, 46, 32),
                                            Color32::from_rgb(35, 165, 90),
                                            Color32::from_rgb(180, 240, 195),
                                            format!("{} ({:.1} ms)", cell.best_preset_name, cell.latency_ms),
                                        )
                                    } else {
                                        (
                                            Color32::from_rgb(48, 24, 28),
                                            Color32::from_rgb(218, 55, 60),
                                            Color32::from_rgb(250, 170, 175),
                                            format!("Лимит ({:.1} ms)", cell.latency_ms),
                                        )
                                    };

                                    let btn = egui::Button::new(
                                        egui::RichText::new(label_text).color(text_color).strong().size(12.0),
                                    )
                                    .fill(bg_color)
                                    .stroke(Stroke::new(1.0, border_color))
                                    .corner_radius(CornerRadius::same(6));

                                    if ui.add(btn).on_hover_text(format!("Нажмите, чтобы применить {}x{} @ {} FPS с пресетом {}", cell.res_width, cell.res_height, cell.fps, cell.best_preset_name)).clicked() {
                                        picked_cell = Some((r_idx, cell.fps, cell.best_preset_id));
                                    }
                                }
                                ui.end_row();
                            }
                        });
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
        ui.heading("Параметры кодека NVIDIA NVENC (H.265 / HEVC)");
        ui.label("Низкоуровневая настройка кремниевого энкодера RTX 3060.");
        ui.add_space(12.0);

        egui::Frame::new()
            .fill(Color32::from_rgb(26, 28, 34))
            .stroke(Stroke::new(1.0, Color32::from_rgb(45, 48, 56)))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(14.0)
            .show(ui, |ui| {
                ui.label("Режим подбора пресета:");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.preset_mode, PresetMode::AutoSmart, "Auto (Smart)");
                    ui.selectable_value(&mut self.preset_mode, PresetMode::ManualP1, "P1 (Fastest)");
                    ui.selectable_value(&mut self.preset_mode, PresetMode::ManualP3, "P3");
                    ui.selectable_value(&mut self.preset_mode, PresetMode::ManualP4, "P4");
                    ui.selectable_value(&mut self.preset_mode, PresetMode::ManualP5, "P5");
                    ui.selectable_value(&mut self.preset_mode, PresetMode::ManualP6, "P6");
                    ui.selectable_value(&mut self.preset_mode, PresetMode::ManualP7, "P7 (Max Quality)");
                });
                ui.add_space(10.0);

                ui.spacing_mut().slider_width = (ui.available_width() * 0.50).clamp(240.0, 700.0);
                ui.label(egui::RichText::new(format!("Контроль качества CQ (Target Quality): {}", self.cq_quality)).color(Color32::WHITE));
                ui.add(egui::Slider::new(&mut self.cq_quality, 16..=32));
                ui.add_space(10.0);

                ui.label(egui::RichText::new(format!("Максимальный лимит битрейта (Max Bitrate Cap): {} Mbps", self.max_bitrate_mbps)).color(Color32::WHITE));
                ui.add(egui::Slider::new(&mut self.max_bitrate_mbps, 5..=60).suffix(" Mbps"));
                ui.add_space(12.0);

                ui.separator();
                ui.add_space(8.0);
                ui.label("Аппаратная архитектура: Optimus Dual-GPU");
                ui.label("• Захват рабочего стола: Intel(R) UHD Graphics via DXGI Desktop Duplication");
                ui.label("• Кодирование кадров: NVIDIA GeForce RTX 3060 Laptop GPU via nvEncodeAPI64.dll");
                ui.label("• Ограничение задержки: zeroReorderDelay = 1, frameIntervalP = 1 (Zero B-Frames)");
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
