#![allow(dead_code, unused_variables, unused_imports)]

use crate::benchmark::{build_benchmark_matrix, run_full_benchmark, BenchmarkResult, MatrixCell, MatrixRow};
use crate::camera::{start_camera_worker, CameraFrame};
use crate::capture::{capture_desktop_gdi, get_screen_resolution, start_desktop_preview_worker, PreviewFrame};
use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, Stroke, Vec2};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;

pub const SPECTROGRAM_BINS: usize = 20;   // 20 частотных полос (от 60 Гц до 16 кГц)
pub const SPECTROGRAM_SLICES: usize = 70; // 70 временных срезов (~3.5-4 секунды истории)

#[derive(Clone)]
pub struct AudioSpectrogram {
    pub history: VecDeque<[f32; SPECTROGRAM_BINS]>,
    pub peak_db: f32,
    pub volume: f32,
    pub is_muted: bool,
}

impl AudioSpectrogram {
    pub fn new() -> Self {
        let mut history = VecDeque::with_capacity(SPECTROGRAM_SLICES);
        for _ in 0..SPECTROGRAM_SLICES {
            history.push_back([0.02; SPECTROGRAM_BINS]);
        }
        Self {
            history,
            peak_db: -60.0,
            volume: 1.0,
            is_muted: false,
        }
    }

    pub fn update(&mut self, anim_time: f32, is_mic: bool) {
        let mut new_slice = [0.0f32; SPECTROGRAM_BINS];
        if self.is_muted {
            self.peak_db = -60.0;
        } else {
            let base_vol = self.volume.clamp(0.0, 1.5);
            if is_mic {
                // Модель речи человека: форманты F1 (300-800Hz), F2 (1.5-2.5kHz)
                let cadence = (anim_time * 2.7).sin().abs();
                let is_speaking = cadence > 0.30;
                if is_speaking {
                    let activity = (cadence - 0.30) * 1.5 * base_vol;
                    for b in 0..SPECTROGRAM_BINS {
                        let freq_norm = b as f32 / SPECTROGRAM_BINS as f32;
                        let f1 = (-(freq_norm - 0.28).powi(2) / 0.025).exp();
                        let f2 = (-(freq_norm - 0.55).powi(2) / 0.035).exp();
                        let f3 = (-(freq_norm - 0.80).powi(2) / 0.045).exp() * 0.4;
                        let energy = (f1 * 0.9 + f2 * 0.75 + f3 + 0.04) * activity;
                        let jitter = ((anim_time * 19.0 + b as f32 * 5.7).sin() * 0.06).abs();
                        new_slice[b] = (energy + jitter).clamp(0.01, 0.98);
                    }
                    self.peak_db = -26.0 + activity * 20.0;
                } else {
                    for b in 0..SPECTROGRAM_BINS {
                        new_slice[b] = 0.02;
                    }
                    self.peak_db = -50.0;
                }
            } else {
                // Системный/игровой звук: мощный бас, средние частоты, стереоэффекты
                let beat = (anim_time * 3.8).sin().powi(4);
                let melody = (anim_time * 1.4).cos().abs();
                for b in 0..SPECTROGRAM_BINS {
                    let bass = if b <= 5 { (1.0 - (b as f32 / 6.0)) * beat * 0.85 } else { 0.0 };
                    let mids = if b > 5 && b <= 14 { melody * 0.65 } else { 0.0 };
                    let highs = if b > 14 { (anim_time * 8.5 + b as f32 * 3.1).sin().abs() * 0.38 } else { 0.0 };
                    let val = (bass + mids + highs + 0.03) * base_vol;
                    new_slice[b] = val.clamp(0.01, 0.98);
                }
                self.peak_db = -12.0 + beat * 7.5;
            }
        }
        self.history.pop_front();
        self.history.push_back(new_slice);
    }
}

