#![allow(dead_code, unused_variables)]
#![windows_subsystem = "windows"]

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, Stroke, Vec2};
use nanocord::network::P2PSender;
use std::sync::atomic::Ordering;
use std::time::Duration;

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([640.0, 600.0])
            .with_min_inner_size([520.0, 480.0])
            .with_title("Nanocord Mock Client • P2P Streamer & BGM")
            .with_active(true),
        ..Default::default()
    };

    eframe::run_native(
        "Nanocord Mock Client",
        native_options,
        Box::new(|cc| {
            // Темная технологичная тема
            let mut visuals = egui::Visuals::dark();
            visuals.panel_fill = Color32::from_rgb(18, 20, 24);
            visuals.window_fill = Color32::from_rgb(24, 26, 32);
            cc.egui_ctx.set_visuals(visuals);

            Ok(Box::new(MockClientApp::new()))
        }),
    )
}

struct MockClientApp {
    sender: P2PSender,
    desktop_tex: Option<egui::TextureHandle>,
    camera_tex: Option<egui::TextureHandle>,
}

impl MockClientApp {
    fn new() -> Self {
        let sender = P2PSender::start();
        Self {
            sender,
            desktop_tex: None,
            camera_tex: None,
        }
    }
}

impl eframe::App for MockClientApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Приём локальных кадров превью для собственного окна (только последний кадр)
        let mut latest_dt = None;
        while let Ok(frame) = self.sender.preview_dt_rx.try_recv() {
            latest_dt = Some(frame);
        }
        if let Some(frame) = latest_dt {
            let img = egui::ColorImage::from_rgba_unmultiplied(
                [frame.width as usize, frame.height as usize],
                &frame.rgba,
            );
            match &mut self.desktop_tex {
                Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                None => self.desktop_tex = Some(ctx.load_texture("mock_dt", img, egui::TextureOptions::LINEAR)),
            }
        }

        let mut latest_cam = None;
        while let Ok(frame) = self.sender.preview_cam_rx.try_recv() {
            latest_cam = Some(frame);
        }
        if let Some(frame) = latest_cam {
            let img = egui::ColorImage::from_rgba_unmultiplied(
                [frame.width as usize, frame.height as usize],
                &frame.rgba,
            );
            match &mut self.camera_tex {
                Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                None => self.camera_tex = Some(ctx.load_texture("mock_cam", img, egui::TextureOptions::LINEAR)),
            }
        }

        ctx.request_repaint_after(Duration::from_millis(33));

        let is_connected = self.sender.is_connected.load(Ordering::Relaxed);
        let telem = self.sender.telemetry.read().unwrap().clone();

        egui::CentralPanel::default().show(ctx, |ui| {
            // ШАПКА: Статус соединения и телеметрия
            egui::Frame::new()
                .fill(Color32::from_rgb(26, 29, 36))
                .stroke(Stroke::new(1.0, Color32::from_rgb(46, 50, 62)))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(12.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let status_text = if is_connected {
                            "🟢 ПОДКЛЮЧЕНО К NANOCORD (127.0.0.1:48120)"
                        } else {
                            "🟡 ОЖИДАНИЕ ПОДКЛЮЧЕНИЯ NANOCORD (127.0.0.1:48120)..."
                        };
                        let col = if is_connected { Color32::from_rgb(35, 165, 90) } else { Color32::from_rgb(234, 179, 8) };
                        ui.label(egui::RichText::new(status_text).strong().size(13.0).color(col));

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let mb = telem.total_bytes as f32 / (1024.0 * 1024.0);
                            ui.label(egui::RichText::new(format!("{:.1} FPS • {:.1} Mbps • {:.1} MB", telem.fps, telem.bitrate_mbps, mb)).size(12.0).color(Color32::WHITE));
                        });
                    });
                });

            ui.add_space(10.0);

            // ПРЕВЬЮ ПОТОКОВ (Слева: Экран 1280x720, Справа: Камера 640x360)
            ui.horizontal(|ui| {
                let avail_w = ui.available_width();
                let dt_w = avail_w * 0.62;
                let cam_w = avail_w - dt_w - 10.0;
                let h = dt_w * (9.0 / 16.0);

                // Превью экрана
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new("Стрим рабочего стола (1280x720)").size(11.0).color(Color32::WHITE).strong());
                    let (dt_rect, _) = ui.allocate_exact_size(Vec2::new(dt_w, h), egui::Sense::hover());
                    let painter = ui.painter();
                    if let Some(ref tex) = self.desktop_tex {
                        painter.image(tex.id(), dt_rect, Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)), Color32::WHITE);
                    } else {
                        painter.rect_filled(dt_rect, CornerRadius::same(6), Color32::from_rgb(14, 16, 20));
                    }
                    painter.rect_stroke(dt_rect, CornerRadius::same(6), Stroke::new(1.0, Color32::from_rgb(60, 66, 80)), egui::StrokeKind::Inside);
                });

                // Превью веб-камеры
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new("Веб-камера (640x360)").size(11.0).color(Color32::WHITE).strong());
                    let (cam_rect, _) = ui.allocate_exact_size(Vec2::new(cam_w, h), egui::Sense::hover());
                    let painter = ui.painter();
                    if let Some(ref tex) = self.camera_tex {
                        painter.image(tex.id(), cam_rect, Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)), Color32::WHITE);
                    } else {
                        painter.rect_filled(cam_rect, CornerRadius::same(6), Color32::from_rgb(14, 16, 20));
                    }
                    painter.rect_stroke(cam_rect, CornerRadius::same(6), Stroke::new(1.0, Color32::from_rgb(60, 66, 80)), egui::StrokeKind::Inside);
                });
            });

            ui.add_space(12.0);

            // УПРАВЛЕНИЕ СКОРОСТЬЮ ТРАНСЛЯЦИИ (FPS)
            egui::Frame::new()
                .fill(Color32::from_rgb(26, 29, 36))
                .stroke(Stroke::new(1.0, Color32::from_rgb(46, 50, 62)))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(10.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Целевой FPS трансляции:").color(Color32::WHITE).strong());
                        let cur_fps = self.sender.target_fps.load(Ordering::Relaxed);
                        for &fps in &[30u32, 60u32, 120u32] {
                            let is_active = cur_fps == fps;
                            let btn_color = if is_active { Color32::from_rgb(88, 101, 242) } else { Color32::from_rgb(40, 44, 54) };
                            if ui.add(egui::Button::new(egui::RichText::new(format!("{} FPS", fps)).color(Color32::WHITE).strong()).fill(btn_color)).clicked() {
                                self.sender.target_fps.store(fps, Ordering::Relaxed);
                            }
                        }
                    });
                });

            ui.add_space(10.0);

            // ПЛЕЕР ФОНОВОЙ МУЗЫКИ (WINMM BGM)
            egui::Frame::new()
                .fill(Color32::from_rgb(26, 29, 36))
                .stroke(Stroke::new(1.0, Color32::from_rgb(46, 50, 62)))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(12.0)
                .show(ui, |ui| {
                    let mut player = self.sender.bgm_player.lock().unwrap();
                    let track_name = player.current_track_name();
                    let is_playing = player.is_playing;
                    let tracks_count = player.tracks.len();

                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("🎶 Фоновая музыка (Downloads):").color(Color32::WHITE).strong().size(13.0));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(egui::RichText::new(format!("Треков найдено: {}", tracks_count)).size(11.0).weak());
                        });
                    });

                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(format!("Сейчас играет: {}", track_name)).color(Color32::from_rgb(35, 165, 90)).strong().size(13.0));
                    ui.add_space(6.0);

                    // Кнопки управления плеером
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new("⏮ Предыдущий").min_size(Vec2::new(110.0, 28.0))).clicked() {
                            player.prev();
                        }
                        let play_label = if is_playing { "⏸ Пауза" } else { "▶ Воспроизведение" };
                        if ui.add(egui::Button::new(play_label).min_size(Vec2::new(130.0, 28.0))).clicked() {
                            player.toggle_pause();
                        }
                        if ui.add(egui::Button::new("⏭ Следующий").min_size(Vec2::new(110.0, 28.0))).clicked() {
                            player.next();
                        }
                    });

                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("Доступные треки в папке Downloads:").size(11.0).weak());

                    // Список треков для выбора
                    let mut chosen_track_idx = None;
                    egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                        let cur_idx = player.current_idx;
                        for (idx, (t_name, _)) in player.tracks.iter().enumerate() {
                            let is_cur = idx == cur_idx;
                            let text = if is_cur { format!("▶ {}", t_name) } else { format!("  {}", t_name) };
                            let color = if is_cur { Color32::from_rgb(88, 101, 242) } else { Color32::from_rgb(215, 220, 230) };
                            if ui.selectable_label(is_cur, egui::RichText::new(text).color(color)).clicked() {
                                chosen_track_idx = Some(idx);
                            }
                        }
                    });
                    if let Some(idx) = chosen_track_idx {
                        player.play_index(idx);
                    }
                });
        });
    }
}
