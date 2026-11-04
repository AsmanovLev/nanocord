#![allow(dead_code, unused_variables, unused_imports)]

use crate::benchmark::{generate_recommendations, run_full_benchmark, BenchmarkResult, ProfileRecommendation};
use crate::capture::get_screen_resolution;
use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum PresetMode {
    AutoSmart,
    ManualP1,
    ManualP3,
    ManualP4,
}

pub struct NanocordApp {
    // Разрешение экрана и стрима
    pub native_width: u32,
    pub native_height: u32,
    pub available_resolutions: Vec<(u32, u32, &'static str)>,
    pub selected_res_idx: usize,

    // Настройки кодирования
    pub target_fps: u32,
    pub preset_mode: PresetMode,
    pub cq_quality: u8,
    pub max_bitrate_mbps: u32,

    // Звук
    pub game_volume: f32,
    pub game_muted: bool,
    pub mic_volume: f32,
    pub mic_muted: bool,

    // Статус и статистика
    pub is_streaming: Arc<AtomicBool>,
    pub current_fps: f32,
    pub encode_time_ms: f32,
    pub current_bitrate_mbps: f32,
    pub total_frames: u64,

    // Бенчмарк латентности
    pub benchmark_results: Option<Vec<BenchmarkResult>>,
    pub recommendations: Option<Vec<ProfileRecommendation>>,
    pub is_benchmarking: bool,
    bench_rx: Option<Receiver<Vec<BenchmarkResult>>>,

    // Выбранная вкладка
    pub active_tab: Tab,
}

#[derive(PartialEq)]
pub enum Tab {
    Stream,
    Benchmark,
    Audio,
}

impl Default for NanocordApp {
    fn default() -> Self {
        let (native_w, native_h) = get_screen_resolution();

        let mut available = vec![
            (854, 480, "480p SD"),
            (1280, 720, "720p HD"),
            (1920, 1080, "1080p Full HD"),
        ];

        // Добавляем 1440p только если физический монитор реально поддерживает его
        if native_w >= 2560 && native_h >= 1440 {
            available.push((2560, 1440, "1440p 2K"));
        }

        // По умолчанию выбираем максимально доступное родное разрешение (например, 1080p)
        let default_res_idx = available.len().saturating_sub(1);

        Self {
            native_width: native_w,
            native_height: native_h,
            available_resolutions: available,
            selected_res_idx: default_res_idx,
            target_fps: 60,
            preset_mode: PresetMode::AutoSmart,
            cq_quality: 22,
            max_bitrate_mbps: 30,
            game_volume: 1.0,
            game_muted: false,
            mic_volume: 1.0,
            mic_muted: false,
            is_streaming: Arc::new(AtomicBool::new(false)),
            current_fps: 60.0,
            encode_time_ms: 14.6,
            current_bitrate_mbps: 18.5,
            total_frames: 0,
            benchmark_results: None,
            recommendations: None,
            is_benchmarking: false,
            bench_rx: None,
            active_tab: Tab::Stream,
        }
    }
}

impl NanocordApp {
    /// Возвращает эффективный NVENC пресет с учетом Auto Smart режима
    pub fn get_effective_preset(&self) -> (u8, &'static str, &'static str) {
        match self.preset_mode {
            PresetMode::ManualP1 => (1, "P1", "Ultra-Low Latency"),
            PresetMode::ManualP3 => (3, "P3", "Balanced Quality"),
            PresetMode::ManualP4 => (4, "P4", "High Quality"),
            PresetMode::AutoSmart => {
                let (w, _, _) = self.available_resolutions[self.selected_res_idx];
                if self.target_fps <= 30 {
                    (4, "P4 [Auto]", "High Quality (Огромный запас по времени)")
                } else if self.target_fps == 60 {
                    if w >= 1920 {
                        (3, "P3 [Auto]", "Balanced Quality (Оптимальный sweet-spot)")
                    } else {
                        (3, "P3 [Auto]", "Balanced Quality (Высокий запас)")
                    }
                } else {
                    // 120+ FPS
                    (1, "P1 [Auto]", "Ultra-Low Latency (Критично для герцовки >100 FPS)")
                }
            }
        }
    }