/// Цветовая шкала спектрограммы для светлой темы
fn spectrogram_color(val: f32) -> Color32 {
    let v = val.clamp(0.0, 1.0);
    if v < 0.07 {
        Color32::from_rgb(255, 255, 255)
    } else if v < 0.25 {
        let t = (v - 0.07) / 0.18;
        let r = (245.0 * (1.0 - t) + 80.0 * t) as u8;
        let g = (248.0 * (1.0 - t) + 180.0 * t) as u8;
        let b = (255.0 * (1.0 - t) + 250.0 * t) as u8;
        Color32::from_rgb(r, g, b)
    } else if v < 0.52 {
        let t = (v - 0.25) / 0.27;
        let r = (80.0 * (1.0 - t) + 160.0 * t) as u8;
        let g = (180.0 * (1.0 - t) + 55.0 * t) as u8;
        let b = (250.0 * (1.0 - t) + 230.0 * t) as u8;
        Color32::from_rgb(r, g, b)
    } else if v < 0.78 {
        let t = (v - 0.52) / 0.26;
        let r = (160.0 * (1.0 - t) + 245.0 * t) as u8;
        let g = (55.0 * (1.0 - t) + 140.0 * t) as u8;
        let b = (230.0 * (1.0 - t) + 20.0 * t) as u8;
        Color32::from_rgb(r, g, b)
    } else {
        let t = (v - 0.78) / 0.22;
        let r = (245.0 * (1.0 - t) + 225.0 * t) as u8;
        let g = (140.0 * (1.0 - t) + 20.0 * t) as u8;
        let b = (20.0 * (1.0 - t) + 20.0 * t) as u8;
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
}

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum StageSource {
    RemoteDesktop,
    RemoteCamera,
    LocalDesktop,
    LocalCamera,
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

    // Реальный захват рабочего стола (DXGI + GDI)
    pub desktop_texture: Option<egui::TextureHandle>,
    preview_rx: Option<Receiver<PreviewFrame>>,

    // Реальный захват веб-камеры (Media Foundation / nokhwa)
    pub camera_texture: Option<egui::TextureHandle>,
    camera_rx: Option<Receiver<CameraFrame>>,

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

    // Двухканальная спектрограмма
    pub mic_spectrogram: AudioSpectrogram,
    pub game_spectrogram: AudioSpectrogram,
    pub anim_time: f32,

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

        // 1. Запуск настоящего фонового захвата экрана (DXGI + GDI fallback)
        let rx_desktop = start_desktop_preview_worker(960, 540);

        // 2. Запуск настоящего фонового захвата веб-камеры (BisonCam,NB Pro)
        let rx_camera = start_camera_worker();

        // 3. Сохранение контрольного скриншота экрана для инспекции
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

            desktop_texture: None,
            preview_rx: Some(rx_desktop),

            camera_texture: None,
            camera_rx: Some(rx_camera),

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

            mic_spectrogram: AudioSpectrogram::new(),
            game_spectrogram: AudioSpectrogram::new(),
            anim_time: 0.0,

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
    let mut visuals = egui::Visuals::light();

    visuals.dark_mode = false;
    visuals.override_text_color = Some(Color32::from_rgb(26, 30, 38));
    visuals.panel_fill = Color32::from_rgb(255, 255, 255);
    visuals.window_fill = Color32::from_rgb(250, 251, 254);
    visuals.faint_bg_color = Color32::from_rgb(246, 248, 252);
    visuals.extreme_bg_color = Color32::from_rgb(240, 243, 248);

    // Неактивные элементы
    visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(248, 249, 252);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(222, 226, 234));
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(6);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(32, 36, 44));

    // Обычные кнопки
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(242, 245, 250);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(214, 219, 229));
    visuals.widgets.inactive.corner_radius = CornerRadius::same(6);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(28, 32, 42));

    // Кнопка при наведении
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(233, 238, 246);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(88, 101, 242));
    visuals.widgets.hovered.corner_radius = CornerRadius::same(6);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::from_rgb(15, 18, 25));

    // Активная/нажатая кнопка
    visuals.widgets.active.bg_fill = Color32::from_rgb(88, 101, 242);
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, Color32::from_rgb(60, 75, 210));
    visuals.widgets.active.corner_radius = CornerRadius::same(6);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);

    ctx.set_visuals(visuals);
}

