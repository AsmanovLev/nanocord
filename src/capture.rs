use windows::core::{Interface, Result};
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1,
    IDXGIOutputDuplication, IDXGIResource, DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HDC, HGDIOBJ, SRCCOPY,
};

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::latest::LatestSlot;

/// `DXGI_ERROR_WAIT_TIMEOUT` = 0x887A0027 — "the desktop did not change".
/// Written as hex so it cannot drift into a value that never matches: treating
/// a still desktop as an error is what pushes capture into the GDI fallback.
const DXGI_ERROR_WAIT_TIMEOUT: i32 = 0x887A0027u32 as i32;

/// Where a frame came from, carried on every frame so "periodic flicker" stops
/// being a guess: the moment the source flips to GDI, capture is degrading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameSource {
    Dxgi,
    Gdi,
}

impl FrameSource {
    pub fn label(self) -> &'static str {
        match self {
            FrameSource::Dxgi => "DXGI",
            FrameSource::Gdi => "GDI",
        }
    }

    pub fn as_u32(self) -> u32 {
        match self {
            FrameSource::Dxgi => 0,
            FrameSource::Gdi => 1,
        }
    }
}

/// Live capture counters for the diagnostics line.
#[derive(Default)]
pub struct CaptureStats {
    pub frames: AtomicU64,
    pub gdi_frames: AtomicU64,
    pub errors: AtomicU64,
    pub fps_x10: AtomicU32,
    pub source: AtomicU32,
    last_error: Mutex<String>,
}

impl CaptureStats {
    pub fn fps(&self) -> f32 {
        self.fps_x10.load(Ordering::Relaxed) as f32 / 10.0
    }

    pub fn source(&self) -> FrameSource {
        match self.source.load(Ordering::Relaxed) {
            1 => FrameSource::Gdi,
            _ => FrameSource::Dxgi,
        }
    }

    pub fn note_error(&self, error: &windows::core::Error) {
        self.errors.fetch_add(1, Ordering::Relaxed);
        let mut guard = self.last_error.lock().unwrap_or_else(|e| e.into_inner());
        *guard = format!("{:#010X} {}", error.code().0, error.message());
    }
}

/// An acquired frame that releases itself.
///
/// DXGI allows exactly one outstanding frame per duplication object. Any early
/// return that skipped `ReleaseFrame` made the next acquire fail with
/// `DXGI_ERROR_INVALID_CALL` and poisoned capture until it was rebuilt.
struct AcquiredFrame {
    duplication: IDXGIOutputDuplication,
    resource: IDXGIResource,
}

impl AcquiredFrame {
    fn texture(&self) -> Result<ID3D11Texture2D> {
        self.resource.cast()
    }
}

impl Drop for AcquiredFrame {
    fn drop(&mut self) {
        unsafe {
            let _ = self.duplication.ReleaseFrame();
        }
    }
}

/// Converts packed BGRA to RGBA while scaling with nearest neighbour.
fn scale_bgra_to_rgba(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let (sw, sh) = (sw.max(1) as usize, sh.max(1) as usize);
    let (dw, dh) = (dw.max(1) as usize, dh.max(1) as usize);
    let mut out = vec![0u8; dw * dh * 4];
    if src.len() < sw * sh * 4 {
        return out;
    }

    if sw == dw && sh == dh {
        for i in 0..dw * dh {
            let s = i * 4;
            out[s] = src[s + 2];
            out[s + 1] = src[s + 1];
            out[s + 2] = src[s];
            out[s + 3] = 255;
        }
        return out;
    }

    for dy in 0..dh {
        let sy = dy * sh / dh;
        let src_row = sy * sw * 4;
        let dst_row = dy * dw * 4;
        for dx in 0..dw {
            let sx = dx * sw / dw;
            let s = src_row + sx * 4;
            let d = dst_row + dx * 4;
            out[d] = src[s + 2];
            out[d + 1] = src[s + 1];
            out[d + 2] = src[s];
            out[d + 3] = 255;
        }
    }
    out
}

