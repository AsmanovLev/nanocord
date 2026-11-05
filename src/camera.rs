#![allow(dead_code, unused_variables, unused_imports)]

use nokhwa::pixel_format::RgbAFormat;
use nokhwa::utils::{CameraIndex, RequestedFormat, RequestedFormatType, Resolution};
use nokhwa::Camera;
use std::sync::mpsc::{sync_channel, Receiver};
use std::thread;
use std::time::Duration;

#[derive(Clone)]
pub struct CameraFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Запускает отдельный поток захвата видео с веб-камеры (Media Foundation / nokhwa)
/// В случае отсутствия или блокировки аппаратной камеры вторым процессом,
/// генерирует мягкий виртуальный поток камеры для бесперебойного звонка.
pub fn start_camera_worker() -> Receiver<CameraFrame> {
    let (tx, rx) = sync_channel(2);
    thread::spawn(move || {
        let index = CameraIndex::Index(0);
        let requested = RequestedFormat::new::<RgbAFormat>(RequestedFormatType::HighestResolution(
            Resolution::new(1280, 720),
        ));

        let mut camera = Camera::new(index, requested).ok();
        if let Some(ref mut c) = camera {
            if c.open_stream().is_err() {
                camera = None;
            }
        }

        if let Some(mut cam) = camera {
            println!("[CAMERA] Hardware webcam active: {}x{}", cam.resolution().width_x, cam.resolution().height_y);
            loop {
                thread::sleep(Duration::from_millis(33)); // ~30 FPS

                match cam.frame() {
                    Ok(frame) => {
                        if let Ok(decoded) = frame.decode_image::<RgbAFormat>() {
                            let width = decoded.width();
                            let height = decoded.height();
                            let rgba = decoded.into_raw();
                            let _ = tx.try_send(CameraFrame {
                                width,
                                height,
                                rgba,
                            });
                        }
                    }
                    Err(_) => {
                        thread::sleep(Duration::from_millis(100));
                    }
                }
            }
        } else {
            println!("[CAMERA] Physical webcam not available or locked, running virtual camera pattern");
            let mut seq = 0u64;
            loop {
                thread::sleep(Duration::from_millis(33)); // 30 FPS
                seq += 1;
                let rgba = generate_virtual_camera_frame(640, 360, seq);
                let _ = tx.try_send(CameraFrame {
                    width: 640,
                    height: 360,
                    rgba,
                });
            }
        }
    });
    rx
}

fn generate_virtual_camera_frame(w: u32, h: u32, seq: u64) -> Vec<u8> {
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let w_usize = w as usize;
    let t = (seq as f32) * 0.05;
    let cx = (w / 2) as f32 + (t * 0.8).sin() * 15.0;
    let cy = (h / 2) as f32 + (t * 0.5).cos() * 8.0;
    let r_head = 45.0;
    let r_head_sq = r_head * r_head;

    for y in 0..h as usize {
        let dy = y as f32 - cy;
        let dy_sq = dy * dy;
        let row_offset = y * w_usize * 4;

        for x in 0..w_usize {
            let dx = x as f32 - cx;
            let dist_sq = dx * dx + dy_sq;
            let offset = row_offset + x * 4;

            if dist_sq < r_head_sq {
                // Силуэт аватара
                buf[offset] = 88;
                buf[offset + 1] = 101;
                buf[offset + 2] = 242;
                buf[offset + 3] = 255;
            } else if dist_sq < (r_head + 4.0) * (r_head + 4.0) {
                // Неоновый ободок
                buf[offset] = 180;
                buf[offset + 1] = 195;
                buf[offset + 2] = 255;
                buf[offset + 3] = 255;
            } else {
                // Темный фон камеры
                buf[offset] = 20;
                buf[offset + 1] = 23;
                buf[offset + 2] = 30;
                buf[offset + 3] = 255;
            }
        }
    }
    buf
}
