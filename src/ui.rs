use crate::benchmark::{run_full_benchmark, BenchmarkResult};
use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;

pub struct NanocordApp {
    // Настройки стрима
    pub target_fps: u32,
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
        Self {
            target_fps: 60,
            cq_quality: 22,
            max_bitrate_mbps: 30,
            game_volume: 1.0,
            game_muted: false,
            mic_volume: 1.0,
            mic_muted: false,
            is_streaming: Arc::new(AtomicBool::new(false)),
            current_fps: 60.0,
            encode_time_ms: 1.2,
            current_bitrate_mbps: 18.5,
            total_frames: 0,
            benchmark_results: None,
            is_benchmarking: false,
            bench_rx: None,
            active_tab: Tab::Stream,
        }
    }
}

impl eframe::App for NanocordApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Проверяем готовность результатов бенчмарка из фонового потока
        if let Some(rx) = &self.bench_rx {
            if let Ok(results) = rx.try_recv() {
                self.benchmark_results = Some(results);
                self.is_benchmarking = false;
                self.bench_rx = None;
            }
        }

        // Постоянная перерисовка во время активного стрима для плавных графиков и счетчиков
        if self.is_streaming.load(Ordering::Relaxed) {
            ctx.request_repaint();
        }

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading("⚡ NANOCORD");
                ui.label(egui::RichText::new("Native Ultra-Low Latency Streamer").weak());

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
                ui.selectable_value(&mut self.active_tab, Tab::Benchmark, "⚡ Latency Benchmark");
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
                ui.set_width(320.0);
                ui.group(|ui| {
                    ui.heading("Video Encoding (NVENC HEVC)");
                    ui.add_space(8.0);

                    // FPS выбор
                    ui.label("Target Framerate:");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.target_fps, 60, "60 FPS");
                        ui.selectable_value(&mut self.target_fps, 120, "120 FPS");
                        ui.selectable_value(&mut self.target_fps, 144, "144 FPS");
                        ui.selectable_value(&mut self.target_fps, 240, "240 FPS");
                    });
                    ui.add_space(8.0);

                    // CQ Качество
                    let cq_label = match self.cq_quality {
                        16..=19 => "Visually Lossless",
                        20..=23 => "Ultra Crisp (Recommended)",
                        24..=27 => "Balanced",
                        _ => "High Compression",
                    };
                    ui.label(format!("Target Quality CQ: {} ({})", self.cq_quality, cq_label));
                    ui.add(egui::Slider::new(&mut self.cq_quality, 16..=32));
                    ui.add_space(8.0);

                    // Максимальный битрейт
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

                    if ui.add_sized([300.0, 36.0], egui::Button::new(egui::RichText::new(btn_text).strong().size(15.0)).fill(btn_color)).clicked() {
                        self.toggle_stream();
                    }
                });
            });

            ui.add_space(16.0);

            // Правая колонка: Телеметрия и мониторинг
            ui.vertical(|ui| {
                ui.group(|ui| {
                    ui.heading("Live Telemetry & Performance");
                    ui.add_space(8.0);

                    egui::Grid::new("telemetry_grid").num_columns(2).spacing([40.0, 10.0]).show(ui, |ui| {
                        ui.label("Display Capture Device:");
                        ui.label(egui::RichText::new("Intel(R) UHD Graphics (Optimus Display)").strong());
                        ui.end_row();

                        ui.label("Hardware Encoder:");
                        ui.label(egui::RichText::new("NVIDIA GeForce RTX 3060 Laptop GPU").color(egui::Color32::from_rgb(118, 185, 0)).strong());
                        ui.end_row();

                        ui.label("Video Codec:");
                        ui.label("H.265 / HEVC Main (NVENC CQ-VBR P1)");
                        ui.end_row();

                        ui.label("Real-time FPS:");
                        ui.label(egui::RichText::new(format!("{:.1} FPS", self.current_fps)).size(16.0).strong());
                        ui.end_row();

                        ui.label("GPU Encode Latency:");
                        ui.label(egui::RichText::new(format!("{:.2} ms / frame", self.encode_time_ms)).color(egui::Color32::from_rgb(80, 200, 255)).size(16.0).strong());
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
        ui.heading("NVENC Hardware Latency Probe");
        ui.label("Тестирование реальной задержки кодирования кремнием RTX 3060 на разных разрешениях и пресетах.");
        ui.add_space(12.0);

        if self.is_benchmarking {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new("Выполняются замеры кадров на GPU... Пожалуйста, подождите.").strong());
            });
        } else {
            if ui.add_sized([260.0, 36.0], egui::Button::new(egui::RichText::new("⚡ ЗАПУСТИТЬ ПРОБУ ЛАТЕНТНОСТИ").strong().size(14.0)).fill(egui::Color32::from_rgb(60, 100, 180))).clicked() {
                self.start_benchmark();
            }
        }

        ui.add_space(16.0);

        if let Some(results) = &self.benchmark_results {
            ui.group(|ui| {
                ui.label(egui::RichText::new("📊 Результаты измерений NVENC RTX 3060:").strong());
                ui.add_space(8.0);

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
            // Имитация VU-метра
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
}