    /// Оценка задержки на кадр для текущей конфигурации
    pub fn estimate_latency_ms(&self) -> f32 {
        let (preset_id, _, _) = self.get_effective_preset();
        let (w, _, _) = self.available_resolutions[self.selected_res_idx];

        // Если есть реальные результаты бенчмарка, берем их!
        if let Some(results) = &self.benchmark_results {
            if let Some(r) = results.iter().find(|r| r.width == w && r.preset_id == preset_id) {
                return r.avg_latency_ms;
            }
        }

        // Иначе базовая аппроксимация для RTX 3060 с шиной Optimus
        match (w, preset_id) {
            (854, 1) => 3.2,
            (854, 3) => 4.1,
            (854, 4) => 5.0,
            (1280, 1) => 5.25,
            (1280, 3) => 6.78,
            (1280, 4) => 8.2,
            (1920, 1) => 10.3,
            (1920, 3) => 14.66,
            (1920, 4) => 18.5,
            (2560, 1) => 17.85,
            (2560, 3) => 23.34,
            _ => 14.6,
        }
    }
}

impl eframe::App for NanocordApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Проверяем завершение фонового бенчмарка
        if let Some(rx) = &self.bench_rx {
            if let Ok(results) = rx.try_recv() {
                self.recommendations = Some(generate_recommendations(&results));
                self.benchmark_results = Some(results);
                self.is_benchmarking = false;
                self.bench_rx = None;
            }
        }

        if self.is_streaming.load(Ordering::Relaxed) {
            ctx.request_repaint();
        }

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading("⚡ NANOCORD");
                ui.label(egui::RichText::new("Native Ultra-Low Latency Screen & Voice").weak());

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.is_streaming.load(Ordering::Relaxed) {
                        ui.label(egui::RichText::new("🔴 LIVE STREAMING").color(egui::Color32::from_rgb(255, 75, 75)).strong());
                    } else if self.is_benchmarking {
                        ui.label(egui::RichText::new("🟡 BENCHMARKING NVENC...").color(egui::Color32::from_rgb(255, 200, 50)).strong());
                    } else {
                        ui.label(egui::RichText::new("🟢 READY").color(egui::Color32::from_rgb(75, 220, 100)).strong());
                    }
                });
            });
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.active_tab, Tab::Stream, "📺 Stream & Controls");
                ui.selectable_value(&mut self.active_tab, Tab::Benchmark, "⚡ Latency Benchmark & Profiles");
                ui.selectable_value(&mut self.active_tab, Tab::Audio, "🎙️ Audio Channels");
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            match self.active_tab {
                Tab::Stream => self.render_stream_tab(ui),
                Tab::Benchmark => self.render_benchmark_tab(ui),
                Tab::Audio => self.render_audio_tab(ui),
            }
        });
    }
}