#[allow(dead_code)]
pub struct DxgiCapture {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub duplication: IDXGIOutputDuplication,
    pub staging: ID3D11Texture2D,
    pub width: u32,
    pub height: u32,
}

impl DxgiCapture {
    pub fn new() -> Result<Self> {
        unsafe {
            // 1. Ищем DXGI-адаптер, к которому физически подключен активный рабочий стол
            let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
            let mut capture_adapter: Option<IDXGIAdapter1> = None;
            let mut capture_output: Option<IDXGIOutput1> = None;

            for i in 0.. {
                let adapter = match factory.EnumAdapters1(i) {
                    Ok(a) => a,
                    Err(_) => break,
                };

                let output = match adapter.EnumOutputs(0) {
                    Ok(out) => out,
                    Err(_) => continue, // На этом адаптере нет монитора
                };

                let output1: IDXGIOutput1 = output.cast()?;
                let desc = output1.GetDesc()?;
                if desc.AttachedToDesktop.as_bool() {
                    let adapter_desc = adapter.GetDesc1()?;
                    println!(
                        "       -> Display Adapter: {}",
                        String::from_utf16_lossy(&adapter_desc.Description)
                    );
                    capture_adapter = Some(adapter);
                    capture_output = Some(output1);
                    break;
                }
            }

            let adapter = capture_adapter.expect("No active display adapter found");
            let output1 = capture_output.expect("No active desktop output found");

            let desc = output1.GetDesc()?;
            let width = (desc.DesktopCoordinates.right - desc.DesktopCoordinates.left) as u32;
            let height = (desc.DesktopCoordinates.bottom - desc.DesktopCoordinates.top) as u32;

            // 2. Создаем Direct3D 11 Device именно на адаптере с дисплеем
            let mut device: Option<ID3D11Device> = None;
            let mut context: Option<ID3D11DeviceContext> = None;
            let mut feature_level = D3D_FEATURE_LEVEL_11_0;

            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN, // Обязательно при передаче конкретного адаптера
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut feature_level),
                Some(&mut context),
            )?;

            let device = device.expect("Failed to create D3D11 device on display adapter");
            let context = context.expect("Failed to create D3D11 context on display adapter");

            // 3. Создаем Output Duplication
            let duplication = output1.DuplicateOutput(&device)?;

            // 4. Создаем staging текстуру для прямого считывания превью в CPU
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut staging: Option<ID3D11Texture2D> = None;
            device.CreateTexture2D(&staging_desc, None, Some(&mut staging))?;
            let staging = staging.expect("Failed to create staging texture");

            println!("DXGI Screen Capture initialized: {}x{}", width, height);

