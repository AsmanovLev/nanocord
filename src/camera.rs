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
pub fn start_camera_worker() -> Receiver<CameraFrame> {
    let (tx, rx) = sync_channel(2);
    thread::spawn(move || {
        let index = CameraIndex::Index(0);
        let requested = RequestedFormat::new::<RgbAFormat>(RequestedFormatType::HighestResolution(
            Resolution::new(1280, 720),
        ));

        let mut camera = match Camera::new(index, requested) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[CAMERA] Camera::new error: {:?}", e);
                return;
            }
        };

        if let Err(e) = camera.open_stream() {
            eprintln!("[CAMERA] Failed to open camera stream: {:?}", e);
            return;
        }

        println!("[CAMERA] Webcam stream active: {}x{}", camera.resolution().width_x, camera.resolution().height_y);

        loop {
            thread::sleep(Duration::from_millis(33)); // ~30 FPS

            match camera.frame() {
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
                Err(e) => {
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
    });
    rx
}
