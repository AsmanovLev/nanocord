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

    /// Захватывает реальный кадр рабочего стола и конвертирует его в RGBA буфер нужного размера
    pub fn capture_preview_rgba(
        &mut self,
        target_w: u32,
        target_h: u32,
    ) -> Result<Option<Vec<u8>>> {
        unsafe {
            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;

            let hr = self.duplication.AcquireNextFrame(
                16,
                &mut frame_info,
                &mut resource,
            );

            if let Err(e) = hr {
                const DXGI_ERROR_WAIT_TIMEOUT: i32 = -2003292419; // 0x887A0027
                if e.code().0 == DXGI_ERROR_WAIT_TIMEOUT {
                    return Ok(None);
                }
                return Err(e);
            }

            let resource = match resource {
                Some(r) => r,
                None => return Ok(None),
            };
            let texture: ID3D11Texture2D = resource.cast()?;

            // Копируем захваченный кадр в staging-текстуру для чтения CPU
            self.context.CopyResource(&self.staging, &texture);

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context.Map(
                &self.staging,
                0,
                D3D11_MAP_READ,
                0,
                Some(&mut mapped),
            )?;

            let pitch = mapped.RowPitch as usize;
            let data_ptr = mapped.pData as *const u8;

            let src_w = self.width as usize;
            let src_h = self.height as usize;
            let dst_w = target_w as usize;
            let dst_h = target_h as usize;

            let mut out_rgba = vec![0u8; dst_w * dst_h * 4];

            // Быстрый 1:1 или билинейный пересчёт BGRA -> RGBA (максимальная резкость)
            if src_w == dst_w && src_h == dst_h {
                for dy in 0..dst_h {
                    let src_row = data_ptr.add(dy * pitch);
                    let dst_offset = dy * dst_w * 4;
                    for dx in 0..dst_w {
                        let px = src_row.add(dx * 4);
                        let b = *px;
                        let g = *px.add(1);
                        let r = *px.add(2);

                        let o = dst_offset + dx * 4;
                        out_rgba[o] = r;
                        out_rgba[o + 1] = g;
                        out_rgba[o + 2] = b;
                        out_rgba[o + 3] = 255;
                    }
                }
            } else {
                for dy in 0..dst_h {
                    let sy = (dy * src_h) / dst_h;
                    let src_row = data_ptr.add(sy * pitch);
                    let dst_offset = dy * dst_w * 4;

                    for dx in 0..dst_w {
                        let sx = (dx * src_w) / dst_w;
                        let px = src_row.add(sx * 4);
                        let b = *px;
                        let g = *px.add(1);
                        let r = *px.add(2);

                        let o = dst_offset + dx * 4;
                        out_rgba[o] = r;
                        out_rgba[o + 1] = g;
                        out_rgba[o + 2] = b;
                        out_rgba[o + 3] = 255;
                    }
                }
            }

            self.context.Unmap(&self.staging, 0);
            self.duplication.ReleaseFrame()?;

            Ok(Some(out_rgba))
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
                const DXGI_ERROR_WAIT_TIMEOUT: i32 = -2003292419; // 0x887A0027
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

/// Запускает фоновый поток захвата настоящего экрана для превью в UI (30 FPS)
pub fn start_desktop_preview_worker(
    target_w: u32,
    target_h: u32,
) -> std::sync::mpsc::Receiver<PreviewFrame> {
    let (tx, rx) = std::sync::mpsc::sync_channel(2);
    std::thread::spawn(move || {
        let mut capture = DxgiCapture::new().ok();
        let mut retry_counter = 0usize;
        let mut last_frame: Option<Vec<u8>> = None;

        loop {
            std::thread::sleep(std::time::Duration::from_millis(33)); // ~30 FPS
            let mut new_rgba = None;

            // Периодическая попытка подключить DXGI, если он был потерян
            if capture.is_none() {
                retry_counter += 1;
                if retry_counter % 30 == 0 {
                    capture = DxgiCapture::new().ok();
                }
            }

            if let Some(ref mut cap) = capture {
                match cap.capture_preview_rgba(target_w, target_h) {
                    Ok(Some(rgba)) => {
                        new_rgba = Some(rgba);
                    }
                    Ok(None) => {
                        // Экран статичен (DXGI timeout) — если нет сохраненного кадра, берем GDI
                        if last_frame.is_none() {
                            new_rgba = capture_desktop_gdi(target_w, target_h);
                        }
                    }
                    Err(_) => {
                        capture = None;
                    }
                }
            } else {
                // Если DXGI не работает, гарантированно захватываем через быстрый GDI
                new_rgba = capture_desktop_gdi(target_w, target_h);
            }

            if let Some(rgba) = new_rgba {
                last_frame = Some(rgba.clone());
                let _ = tx.try_send(PreviewFrame {
                    width: target_w,
                    height: target_h,
                    rgba,
                });
            } else if let Some(ref rgba) = last_frame {
                // При статичном экране повторно отдаем последний кадр, чтобы P2P-стрим не зависал в ожидании
                let _ = tx.try_send(PreviewFrame {
                    width: target_w,
                    height: target_h,
                    rgba: rgba.clone(),
                });
            }
        }
    });
    rx
}