            Ok(Self {
                device,
                context,
                duplication,
                staging,
                width,
                height,
            })
        }
    }

    /// Acquires one frame, or `None` when the desktop has not changed.
    fn acquire(&self, timeout_ms: u32) -> Result<Option<AcquiredFrame>> {
        unsafe {
            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;

            match self
                .duplication
                .AcquireNextFrame(timeout_ms, &mut frame_info, &mut resource)
            {
                Ok(()) => match resource {
                    None => Ok(None),
                    Some(resource) => Ok(Some(AcquiredFrame {
                        duplication: self.duplication.clone(),
                        resource,
                    })),
                },
                Err(e) if e.code().0 == DXGI_ERROR_WAIT_TIMEOUT => Ok(None),
                Err(e) => Err(e),
            }
        }
    }

    /// Copies the current desktop into a packed BGRA buffer.
    ///
    /// `Ok(None)` means "nothing changed" — the normal state of a still desktop.
    pub fn grab(&mut self) -> Result<Option<Vec<u8>>> {
        let Some(frame) = self.acquire(16)? else {
            return Ok(None);
        };

        // `frame` releases itself on drop, including on every error path below.
        let texture = frame.texture()?;
        unsafe {
            self.context.CopyResource(&self.staging, &texture);
        }

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        }

        let pitch = mapped.RowPitch as usize;
        if mapped.pData.is_null() {
            unsafe {
                self.context.Unmap(&self.staging, 0);
            }
            return Ok(None);
        }
        let row_bytes = self.width as usize * 4;
        let mut bgra = vec![0u8; row_bytes * self.height as usize];
        unsafe {
            for y in 0..self.height as usize {
                let src = (mapped.pData as *const u8).add(y * pitch);
                let dst = bgra.as_mut_ptr().add(y * row_bytes);
                std::ptr::copy_nonoverlapping(src, dst, row_bytes);
            }
            self.context.Unmap(&self.staging, 0);
        }

        Ok(Some(bgra))
    }

    /// Захватывает реальный кадр рабочего стола и конвертирует его в RGBA буфер нужного размера
    pub fn capture_preview_rgba(
        &mut self,
        target_w: u32,
        target_h: u32,
    ) -> Result<Option<Vec<u8>>> {
        match self.grab()? {
            Some(bgra) => Ok(Some(scale_bgra_to_rgba(
                &bgra,
                self.width,
                self.height,
                target_w,
                target_h,
            ))),
            None => Ok(None),
        }
    }

    /// Захватывает следующий кадр экрана. Если картинка не изменилась, возвращает Ok(None).
    pub fn acquire_next_frame(
        &mut self,
        timeout_ms: u32,
    ) -> Result<Option<(ID3D11Texture2D, DXGI_OUTDUPL_FRAME_INFO)>> {
        unsafe {
            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;

            let hr = self.duplication.AcquireNextFrame(
                timeout_ms,
                &mut frame_info,
                &mut resource,
            );

            if let Err(e) = hr {
                // `DXGI_ERROR_WAIT_TIMEOUT` = 0x887A0027. Derived from hex so the
                // comparison cannot drift: "nothing changed" must not be treated
                // as a broken capture, or every still desktop falls back to GDI.
                const DXGI_ERROR_WAIT_TIMEOUT: i32 = 0x887A0027u32 as i32;
                if e.code().0 == DXGI_ERROR_WAIT_TIMEOUT {
                    return Ok(None);
                }
                return Err(e);
            }

            let resource = resource.expect("Valid resource expected on success");
            let texture: ID3D11Texture2D = resource.cast()?;

            Ok(Some((texture, frame_info)))
        }
    }

    /// Освобождает захваченный кадр
    pub fn release_frame(&self) -> Result<()> {
        unsafe {
            self.duplication.ReleaseFrame()?;
            Ok(())
        }
    }
}

