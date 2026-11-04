use windows::core::{Interface, Result};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1,
    IDXGIOutputDuplication, IDXGIResource, DXGI_OUTDUPL_FRAME_INFO,
};

#[allow(dead_code)]
pub struct DxgiCapture {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub duplication: IDXGIOutputDuplication,
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

            println!("DXGI Screen Capture initialized: {}x{}", width, height);

            Ok(Self {
                device,
                context,
                duplication,
                width,
                height,
            })
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