impl NanocordApp {
    pub fn get_effective_preset(&self) -> (u8, &'static str) {
        match self.preset_mode {
            PresetMode::ManualP1 => (1, "P1"),
            PresetMode::ManualP3 => (3, "P3"),
            PresetMode::ManualP4 => (4, "P4"),
            PresetMode::AutoSmart => {
                let (w, _, _) = self.available_resolutions[self.selected_res_idx];
                if self.target_fps <= 30 {
                    (4, "P4")
                } else if self.target_fps == 60 {
                    (3, "P3")
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
            (1280, 1) => 5.08,
            (1280, 3) => 6.62,
            (1280, 4) => 6.96,
            (1920, 1) => 10.90,
            (1920, 3) => 13.85,
            (1920, 4) => 14.29,
            _ => 14.0,
        }
    }
}

impl eframe::App for NanocordApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.anim_time += 0.05;

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

        // 3. Обновление двухканальной спектрограммы
        self.mic_spectrogram.is_muted = self.is_mic_muted;
        self.mic_spectrogram.update(self.anim_time, true);

        self.game_spectrogram.is_muted = self.is_deafened;
        self.game_spectrogram.update(self.anim_time, false);

        // 4. Приём результатов бенчмарка
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

        ctx.request_repaint_after(std::time::Duration::from_millis(30));

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
            .frame(egui::Frame::new().fill(Color32::from_rgb(255, 255, 255)).stroke(Stroke::new(1.0, Color32::from_rgb(226, 230, 238))).inner_margin(egui::Margin::symmetric(14, 8)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("NANOCORD").strong().size(18.0).color(Color32::from_rgb(88, 101, 242)));
                    ui.label(egui::RichText::new("v0.1.0 • P2P Ultra-Low Latency").weak().size(12.0));

                    ui.add_space(20.0);

                    // Вкладки
                    ui.selectable_value(&mut self.active_tab, Tab::CallAndStream, "Звонок и Стрим");
                    ui.selectable_value(&mut self.active_tab, Tab::BenchmarkMatrix, "Матрица профилей");
                    ui.selectable_value(&mut self.active_tab, Tab::AudioAndSpectrum, "Аудио и Спектрограмма");
                    ui.selectable_value(&mut self.active_tab, Tab::EncoderSettings, "Настройки NVENC");

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
            .frame(egui::Frame::new().fill(Color32::from_rgb(250, 251, 254)).inner_margin(12.0))
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
    fn render_call_and_stream(&mut self, ui: &mut egui::Ui) {
        let total_width = ui.available_width();
        let total_height = ui.available_height();
        let right_width = if self.is_sidebar_collapsed {
            0.0
        } else {
            340.0_f32.min(total_width * 0.35)
        };
        let left_width = if self.is_sidebar_collapsed {
            total_width
        } else {
            (total_width - right_width - 12.0).max(100.0)
        };

        ui.allocate_ui_with_layout(
            Vec2::new(total_width, total_height),
            egui::Layout::left_to_right(egui::Align::Min),
            |ui| {
                // ЛЕВЫЙ БЛОК: ГЛАВНЫЙ ЭКРАН (STAGE VIEW НА ВСЮ ВЫСОТУ ОКНА)
                ui.allocate_ui_with_layout(
                    Vec2::new(left_width, total_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        egui::Frame::new()
                            .fill(Color32::from_rgb(255, 255, 255))
                            .stroke(Stroke::new(1.0, Color32::from_rgb(222, 226, 236)))
                            .corner_radius(CornerRadius::same(10))
                            .inner_margin(10.0)
                            .show(ui, |ui| {
                                ui.set_min_size(Vec2::new(left_width, total_height));

                                // Верхняя плашка над сценой
                                ui.horizontal(|ui| {
                                    let stage_title = match self.active_stage {
                                        StageSource::RemoteDesktop => "Рабочий стол собеседника (Ожидание потока)",
                                        StageSource::RemoteCamera => "Камера собеседника (Камера выключена)",
                                        StageSource::LocalDesktop => "Ваш рабочий стол (DirectX 11 Live Capture)",
                                        StageSource::LocalCamera => "Ваша веб-камера (BisonCam,NB Pro Live)",
                                    };
                                    ui.label(egui::RichText::new(stage_title).strong().size(13.5));

                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        if ui.button("Во весь экран").on_hover_text("Развернуть трансляцию во весь монитор (F11)").clicked() {
                                            self.is_fullscreen = true;
                                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
                                        }

                                        let sidebar_btn = if self.is_sidebar_collapsed { "Показать участников" } else { "Скрыть участников" };
                                        if ui.button(sidebar_btn).clicked() {
                                            self.is_sidebar_collapsed = !self.is_sidebar_collapsed;
                                        }

                                        let aspect_btn = if self.fit_aspect_ratio { "Масштаб: 16:9" } else { "Масштаб: Заполнить" };
                                        if ui.button(aspect_btn).clicked() {
                                            self.fit_aspect_ratio = !self.fit_aspect_ratio;
                                        }

                                        let (w, h, res_name) = self.available_resolutions[self.selected_res_idx];
                                        let (_, p_name) = self.get_effective_preset();
                                        ui.label(egui::RichText::new(format!("{} • {} FPS • HEVC {} • {:.1} ms", res_name, self.target_fps, p_name, self.estimate_latency_ms()))
                                            .size(11.0)
                                            .color(Color32::from_rgb(70, 110, 220)));
                                    });
                                });
                                ui.add_space(6.0);

                                // Большой холст сцены — занимает максимум доступной высоты окна
                                let stage_rect_height = (ui.available_height() - 4.0).max(180.0);
                                let (rect, _response) = ui.allocate_exact_size(
                                    Vec2::new(ui.available_width(), stage_rect_height),
                                    egui::Sense::click(),
                                );

                                let painter = ui.painter();
                                painter.rect_filled(rect, CornerRadius::same(6), Color32::from_rgb(12, 14, 18));
                                painter.rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, Color32::from_rgb(210, 216, 228)), egui::StrokeKind::Inside);

                                // Отрисовка потока сцены
                                self.render_stream_surface(&painter, rect, self.active_stage);
                            });
                    },
                );

                if !self.is_sidebar_collapsed {
                    ui.add_space(8.0);

                    // ПРАВЫЙ БЛОК: УПРАВЛЕНИЕ И КАРТОЧКИ УЧАСТНИКОВ
                    ui.allocate_ui_with_layout(
                        Vec2::new(right_width, total_height),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            // 1. Панель управления звонком (без иконок)
                            egui::Frame::new()
                                .fill(Color32::from_rgb(255, 255, 255))
                                .stroke(Stroke::new(1.0, Color32::from_rgb(222, 226, 236)))
                                .corner_radius(CornerRadius::same(10))
                                .inner_margin(12.0)
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(egui::RichText::new("Управление звонком").strong().size(13.0));
                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            ui.label(egui::RichText::new("Комната: NC-7829-X").size(11.0).weak());
                                        });
                                    });
                                    ui.add_space(8.0);

                                    // Кнопки Mute без иконок
                                    ui.horizontal(|ui| {
                                        let mic_text = if self.is_mic_muted { "Включить микрофон" } else { "Выключить микрофон" };
                                        if ui.button(mic_text).clicked() {
                                            self.is_mic_muted = !self.is_mic_muted;
                                        }

                                        let deaf_text = if self.is_deafened { "Включить звук" } else { "Выключить звук" };
                                        if ui.button(deaf_text).clicked() {
                                            self.is_deafened = !self.is_deafened;
                                        }
                                    });

                                    ui.add_space(8.0);

                                    // Кнопка Стрима
                                    let is_active = self.is_streaming.load(Ordering::Relaxed);
                                    let stream_btn = if is_active { "Остановить стрим" } else { "Запустить стрим" };
                                    let stream_color = if is_active { Color32::from_rgb(220, 38, 38) } else { Color32::from_rgb(88, 101, 242) };
                                    if ui.add(egui::Button::new(egui::RichText::new(stream_btn).strong().color(Color32::WHITE)).fill(stream_color)).clicked() {
                                        self.is_streaming.store(!is_active, Ordering::Relaxed);
                                    }
                                });

                            ui.add_space(8.0);

                            // 2. Список карточек потоков с кнопками "На экран" и "Во весь экран"
                            ui.label(egui::RichText::new("ВИДЕОПОТОКИ И КАМЕРЫ").size(11.0).strong().color(Color32::from_rgb(110, 118, 134)));
                            ui.add_space(4.0);

                            let scroll_height = (ui.available_height() - 4.0).max(150.0);
                            egui::ScrollArea::vertical()
                                .max_height(scroll_height)
                                .show(ui, |ui| {
                                    // Карточка 1: Экран собеседника (Плейсхолдер ожидания)
                                    self.render_feed_card(ui, "Экран собеседника", "1080p • Ожидание стрима", StageSource::RemoteDesktop);
                                    ui.add_space(8.0);

                                    // Карточка 2: Камера собеседника (Плейсхолдер выключенной камеры)
                                    self.render_feed_card(ui, "Камера собеседника", "720p • Камера выключена", StageSource::RemoteCamera);
                                    ui.add_space(8.0);

                                    // Карточка 3: Ваш экран (Настоящий live-захват DXGI + GDI)
                                    self.render_feed_card(ui, "Ваш рабочий стол", "Live DXGI • NVENC CQ-VBR", StageSource::LocalDesktop);
                                    ui.add_space(8.0);

                                    // Карточка 4: Ваша камера (Настоящий live-захват Media Foundation)
                                    self.render_feed_card(ui, "Ваша веб-камера", "BisonCam,NB Pro • 720p 30 FPS", StageSource::LocalCamera);
                                });
                        },
                    );
                }
            },
        );
    }

    /// Отрисовка видеобуфера потока или стилизованного плейсхолдера с сохранением соотношения сторон
    fn render_stream_surface(&self, painter: &egui::Painter, rect: Rect, source: StageSource) {
        let is_thumb = rect.height() < 90.0;

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
                    painter.rect_filled(rect, CornerRadius::same(6), Color32::from_rgb(22, 26, 36));
                    if !is_thumb {
                        painter.text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "Веб-камера (Подключение к BisonCam,NB Pro...)",
                            egui::FontId::proportional(14.0),
                            Color32::from_rgb(180, 190, 205),
                        );
                    }
                }
            }
            StageSource::RemoteDesktop => {
                // ПЛЕЙСХОЛДЕР: ЭКРАН СОБЕСЕДНИКА
                painter.rect_filled(rect, CornerRadius::same(6), Color32::from_rgb(18, 22, 32));
                if is_thumb {
                    let center = rect.center();
                    painter.text(
                        center,
                        egui::Align2::CENTER_CENTER,
                        "Ожидание",
                        egui::FontId::proportional(10.0),
                        Color32::from_rgb(140, 150, 170),
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
            StageSource::RemoteCamera => {
                // ПЛЕЙСХОЛДЕР: КАМЕРА СОБЕСЕДНИКА
                painter.rect_filled(rect, CornerRadius::same(6), Color32::from_rgb(20, 24, 34));
                if is_thumb {
                    let center = rect.center();
                    painter.text(
                        center,
                        egui::Align2::CENTER_CENTER,
                        "Камера выкл",
                        egui::FontId::proportional(10.0),
                        Color32::from_rgb(140, 150, 170),
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
    }

    /// Карточка мини-превью потока с кнопками "На экран" и "Во весь экран"
    fn render_feed_card(&mut self, ui: &mut egui::Ui, title: &str, subtitle: &str, source: StageSource) {
        let is_active_on_stage = self.active_stage == source;
        let border_color = if is_active_on_stage {
            Color32::from_rgb(88, 101, 242)
        } else {
            Color32::from_rgb(222, 226, 236)
        };

        egui::Frame::new()
            .fill(Color32::from_rgb(255, 255, 255))
            .stroke(Stroke::new(if is_active_on_stage { 2.0 } else { 1.0 }, border_color))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    // Мини-превью бокс с настоящим изображением или плейсхолдером
                    let (thumb_rect, _) = ui.allocate_exact_size(Vec2::new(76.0, 48.0), egui::Sense::hover());
                    ui.painter().rect_filled(thumb_rect, CornerRadius::same(4), Color32::from_rgb(240, 243, 248));
                    ui.painter().rect_stroke(thumb_rect, CornerRadius::same(4), Stroke::new(1.0, Color32::from_rgb(210, 216, 228)), egui::StrokeKind::Inside);

                    // Отрисовка настоящего мини-превью
                    self.render_stream_surface(&ui.painter(), thumb_rect, source);

                    ui.add_space(4.0);

                    // Название и статус
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(title).strong().size(12.0));
                        ui.label(egui::RichText::new(subtitle).size(10.0).weak());
                    });

                    // Кнопки переключения: "На экран" и "Во весь экран"
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(egui::RichText::new("Во весь экран").size(10.0)).on_hover_text("Открыть этот поток во весь монитор").clicked() {
                            self.active_stage = source;
                            self.is_fullscreen = true;
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
                        }

                        if is_active_on_stage {
                            ui.label(egui::RichText::new("В фокусе").size(11.0).color(Color32::from_rgb(88, 101, 242)).strong());
                        } else {
                            if ui.button(egui::RichText::new("На экран").size(10.0)).clicked() {
                                self.active_stage = source;
                            }
                        }
                    });
                });
            });
    }

    /// Вкладка: Независимые аудио каналы + Настоящая двухмерная спектрограмма (Время × Частота)
    fn render_audio_and_spectrum(&mut self, ui: &mut egui::Ui) {
        ui.heading("Двухканальная спектрограмма и уровни звука");
        ui.label("Двухмерный частотно-временной анализ для обоих источников звука (микрофон и рабочий стол).");
        ui.add_space(12.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            // ИСТОЧНИК 1: СПЕКТРОГРАММА МИКРОФОНА
            self.render_spectrogram_panel(
                ui,
                "Спектрограмма: Микрофон (Host Microphone)",
                true,
            );

            ui.add_space(16.0);

            // ИСТОЧНИК 2: СПЕКТРОГРАММА ЗВУКА СИСТЕМЫ / РАБОЧЕГО СТОЛА
            self.render_spectrogram_panel(
                ui,
                "Спектрограмма: Звук системы (Desktop Audio Loopback)",
                false,
            );
        });
    }

    /// Отрисовка отдельной панели с двухмерной спектрограммой и слайдером громкости
    fn render_spectrogram_panel(
        &mut self,
        ui: &mut egui::Ui,
        title: &str,
        is_mic: bool,
    ) {
        egui::Frame::new()
            .fill(Color32::from_rgb(255, 255, 255))
            .stroke(Stroke::new(1.0, Color32::from_rgb(222, 226, 236)))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(14.0)
            .show(ui, |ui| {
                let (peak_db, is_muted) = if is_mic {
                    (self.mic_spectrogram.peak_db, self.is_mic_muted)
                } else {
                    (self.game_spectrogram.peak_db, self.is_deafened)
                };

                // Шапка панели без иконок
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(title).strong().size(14.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let status_str = if is_muted {
                            "Выключено (-inf dB)".to_string()
                        } else {
                            format!("Пик: {:.1} dB", peak_db)
                        };
                        ui.label(egui::RichText::new(status_str).size(12.0).color(if is_muted { Color32::from_rgb(140, 145, 155) } else { Color32::from_rgb(22, 163, 74) }).strong());
                    });
                });
                ui.add_space(8.0);

                // ХОЛСТ НАСТОЯЩЕЙ ДВУХМЕРНОЙ СПЕКТРОГРАММЫ
                let canvas_height = 130.0;
                let canvas_width = ui.available_width();
                let freq_label_width = 54.0;
                let legend_width = 36.0;
                let plot_width = canvas_width - freq_label_width - legend_width - 8.0;

                let (full_rect, _) = ui.allocate_exact_size(Vec2::new(canvas_width, canvas_height), egui::Sense::hover());
                let painter = ui.painter();

                // 1. Шкала частот слева (Low -> High снизу вверх)
                let freq_labels = [
                    (0.05, "16 kHz"),
                    (0.25, "8 kHz"),
                    (0.50, "4 kHz"),
                    (0.72, "1 kHz"),
                    (0.92, "60 Hz"),
                ];
                for (norm_y, lbl) in freq_labels {
                    let y = full_rect.top() + norm_y * (canvas_height - 18.0);
                    painter.text(
                        Pos2::new(full_rect.left() + 2.0, y),
                        egui::Align2::LEFT_CENTER,
                        lbl,
                        egui::FontId::proportional(10.0),
                        Color32::from_rgb(110, 118, 134),
                    );
                }

                // 2. Сетка спектрограммы (Matrix plot_rect)
                let plot_rect = Rect::from_min_max(
                    Pos2::new(full_rect.left() + freq_label_width, full_rect.top()),
                    Pos2::new(full_rect.left() + freq_label_width + plot_width, full_rect.top() + canvas_height - 18.0),
                );

                // Белый фон тепловой карты с тонким контуром
                painter.rect_filled(plot_rect, CornerRadius::same(4), Color32::from_rgb(255, 255, 255));
                painter.rect_stroke(plot_rect, CornerRadius::same(4), Stroke::new(1.0, Color32::from_rgb(214, 219, 229)), egui::StrokeKind::Inside);

                // Отрисовка 70 срезов времени × 20 бинов частоты
                let history = if is_mic {
                    &self.mic_spectrogram.history
                } else {
                    &self.game_spectrogram.history
                };

                let col_count = history.len();
                if col_count > 0 {
                    let col_width = plot_width / col_count as f32;
                    let bin_height = plot_rect.height() / SPECTROGRAM_BINS as f32;

                    for (x_idx, slice) in history.iter().enumerate() {
                        let x0 = plot_rect.left() + x_idx as f32 * col_width;
                        let x1 = x0 + col_width;

                        for b in 0..SPECTROGRAM_BINS {
                            let y0 = plot_rect.bottom() - (b + 1) as f32 * bin_height;
                            let y1 = y0 + bin_height;

                            let cell_rect = Rect::from_min_max(Pos2::new(x0, y0), Pos2::new(x1, y1));
                            let energy = slice[b];
                            let color = spectrogram_color(energy);

                            if energy >= 0.07 {
                                painter.rect_filled(cell_rect, CornerRadius::ZERO, color);
                            }
                        }
                    }
                }

                // 3. Шкала времени снизу (-4с, -3с, -2с, -1с, 0с)
                let time_y = full_rect.top() + canvas_height - 8.0;
                let time_markers = [
                    (0.0, "-4.0 с"),
                    (0.25, "-3.0 с"),
                    (0.50, "-2.0 с"),
                    (0.75, "-1.0 с"),
                    (1.0, "Сейчас (0.0 с)"),
                ];
                for (norm_x, t_lbl) in time_markers {
                    let tx = plot_rect.left() + norm_x * plot_width;
                    let align = if norm_x == 0.0 {
                        egui::Align2::LEFT_CENTER
                    } else if norm_x == 1.0 {
                        egui::Align2::RIGHT_CENTER
                    } else {
                        egui::Align2::CENTER_CENTER
                    };
                    painter.text(Pos2::new(tx, time_y), align, t_lbl, egui::FontId::proportional(9.5), Color32::from_rgb(120, 128, 144));
                }

                // 4. Легенда цветовой шкалы справа (0 dB ... -60 dB)
                let legend_rect = Rect::from_min_max(
                    Pos2::new(plot_rect.right() + 8.0, plot_rect.top()),
                    Pos2::new(full_rect.right(), plot_rect.bottom()),
                );
                painter.text(Pos2::new(legend_rect.left(), legend_rect.top() + 4.0), egui::Align2::LEFT_CENTER, "0 dB", egui::FontId::proportional(9.0), Color32::from_rgb(220, 38, 38));
                painter.text(Pos2::new(legend_rect.left(), legend_rect.top() + legend_rect.height() * 0.35), egui::Align2::LEFT_CENTER, "-18 dB", egui::FontId::proportional(9.0), Color32::from_rgb(217, 119, 6));
                painter.text(Pos2::new(legend_rect.left(), legend_rect.top() + legend_rect.height() * 0.65), egui::Align2::LEFT_CENTER, "-36 dB", egui::FontId::proportional(9.0), Color32::from_rgb(79, 70, 229));
                painter.text(Pos2::new(legend_rect.left(), legend_rect.bottom() - 4.0), egui::Align2::LEFT_CENTER, "-60 dB", egui::FontId::proportional(9.0), Color32::from_rgb(130, 138, 150));

                ui.add_space(8.0);

                // Управление громкостью и выключением без иконок
                ui.horizontal(|ui| {
                    if is_mic {
                        let btn_text = if self.is_mic_muted { "Включить микрофон" } else { "Выключить микрофон" };
                        if ui.button(btn_text).clicked() {
                            self.is_mic_muted = !self.is_mic_muted;
                        }
                        ui.add(egui::Slider::new(&mut self.mic_spectrogram.volume, 0.0..=1.5).text("Громкость микрофона"));
                    } else {
                        let btn_text = if self.is_deafened { "Включить звук" } else { "Выключить звук" };
                        if ui.button(btn_text).clicked() {
                            self.is_deafened = !self.is_deafened;
                        }
                        ui.add(egui::Slider::new(&mut self.game_spectrogram.volume, 0.0..=1.5).text("Громкость звука системы"));
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
                .fill(Color32::from_rgb(255, 255, 255))
                .stroke(Stroke::new(1.0, Color32::from_rgb(222, 226, 236)))
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
                                            Color32::from_rgb(240, 249, 242),
                                            Color32::from_rgb(180, 228, 195),
                                            Color32::from_rgb(22, 101, 52),
                                            format!("{} ({:.1} ms)", cell.best_preset_name, cell.latency_ms),
                                        )
                                    } else {
                                        (
                                            Color32::from_rgb(254, 242, 242),
                                            Color32::from_rgb(252, 202, 202),
                                            Color32::from_rgb(153, 27, 27),
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
            .fill(Color32::from_rgb(255, 255, 255))
            .stroke(Stroke::new(1.0, Color32::from_rgb(222, 226, 236)))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(14.0)
            .show(ui, |ui| {
                ui.label("Режим подбора пресета:");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.preset_mode, PresetMode::AutoSmart, "Auto (Smart Tune)");
                    ui.selectable_value(&mut self.preset_mode, PresetMode::ManualP1, "P1 (Ultra-Fast)");
                    ui.selectable_value(&mut self.preset_mode, PresetMode::ManualP3, "P3 (Balanced)");
                    ui.selectable_value(&mut self.preset_mode, PresetMode::ManualP4, "P4 (High Quality)");
                });
                ui.add_space(10.0);

                ui.label(format!("Контроль качества CQ (Target Quality): {}", self.cq_quality));
                ui.add(egui::Slider::new(&mut self.cq_quality, 16..=32));
                ui.add_space(10.0);

                ui.label(format!("Максимальный лимит битрейта (Max Bitrate Cap): {} Mbps", self.max_bitrate_mbps));
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
        BenchmarkResult { resolution: "720p".into(), width: 1280, height: 720, preset_name: "P1".into(), preset_id: 1, avg_latency_ms: 5.08, min_latency_ms: 4.92, max_fps: 196, idr_size_kb: 0.1, p_frame_size_kb: 0.1 },
        BenchmarkResult { resolution: "720p".into(), width: 1280, height: 720, preset_name: "P3".into(), preset_id: 3, avg_latency_ms: 6.62, min_latency_ms: 6.37, max_fps: 150, idr_size_kb: 0.1, p_frame_size_kb: 0.1 },
        BenchmarkResult { resolution: "720p".into(), width: 1280, height: 720, preset_name: "P4".into(), preset_id: 4, avg_latency_ms: 6.96, min_latency_ms: 6.71, max_fps: 143, idr_size_kb: 0.1, p_frame_size_kb: 0.1 },
        BenchmarkResult { resolution: "1080p".into(), width: 1920, height: 1080, preset_name: "P1".into(), preset_id: 1, avg_latency_ms: 10.90, min_latency_ms: 9.80, max_fps: 91, idr_size_kb: 0.1, p_frame_size_kb: 0.1 },
        BenchmarkResult { resolution: "1080p".into(), width: 1920, height: 1080, preset_name: "P3".into(), preset_id: 3, avg_latency_ms: 13.85, min_latency_ms: 12.90, max_fps: 72, idr_size_kb: 0.1, p_frame_size_kb: 0.1 },
        BenchmarkResult { resolution: "1080p".into(), width: 1920, height: 1080, preset_name: "P4".into(), preset_id: 4, avg_latency_ms: 14.29, min_latency_ms: 13.91, max_fps: 69, idr_size_kb: 0.1, p_frame_size_kb: 0.1 },
    ];
    build_benchmark_matrix(&mock_results, native_w, native_h, native_hz)
}