/// Определяет физическое разрешение основного активного монитора
pub fn get_screen_resolution() -> (u32, u32) {
    unsafe {
        if let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() {
            for i in 0.. {
                let adapter = match factory.EnumAdapters1(i) {
                    Ok(a) => a,
                    Err(_) => break,
                };
                if let Ok(output) = adapter.EnumOutputs(0) {
                    if let Ok(output1) = output.cast::<IDXGIOutput1>() {
                        if let Ok(desc) = output1.GetDesc() {
                            if desc.AttachedToDesktop.as_bool() {
                                let width = (desc.DesktopCoordinates.right - desc.DesktopCoordinates.left) as u32;
                                let height = (desc.DesktopCoordinates.bottom - desc.DesktopCoordinates.top) as u32;
                                if width > 0 && height > 0 {
                                    return (width, height);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    (1920, 1080)
}

/// Возвращает физическую частоту обновления экрана в Гц (например, 144)
pub fn get_screen_refresh_rate() -> u32 {
    unsafe {
        let hdc = GetDC(None);
        if !hdc.0.is_null() {
            let refresh = windows::Win32::Graphics::Gdi::GetDeviceCaps(Some(hdc), windows::Win32::Graphics::Gdi::VREFRESH) as u32;
            ReleaseDC(None, hdc);
            if refresh > 0 {
                return refresh;
            }
        }
    }
    144
}

#[derive(Clone)]
pub struct PreviewFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub source: FrameSource,
    pub seq: u64,
}

/// Надёжный захват рабочего стола через Windows GDI (работает на любом железе без таймаутов)
pub fn capture_desktop_gdi(target_w: u32, target_h: u32) -> Option<Vec<u8>> {
    unsafe {
        let hdc_screen = GetDC(None);
        if hdc_screen.0.is_null() {
            return None;
        }

        let hdc_mem = CreateCompatibleDC(Some(hdc_screen));
        if hdc_mem.0.is_null() {
            ReleaseDC(None, hdc_screen);
            return None;
        }

        let (screen_w, screen_h) = get_screen_resolution();
        let hbm = CreateCompatibleBitmap(hdc_screen, screen_w as i32, screen_h as i32);
        if hbm.0.is_null() {
            let _ = DeleteDC(hdc_mem);
            ReleaseDC(None, hdc_screen);
            return None;
        }

        let old_bm = SelectObject(hdc_mem, HGDIOBJ(hbm.0));
        let _ = BitBlt(hdc_mem, 0, 0, screen_w as i32, screen_h as i32, Some(hdc_screen), 0, 0, SRCCOPY);
        // В Windows GDI перед вызовом GetDIBits битмап ОБЯЗАТЕЛЬНО должен быть отвязан от DC!
        SelectObject(hdc_mem, old_bm);

        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: screen_w as i32,
                biHeight: screen_h as i32, // Positive = Bottom-up DIB (GetDIBits requires positive height!)
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut src_bgra = vec![0u8; (screen_w * screen_h * 4) as usize];
        let lines = GetDIBits(
            hdc_screen,
            hbm,
            0,
            screen_h,
            Some(src_bgra.as_mut_ptr() as *mut _),
            &mut bi,
            DIB_RGB_COLORS,
        );

        let _ = DeleteObject(HGDIOBJ(hbm.0));
        let _ = DeleteDC(hdc_mem);
        ReleaseDC(None, hdc_screen);

        if lines == 0 {
            return None;
        }

        // Конвертация bottom-up BGRA -> top-down RGBA с масштабированием
        let src_w = screen_w as usize;
        let src_h = screen_h as usize;
        let dst_w = target_w as usize;
        let dst_h = target_h as usize;
        let mut out_rgba = vec![0u8; dst_w * dst_h * 4];

        for dy in 0..dst_h {
            // Маппим dy (сверху вниз 0..dst_h) на sy bottom-up буфера GDI:
            // В GDI строка 0 — это низ экрана, строка src_h - 1 — это верх экрана.
            let sy_top = (dy * src_h) / dst_h;
            let sy_bottom_up = src_h.saturating_sub(1 + sy_top);
            let src_row = sy_bottom_up * src_w * 4;
            let dst_row = dy * dst_w * 4;

            for dx in 0..dst_w {
                let sx = (dx * src_w) / dst_w;
                let s_idx = src_row + sx * 4;
                let d_idx = dst_row + dx * 4;

                let b = src_bgra[s_idx];
                let g = src_bgra[s_idx + 1];
                let r = src_bgra[s_idx + 2];

                out_rgba[d_idx] = r;
                out_rgba[d_idx + 1] = g;
                out_rgba[d_idx + 2] = b;
                out_rgba[d_idx + 3] = 255;
            }
        }

        Some(out_rgba)
    }
}

/// Запускает фоновый поток захвата рабочего стола.
///
/// `fps` общий с UI — ползунок действует без перезапуска. Кадр отдаётся только
/// когда рабочий стол изменился; GDI применяется лишь для холодного старта,
/// потому что под DWM он отдаёт чёрные и рваные кадры.
pub fn start_desktop_preview_worker(
    target_w: u32,
    target_h: u32,
    fps: Arc<AtomicU32>,
    is_streaming: Arc<std::sync::atomic::AtomicBool>,
    stats: Arc<CaptureStats>,
) -> Arc<LatestSlot<PreviewFrame>> {
    let slot = Arc::new(LatestSlot::new());
    let out = slot.clone();

    std::thread::spawn(move || {
        let mut capture: Option<DxgiCapture> = None;
        let mut next_attempt = Instant::now();
        let mut backoff = Duration::from_millis(250);
        let mut seq = 0u64;
        let mut last_source = FrameSource::Dxgi;
        let mut next_tick = Instant::now();
        let mut fps_window_start = Instant::now();
        let mut fps_window_frames = 0u32;

        loop {
            let streaming = is_streaming.load(Ordering::Relaxed);
            // 1 FPS in idle keeps the cost near zero without faking frames.
            let target_fps = if streaming {
                fps.load(Ordering::Relaxed).clamp(1, 120)
            } else {
                1
            };
            let period = Duration::from_nanos(1_000_000_000 / target_fps as u64);

            // Deadline pacing: sleeping `period` *before* the work yields
            // 1/(period + work), so the configured fps is never reached.
            next_tick += period;
            match next_tick.checked_duration_since(Instant::now()) {
                Some(wait) if !wait.is_zero() => std::thread::sleep(wait),
                _ => next_tick = Instant::now() + period,
            }

            if capture.is_none() && Instant::now() >= next_attempt {
                match DxgiCapture::new() {
                    Ok(c) => {
                        capture = Some(c);
                        backoff = Duration::from_millis(250);
                    }
                    Err(e) => {
                        stats.note_error(&e);
                        next_attempt = Instant::now() + backoff;
                        backoff = (backoff * 2).min(Duration::from_secs(2));
                    }
                }
            }

            let mut fresh: Option<(Vec<u8>, FrameSource)> = None;

            if let Some(cap) = capture.as_mut() {
                match cap.capture_preview_rgba(target_w, target_h) {
                    Ok(Some(rgba)) => fresh = Some((rgba, FrameSource::Dxgi)),
                    // Still desktop: nothing to send, nothing to repaint.
                    Ok(None) => {}
                    Err(e) => {
                        stats.note_error(&e);
                        // ACCESS_LOST is routine (UAC, mode change, exclusive
                        // fullscreen). Rebuild with a backoff; the UI keeps the
                        // last texture, so no black frame is shown meanwhile.
                        capture = None;
                        next_attempt = Instant::now() + backoff;
                        backoff = (backoff * 2).min(Duration::from_secs(2));
                    }
                }
            }

            // GDI only while there is nothing on screen at all (cold start).
            if fresh.is_none() && slot.is_empty() && capture.is_none() {
                if let Some(rgba) = capture_desktop_gdi(target_w, target_h) {
                    stats.gdi_frames.fetch_add(1, Ordering::Relaxed);
                    fresh = Some((rgba, FrameSource::Gdi));
                }
            }

            if let Some((rgba, source)) = fresh {
                seq += 1;
                stats.frames.fetch_add(1, Ordering::Relaxed);
                stats.source.store(source.as_u32(), Ordering::Relaxed);
                if source != last_source {
                    crate::diag::log(&format!("capture source: {}", source.label()));
                    last_source = source;
                }

                slot.put(PreviewFrame {
                    width: target_w,
                    height: target_h,
                    rgba,
                    source,
                    seq,
                });

                fps_window_frames += 1;
                if fps_window_start.elapsed() >= Duration::from_secs(1) {
                    let seconds = fps_window_start.elapsed().as_secs_f32().max(0.001);
                    let measured = fps_window_frames as f32 / seconds;
                    stats
                        .fps_x10
                        .store((measured * 10.0) as u32, Ordering::Relaxed);
                    fps_window_frames = 0;
                    fps_window_start = Instant::now();
                }
            }
        }
    });

    out
}