impl NanocordApp {
    fn render_stream_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // Левая колонка: Настройки стрима
            ui.vertical(|ui| {
                ui.set_width(360.0);
                ui.group(|ui| {
                    ui.heading("Stream Video Configuration");
                    ui.add_space(6.0);

                    // 1. Выбор разрешения
                    ui.label("Stream Resolution:");
                    ui.horizontal_wrapped(|ui| {
                        for (idx, &(_, _, label)) in self.available_resolutions.iter().enumerate() {
                            ui.selectable_value(&mut self.selected_res_idx, idx, label);
                        }
                    });
                    ui.add_space(8.0);

                    // 2. Выбор FPS
                    ui.label("Target Framerate:");
                    ui.horizontal_wrapped(|ui| {
                        ui.selectable_value(&mut self.target_fps, 24, "24");
                        ui.selectable_value(&mut self.target_fps, 30, "30");
                        ui.selectable_value(&mut self.target_fps, 60, "60 FPS");
                        ui.selectable_value(&mut self.target_fps, 120, "120");
                        ui.selectable_value(&mut self.target_fps, 144, "144");
                        ui.selectable_value(&mut self.target_fps, 240, "240");
                    });
                    ui.add_space(8.0);

                    // 3. Выбор пресета NVENC
                    ui.label("NVENC Hardware Preset:");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.preset_mode, PresetMode::AutoSmart, "✨ Auto (Smart)");
                        ui.selectable_value(&mut self.preset_mode, PresetMode::ManualP1, "P1 (Ultra-Fast)");
                        ui.selectable_value(&mut self.preset_mode, PresetMode::ManualP3, "P3 (Balanced)");
                        ui.selectable_value(&mut self.preset_mode, PresetMode::ManualP4, "P4 (High)");
                    });

                    let (_, p_label, p_desc) = self.get_effective_preset();
                    ui.label(egui::RichText::new(format!("Active Preset: {} — {}", p_label, p_desc)).size(11.0).weak());
                    ui.add_space(8.0);

                    // 4. Анализатор запаса по времени (Budget & Headroom)
                    let frame_budget = 1000.0 / self.target_fps as f32;
                    let est_latency = self.estimate_latency_ms();
                    let headroom_ms = frame_budget - est_latency;

                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.label("Frame Budget:");
                            ui.label(egui::RichText::new(format!("{:.1} ms", frame_budget)).strong());
                            ui.label("| Est. Encode:");
                            ui.label(egui::RichText::new(format!("{:.1} ms", est_latency)).strong());
                        });

                        if headroom_ms < 0.0 {
                            ui.colored_label(
                                egui::Color32::from_rgb(255, 70, 70),
                                format!("⚠️ ПРЕВЫШЕНИЕ БЮДЖЕТА на {:.1} мс! Риск пропуска кадров на {} FPS.", -headroom_ms, self.target_fps),
                            );
                            ui.label(egui::RichText::new("Совет: выберите разрешение 720p или пресет P1.").size(11.0).weak());
                        } else if headroom_ms < 3.0 {
                            ui.colored_label(
                                egui::Color32::from_rgb(255, 200, 50),
                                format!("⚠️ Небольшой запас: +{:.1} мс ({:.0}%). В тяжелых играх возможны задержки.", headroom_ms, (headroom_ms / frame_budget) * 100.0),
                            );
                        } else {
                            ui.colored_label(
                                egui::Color32::from_rgb(80, 220, 100),
                                format!("✓ Отличный запас: +{:.1} мс ({:.0}%). Стабильный захват без просадок.", headroom_ms, (headroom_ms / frame_budget) * 100.0),
                            );
                        }
                    });
                    ui.add_space(8.0);

                    // 5. CQ Качество и Битрейт
                    let cq_label = match self.cq_quality {
                        16..=19 => "Visually Lossless",
                        20..=23 => "Ultra Crisp (Recommended)",
                        24..=27 => "Balanced",
                        _ => "High Compression",
                    };
                    ui.label(format!("Target Quality CQ: {} ({})", self.cq_quality, cq_label));
                    ui.add(egui::Slider::new(&mut self.cq_quality, 16..=32));
                    ui.add_space(6.0);

                    ui.label(format!("Max Bitrate Cap: {} Mbps", self.max_bitrate_mbps));
                    ui.add(egui::Slider::new(&mut self.max_bitrate_mbps, 5..=60).suffix(" Mbps"));
                    ui.add_space(12.0);

                    // Кнопка Старт / Стоп
                    let is_active = self.is_streaming.load(Ordering::Relaxed);
                    let btn_text = if is_active { "⏹ STOP STREAM" } else { "▶ START LIVE STREAM" };
                    let btn_color = if is_active {
                        egui::Color32::from_rgb(180, 50, 50)
                    } else {
                        egui::Color32::from_rgb(45, 140, 70)
                    };

                    if ui.add_sized([340.0, 36.0], egui::Button::new(egui::RichText::new(btn_text).strong().size(15.0)).fill(btn_color)).clicked() {
                        self.toggle_stream();
                    }
                });
            });

            ui.add_space(16.0);

            // Правая колонка: Телеметрия и характеристики оборудования
            ui.vertical(|ui| {
                ui.group(|ui| {
                    ui.heading("Live Telemetry & Architecture");
                    ui.add_space(8.0);

                    let (w, h, res_name) = self.available_resolutions[self.selected_res_idx];
                    let (_, p_name, _) = self.get_effective_preset();

                    egui::Grid::new("telemetry_grid").num_columns(2).spacing([40.0, 10.0]).show(ui, |ui| {
                        ui.label("Display Resolution:");
                        ui.label(format!("{}x{} (Native Monitor)", self.native_width, self.native_height));
                        ui.end_row();

                        ui.label("Active Output Stream:");
                        ui.label(egui::RichText::new(format!("{} ({}x{}) @ {} FPS", res_name, w, h, self.target_fps)).strong());
                        ui.end_row();

                        ui.label("Hardware Encoder:");
                        ui.label(egui::RichText::new("NVIDIA GeForce RTX 3060 Laptop GPU").color(egui::Color32::from_rgb(118, 185, 0)).strong());
                        ui.end_row();

                        ui.label("Video Codec / Preset:");
                        ui.label(format!("HEVC (H.265) CQ-VBR {}", p_name));
                        ui.end_row();

                        ui.label("GPU Latency / Frame:");
                        ui.label(egui::RichText::new(format!("{:.2} ms", self.estimate_latency_ms())).color(egui::Color32::from_rgb(80, 200, 255)).size(16.0).strong());
                        ui.end_row();

                        ui.label("Current Bitrate:");
                        ui.label(egui::RichText::new(format!("{:.1} Mbps", self.current_bitrate_mbps)).size(16.0).strong());
                        ui.end_row();

                        ui.label("Frames Encoded:");
                        ui.label(format!("{}", self.total_frames));
                        ui.end_row();
                    });
                });
            });
        });
    }

    fn render_benchmark_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("NVENC Hardware Latency Probe & Smart Profiles");
        ui.label(format!(
            "Аппаратный замер кремния RTX 3060. Тестируются разрешения только в пределах физического экрана: {}x{}.",
            self.native_width, self.native_height
        ));
        ui.add_space(10.0);

        if self.is_benchmarking {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new("Выполняются замеры кадров на GPU... Пожалуйста, подождите.").strong());
            });
        } else {
            if ui.add_sized([280.0, 36.0], egui::Button::new(egui::RichText::new("⚡ ЗАПУСТИТЬ ПРОБУ ЛАТЕНТНОСТИ").strong().size(14.0)).fill(egui::Color32::from_rgb(60, 100, 180))).clicked() {
                self.start_benchmark();
            }
        }

        ui.add_space(14.0);

        // Карточки рекомендованных профилей
        if let Some(recs) = &self.recommendations {
            ui.heading("🎯 Рекомендуемые профили стриминга (на основе кремния):");
            ui.add_space(6.0);

            let mut to_apply = None;

            egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                for rec in recs {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(&rec.title).strong().size(14.0));
                            ui.label(format!("→ {} ({})", rec.res_label, rec.preset_name));

                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button(egui::RichText::new("Применить профиль").strong()).clicked() {
                                    to_apply = Some(rec.clone());
                                }
                                ui.colored_label(
                                    egui::Color32::from_rgb(80, 220, 100),
                                    format!("Запас: +{:.0}% ({:.1} мс)", rec.headroom_percent, rec.frame_budget_ms - rec.latency_ms),
                                );
                            });
                        });
                        ui.label(egui::RichText::new(&rec.description).size(12.0).weak());
                    });
                    ui.add_space(4.0);
                }
            });

            if let Some(rec) = to_apply {
                self.apply_recommendation(&rec);
            }

            ui.add_space(12.0);
        }

        // Детальная таблица результатов
        if let Some(results) = &self.benchmark_results {
            ui.group(|ui| {
                ui.label(egui::RichText::new("📊 Детальные замеры задержки кадров (NVENC RTX 3060):").strong());
                ui.add_space(6.0);

                egui::Grid::new("bench_table")
                    .striped(true)
                    .spacing([24.0, 8.0])
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new("Разрешение").strong());
                        ui.label(egui::RichText::new("Пресет NVENC").strong());
                        ui.label(egui::RichText::new("Средняя задержка").strong());
                        ui.label(egui::RichText::new("Мин. задержка").strong());
                        ui.label(egui::RichText::new("Макс. FPS чипа").strong());
                        ui.label(egui::RichText::new("Размер IDR / P").strong());
                        ui.end_row();

                        for r in results {
                            ui.label(&r.resolution);
                            ui.label(&r.preset_name);
                            ui.label(egui::RichText::new(format!("{:.2} ms", r.avg_latency_ms)).color(egui::Color32::from_rgb(80, 220, 255)).strong());
                            ui.label(format!("{:.2} ms", r.min_latency_ms));
                            ui.label(egui::RichText::new(format!("{} FPS", r.max_fps)).color(egui::Color32::from_rgb(100, 255, 100)).strong());
                            ui.label(format!("{:.1} KB / {:.1} KB", r.idr_size_kb, r.p_frame_size_kb));
                            ui.end_row();
                        }
                    });
            });
        }
    }

    fn render_audio_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Independent Dual-Audio Channels");
        ui.label("Раздельные аудиопотоки с независимой громкостью (как в Discord / SonoBus).");
        ui.add_space(12.0);

        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("🎮 Game / Desktop Audio (WASAPI Loopback)").strong());
                if ui.button(if self.game_muted { "🔇 Unmute" } else { "🔊 Mute" }).clicked() {
                    self.game_muted = !self.game_muted;
                }
            });
            ui.add_space(4.0);
            ui.add(egui::Slider::new(&mut self.game_volume, 0.0..=1.5).text("Громкость игры"));
            let val = if self.game_muted { 0.0 } else { 0.72 };
            ui.add(egui::ProgressBar::new(val).text("L: ▓▓▓▓▓▓▓▓▓▓░░  R: ▓▓▓▓▓▓▓▓▓▓░░"));
        });

        ui.add_space(12.0);

        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("🎙️ Host Microphone (WASAPI Input)").strong());
                if ui.button(if self.mic_muted { "🔇 Unmute" } else { "🎙️ Mute" }).clicked() {
                    self.mic_muted = !self.mic_muted;
                }
            });
            ui.add_space(4.0);
            ui.add(egui::Slider::new(&mut self.mic_volume, 0.0..=1.5).text("Громкость микрофона"));
            let val = if self.mic_muted { 0.0 } else { 0.45 };
            ui.add(egui::ProgressBar::new(val).text("Mic: ▓▓▓▓▓▓░░░░░░"));
        });
    }

    fn toggle_stream(&mut self) {
        let current = self.is_streaming.load(Ordering::Relaxed);
        self.is_streaming.store(!current, Ordering::Relaxed);
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

    fn apply_recommendation(&mut self, rec: &ProfileRecommendation) {
        self.target_fps = rec.target_fps;

        // Находим индекс разрешения
        if let Some(idx) = self.available_resolutions.iter().position(|&(w, _, _)| w == rec.width) {
            self.selected_res_idx = idx;
        }

        // Пресет
        self.preset_mode = match rec.preset_id {
            1 => PresetMode::ManualP1,
            3 => PresetMode::ManualP3,
            4 => PresetMode::ManualP4,
            _ => PresetMode::AutoSmart,
        };

        // Переключаемся на главную вкладку стрима
        self.active_tab = Tab::Stream;
    }
}
