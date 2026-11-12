#![allow(non_snake_case, dead_code, non_camel_case_types)]

use std::ffi::c_void;
use std::mem::{size_of, zeroed, MaybeUninit};
use windows::core::{Interface, GUID, PCWSTR, Result};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ,
    D3D11_CPU_ACCESS_WRITE, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAP_WRITE_DISCARD, D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11_USAGE_DYNAMIC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

// Real NVIDIA Codec & Preset GUIDs
pub const NV_ENC_CODEC_H264_GUID: GUID = GUID::from_values(0x6bc82762, 0x4e63, 0x4ca4, [0xaa, 0x85, 0x1e, 0x50, 0xf3, 0x21, 0xf6, 0xbf]);
pub const NV_ENC_CODEC_HEVC_GUID: GUID = GUID::from_values(0x790cdc88, 0x4522, 0x4d7b, [0x94, 0x25, 0xbd, 0xa9, 0x97, 0x5f, 0x76, 0x03]);
pub const NV_ENC_PRESET_P1_GUID: GUID = GUID::from_values(0xfc0a8d3e, 0x45f8, 0x4cf8, [0x80, 0xc7, 0x29, 0x88, 0x71, 0x59, 0x0e, 0xbf]);
pub const NV_ENC_PRESET_P2_GUID: GUID = GUID::from_values(0xf581cfb8, 0x88d6, 0x4381, [0x93, 0xf0, 0xdf, 0x13, 0xf9, 0xc2, 0x7d, 0xab]);
pub const NV_ENC_PRESET_P3_GUID: GUID = GUID::from_values(0x36850110, 0x3a07, 0x441f, [0x94, 0xd5, 0x36, 0x70, 0x63, 0x1f, 0x91, 0xf6]);
pub const NV_ENC_PRESET_P4_GUID: GUID = GUID::from_values(0x90a7b826, 0xdf06, 0x4862, [0xb9, 0xd2, 0xcd, 0x6d, 0x73, 0xa0, 0x86, 0x81]);
pub const NV_ENC_PRESET_P5_GUID: GUID = GUID::from_values(0x21c6e6b4, 0x297a, 0x4cba, [0x99, 0x8f, 0xb6, 0xcb, 0xde, 0x72, 0xad, 0xe3]);
pub const NV_ENC_PRESET_P6_GUID: GUID = GUID::from_values(0x8e75c279, 0x6299, 0x4ab6, [0x83, 0x02, 0x0b, 0x21, 0x5a, 0x33, 0x5c, 0xf5]);
pub const NV_ENC_PRESET_P7_GUID: GUID = GUID::from_values(0x84848c12, 0x6f71, 0x4c13, [0x93, 0x1b, 0x53, 0xe2, 0x83, 0xf5, 0x79, 0x74]);

pub const NV_ENC_SUCCESS: u32 = 0;
pub const NV_ENC_DEVICE_TYPE_DIRECTX: u32 = 0;
pub const NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX: u32 = 0;
pub const NV_ENC_BUFFER_FORMAT_ARGB: u32 = 0x01000000;
pub const NV_ENC_BUFFER_USAGE_INPUT_IMAGE: u32 = 0;

pub const NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY: u32 = 3;
pub const NV_ENC_PARAMS_RC_CONSTQP: u32 = 0;
pub const NV_ENC_PARAMS_RC_VBR: u32 = 1;
pub const NV_ENC_PARAMS_RC_CBR: u32 = 2;
pub const NV_ENC_PIC_STRUCT_FRAME: u32 = 1;
pub const NV_ENC_PIC_FLAG_FORCEIDR: u32 = 0x00000004;

pub const NVENCAPI_VERSION: u32 = 0x0100000D;
pub const NV_ENCODE_API_FUNCTION_LIST_VER: u32 = 0x7102000D;
pub const NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER: u32 = 0x7101000D;
pub const NV_ENC_INITIALIZE_PARAMS_VER: u32 = 0xF107000D;
pub const NV_ENC_CONFIG_VER: u32 = 0xF109000D;
pub const NV_ENC_PRESET_CONFIG_VER: u32 = 0xF105000D;
pub const NV_ENC_RC_PARAMS_VER: u32 = 0x7101000D;
pub const NV_ENC_CREATE_BITSTREAM_BUFFER_VER: u32 = 0x7101000D;
pub const NV_ENC_REGISTER_RESOURCE_VER: u32 = 0x7105000D;
pub const NV_ENC_MAP_INPUT_RESOURCE_VER: u32 = 0x7104000D;
pub const NV_ENC_PIC_PARAMS_VER: u32 = 0xF107000D;
pub const NV_ENC_LOCK_BITSTREAM_VER: u32 = 0xF102000D;

#[repr(C)]
#[derive(Copy, Clone)]
pub struct NvEncApiFunctionList {
    pub version: u32,
    pub reserved: u32,
    pub nvEncOpenEncodeSession: *const c_void,
    pub nvEncGetEncodeGUIDCount: *const c_void,
    pub nvEncGetEncodeProfileGUIDCount: *const c_void,
    pub nvEncGetEncodeProfileGUIDs: *const c_void,
    pub nvEncGetEncodeGUIDs: *const c_void,
    pub nvEncGetInputFormatCount: *const c_void,
    pub nvEncGetInputFormats: *const c_void,
    pub nvEncGetEncodeCaps: *const c_void,
    pub nvEncGetEncodePresetCount: *const c_void,
    pub nvEncGetEncodePresetGUIDs: *const c_void,
    pub nvEncGetEncodePresetConfig: *const c_void,
    pub nvEncInitializeEncoder: unsafe extern "system" fn(*mut c_void, *mut NvEncInitializeParams) -> u32,
    pub nvEncCreateInputBuffer: *const c_void,
    pub nvEncDestroyInputBuffer: *const c_void,
    pub nvEncCreateBitstreamBuffer: unsafe extern "system" fn(*mut c_void, *mut NvEncCreateBitstreamBuffer) -> u32,
    pub nvEncDestroyBitstreamBuffer: unsafe extern "system" fn(*mut c_void, *mut c_void) -> u32,
    pub nvEncEncodePicture: unsafe extern "system" fn(*mut c_void, *mut NvEncPicParams) -> u32,
    pub nvEncLockBitstream: unsafe extern "system" fn(*mut c_void, *mut NvEncLockBitstream) -> u32,
    pub nvEncUnlockBitstream: unsafe extern "system" fn(*mut c_void, *mut c_void) -> u32,
    pub nvEncLockInputBuffer: *const c_void,
    pub nvEncUnlockInputBuffer: *const c_void,
    pub nvEncGetEncodeStats: *const c_void,
    pub nvEncGetSequenceParams: *const c_void,
    pub nvEncRegisterAsyncEvent: *const c_void,
    pub nvEncUnregisterAsyncEvent: *const c_void,
    pub nvEncMapInputResource: unsafe extern "system" fn(*mut c_void, *mut NvEncMapInputResource) -> u32,
    pub nvEncUnmapInputResource: unsafe extern "system" fn(*mut c_void, *mut c_void) -> u32,
    pub nvEncDestroyEncoder: unsafe extern "system" fn(*mut c_void) -> u32,
    pub nvEncInvalidateRefFrames: *const c_void,
    pub nvEncOpenEncodeSessionEx: unsafe extern "system" fn(*mut NvEncOpenEncodeSessionExParams, *mut *mut c_void) -> u32,
    pub nvEncRegisterResource: unsafe extern "system" fn(*mut c_void, *mut NvEncRegisterResource) -> u32,
    pub nvEncUnregisterResource: unsafe extern "system" fn(*mut c_void, *mut c_void) -> u32,
    pub nvEncReconfigureEncoder: *const c_void,
    pub reserved1: *const c_void,
    pub nvEncCreateMVBuffer: *const c_void,
    pub nvEncDestroyMVBuffer: *const c_void,
    pub nvEncRunMotionEstimationOnly: *const c_void,
    pub nvEncGetLastErrorString: *const c_void,
    pub nvEncSetIOCudaStreams: *const c_void,
    pub nvEncGetEncodePresetConfigEx: unsafe extern "system" fn(*mut c_void, GUID, GUID, u32, *mut NvEncPresetConfig) -> u32,
    pub nvEncGetSequenceParamEx: *const c_void,
    pub nvEncRestoreEncoderState: *const c_void,
    pub nvEncLookaheadPicture: *const c_void,
    pub reserved2: [*const c_void; 275],
}

#[repr(C)]
pub struct NvEncOpenEncodeSessionExParams {
    pub version: u32,
    pub deviceType: u32,
    pub device: *mut c_void,
    pub reserved: *mut c_void,
    pub apiVersion: u32,
    pub reserved1: [u32; 253],
    pub reserved2: [*mut c_void; 64],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct NvEncRcParams {
    pub version: u32,
    pub rateControlMode: u32,
    pub constQP: [u32; 3],
    pub averageBitRate: u32,
    pub maxBitRate: u32,
    pub vbvBufferSize: u32,
    pub vbvInitialDelay: u32,
    pub flags: u32,
    pub minQP: [u32; 3],
    pub maxQP: [u32; 3],
    pub initialRCQP: [u32; 3],
    pub temporallayerIdxMask: u32,
    pub temporalLayerInfo: [u32; 2],
    pub targetQuality: u8,
    pub targetQualityLSB: u8,
    pub lookaheadDepth: u16,
    pub lowDelayKeyFrameScale: u8,
    pub reserved_trailing: [u8; 35],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct NvEncConfig {
    pub version: u32,
    pub profileGUID: GUID,
    pub gopLength: u32,
    pub frameIntervalP: i32,
    pub monoChromeEncoding: u32,
    pub frameFieldMode: u32,
    pub mvPrecision: u32,
    pub rcParams: NvEncRcParams,
    pub encodeCodecConfig: [u8; 1792],
    pub reserved: [u32; 278],
    pub reserved2: [*mut c_void; 64],
}

#[repr(C)]
pub struct NvEncPresetConfig {
    pub version: u32,
    pub reserved: u32,
    pub presetCfg: NvEncConfig,
    pub reserved1: [u32; 256],
    pub reserved2: [*mut c_void; 64],
}

#[repr(C)]
pub struct NvEncInitializeParams {
    pub version: u32,
    pub encodeGUID: GUID,
    pub presetGUID: GUID,
    pub encodeWidth: u32,
    pub encodeHeight: u32,
    pub darWidth: u32,
    pub darHeight: u32,
    pub frameRateNum: u32,
    pub frameRateDen: u32,
    pub enableEncodeAsync: u32,
    pub enablePTD: u32,
    pub bitfields: u32,
    pub privDataSize: u32,
    pub reserved: u32,
    pub privData: *mut c_void,
    pub encodeConfig: *mut NvEncConfig,
    pub maxEncodeWidth: u32,
    pub maxEncodeHeight: u32,
    pub maxMEHintCountsPerBlock: [u8; 32],
    pub tuningInfo: u32,
    pub bufferFormat: u32,
    pub numStateBuffers: u32,
    pub outputStatsLevel: u32,
    pub reserved1: [u32; 284],
    pub reserved2: [*mut c_void; 64],
}

#[repr(C)]
pub struct NvEncCreateBitstreamBuffer {
    pub version: u32,
    pub size: u32,
    pub memoryHeap: u32,
    pub reserved: u32,
    pub bitstreamBuffer: *mut c_void,
    pub bitstreamBufferPtr: *mut c_void,
    pub reserved1: [u32; 58],
    pub reserved2: [*mut c_void; 64],
}

#[repr(C)]
pub struct NvEncRegisterResource {
    pub version: u32,
    pub resourceType: u32,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub subResourceIndex: u32,
    pub resourceToRegister: *mut c_void,
    pub registeredResource: *mut c_void,
    pub bufferFormat: u32,
    pub bufferUsage: u32,
    pub pInputFencePoint: *mut c_void,
    pub chromaOffset: [u32; 2],
    pub chromaOffsetIn: [u32; 2],
    pub reserved1: [u32; 244],
    pub reserved2: [*mut c_void; 61],
}

#[repr(C)]
pub struct NvEncMapInputResource {
    pub version: u32,
    pub subResourceIndex: u32,
    pub inputResource: *mut c_void,
    pub registeredResource: *mut c_void,
    pub mappedResource: *mut c_void,
    pub mappedBufferFmt: u32,
    pub reserved1: [u32; 251],
    pub reserved2: [*mut c_void; 63],
}

#[repr(C)]
pub struct NvEncPicParams {
    pub version: u32,
    pub inputWidth: u32,
    pub inputHeight: u32,
    pub inputPitch: u32,
    pub encodePicFlags: u32,
    pub frameIdx: u32,
    pub inputTimeStamp: u64,
    pub inputDuration: u64,
    pub inputBuffer: *mut c_void,
    pub outputBitstream: *mut c_void,
    pub completionEvent: *mut c_void,
    pub bufferFmt: u32,
    pub pictureStruct: u32,
    pub pictureType: u32,
    pub _pad0: u32,
    pub codecPicParams: [u8; 1544],
    pub meHintCountsPerBlock: [u8; 32],
    pub meExternalHints: *mut c_void,
    pub reserved2: [u32; 7],
    pub _pad1: u32,
    pub reserved5: [*mut c_void; 2],
    pub qpDeltaMap: *mut i8,
    pub qpDeltaMapSize: u32,
    pub reservedBitFields: u32,
    pub meHintRefPicDist: [u16; 2],
    pub diffPicNumHint: i32,
    pub alphaBuffer: *mut c_void,
    pub meExternalSbHints: *mut c_void,
    pub meSbHintsCount: u32,
    pub stateBufferIdx: u32,
    pub outputReconBuffer: *mut c_void,
    pub reserved3: [u32; 284],
    pub reserved6: [*mut c_void; 57],
}

#[repr(C)]
pub struct NvEncLockBitstream {
    pub version: u32,
    pub flags: u32,
    pub outputBitstream: *mut c_void,
    pub sliceOffsets: *mut u32,
    pub frameIdx: u32,
    pub hwEncodeStatus: u32,
    pub numSlices: u32,
    pub bitstreamSizeInBytes: u32,
    pub outputTimeStamp: u64,
    pub outputDuration: u64,
    pub bitstreamBufferPtr: *mut c_void,
    pub pictureType: u32,
    pub pictureStruct: u32,
    pub frameAvgQP: u32,
    pub frameSatd: u32,
    pub ltrFrameIdx: u32,
    pub ltrFrameBitmap: u32,
    pub temporalId: u32,
    pub intraMBCount: u32,
    pub interMBCount: u32,
    pub averageMVX: i32,
    pub averageMVY: i32,
    pub alphaLayerSizeInBytes: u32,
    pub outputStatsPtrSize: u32,
    pub reserved: u32,
    pub outputStatsPtr: *mut c_void,
    pub frameIdxDisplay: u32,
    pub reserved1: [u32; 219],
    pub reserved2: [*mut c_void; 63],
    pub reservedInternal: [u32; 8],
}

// Compile-time static assertions for exact C struct sizes
const _: () = {
    assert!(size_of::<NvEncApiFunctionList>() == 2552);
    assert!(size_of::<NvEncOpenEncodeSessionExParams>() == 1552);
    assert!(size_of::<NvEncInitializeParams>() == 1800);
    assert!(size_of::<NvEncConfig>() == 3584);
    assert!(size_of::<NvEncPresetConfig>() == 5128);
    assert!(size_of::<NvEncRcParams>() == 128);
    assert!(size_of::<NvEncCreateBitstreamBuffer>() == 776);
    assert!(size_of::<NvEncRegisterResource>() == 1536);
    assert!(size_of::<NvEncMapInputResource>() == 1544);
    assert!(size_of::<NvEncPicParams>() == 3360);
    assert!(size_of::<NvEncLockBitstream>() == 1544);
};

type NvEncodeAPICreateInstanceFn = unsafe extern "system" fn(*mut NvEncApiFunctionList) -> u32;

pub struct NvencEncoder {
    api: NvEncApiFunctionList,
    encoder: *mut c_void,
    bitstream_buffer: *mut c_void,
    registered_resource: *mut c_void,
    width: u32,
    height: u32,
    frame_count: u32,
    pub nvidia_device: ID3D11Device,
    pub nvidia_context: ID3D11DeviceContext,
    opt_staging_tex: Option<ID3D11Texture2D>,
    opt_nv_tex: Option<ID3D11Texture2D>,
    is_cross_adapter: bool,
}

impl NvencEncoder {
    pub fn new(
        capture_device: &ID3D11Device,
        width: u32,
        height: u32,
        cq: u8,
        max_bitrate_mbps: u32,
    ) -> Result<Self> {
        Self::new_with_preset_and_fps(
            capture_device,
            width,
            height,
            cq,
            max_bitrate_mbps,
            60,
            NV_ENC_PRESET_P1_GUID,
        )
    }

    pub fn new_with_preset(
        capture_device: &ID3D11Device,
        width: u32,
        height: u32,
        cq: u8,
        max_bitrate_mbps: u32,
        preset_guid: GUID,
    ) -> Result<Self> {
        Self::new_with_preset_and_fps(
            capture_device,
            width,
            height,
            cq,
            max_bitrate_mbps,
            60,
            preset_guid,
        )
    }

    pub fn new_h264(
        capture_device: &ID3D11Device,
        width: u32,
        height: u32,
        cq: u8,
        max_bitrate_mbps: u32,
        fps: u32,
        preset_guid: GUID,
    ) -> Result<Self> {
        Self::new_with_codec_preset_and_fps(
            capture_device,
            width,
            height,
            cq,
            max_bitrate_mbps,
            fps,
            NV_ENC_CODEC_H264_GUID,
            preset_guid,
        )
    }

    /// Автономный конструктор NVENC HEVC (H.265) энкодера с собственным контекстом NVIDIA D3D11
    pub fn new_standalone_hevc(
        width: u32,
        height: u32,
        cq: u8,
        max_bitrate_mbps: u32,
        fps: u32,
        preset_guid: GUID,
    ) -> Result<Self> {
        Self::new_standalone_codec(width, height, cq, max_bitrate_mbps, fps, NV_ENC_CODEC_HEVC_GUID, preset_guid)
    }

    /// Автономный конструктор NVENC H.264 энкодера с собственным контекстом NVIDIA D3D11
    pub fn new_standalone_h264(
        width: u32,
        height: u32,
        cq: u8,
        max_bitrate_mbps: u32,
        fps: u32,
        preset_guid: GUID,
    ) -> Result<Self> {
        Self::new_standalone_codec(width, height, cq, max_bitrate_mbps, fps, NV_ENC_CODEC_H264_GUID, preset_guid)
    }

    pub fn new_standalone_codec(
        width: u32,
        height: u32,
        cq: u8,
        max_bitrate_mbps: u32,
        fps: u32,
        codec_guid: GUID,
        preset_guid: GUID,
    ) -> Result<Self> {
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
            let mut nvidia_adapter: Option<IDXGIAdapter1> = None;
            for i in 0.. {
                let adapter = match factory.EnumAdapters1(i) {
                    Ok(a) => a,
                    Err(_) => break,
                };
                let desc = adapter.GetDesc1()?;
                if desc.VendorId == 0x10DE {
                    nvidia_adapter = Some(adapter);
                    break;
                }
            }
            let nvidia_adapter = nvidia_adapter.ok_or_else(|| windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259)))?;

            let mut dev_nv: Option<ID3D11Device> = None;
            let mut ctx_nv: Option<ID3D11DeviceContext> = None;
            let mut feature_level = D3D_FEATURE_LEVEL_11_0;
            D3D11CreateDevice(
                &nvidia_adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut dev_nv),
                Some(&mut feature_level),
                Some(&mut ctx_nv),
            )?;
            crate::diag::log("nvenc: D3D11 device created on NVIDIA adapter");
            let (nvidia_device, nvidia_context) = match (dev_nv, ctx_nv) {
                (Some(d), Some(c)) => (d, c),
                _ => {
                    crate::diag::log("nvenc: D3D11 device creation returned empty");
                    return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259)));
                }
            };

            // CPU пишет в dynamic staging, затем CopyResource в DEFAULT-текстуру.
            // Прямой Map в зарегистрированный ресурс NVENC не принимает:
            // Map возвращает E_FAIL, и кадр не кодируется вообще.
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DYNAMIC,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                MiscFlags: 0,
            };
            let mut st = None;
            nvidia_device.CreateTexture2D(&staging_desc, None, Some(&mut st))?;
            let staging_tex = st.ok_or_else(|| {
                windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259))
            })?;
            crate::diag::log("nvenc: staging texture created");

            let nv_desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut nt = None;
            nvidia_device.CreateTexture2D(&nv_desc, None, Some(&mut nt))?;
            let opt_nv_tex = nt.ok_or_else(|| {
                windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259))
            })?;
            crate::diag::log("nvenc: encoder input texture created (DEFAULT)");

            let dll_name: Vec<u16> = "nvEncodeAPI64.dll\0".encode_utf16().collect();
            let h_module = LoadLibraryW(PCWSTR(dll_name.as_ptr()))?;

            let create_instance_name = b"NvEncodeAPICreateInstance\0";
            let create_instance_proc = GetProcAddress(
                h_module,
                windows::core::PCSTR(create_instance_name.as_ptr()),
            ).ok_or_else(|| windows::core::Error::from_thread())?;

            let create_instance: NvEncodeAPICreateInstanceFn = std::mem::transmute(create_instance_proc);

            crate::diag::log("nvenc: dll loaded, creating API instance");
            let mut api_uninit = MaybeUninit::<NvEncApiFunctionList>::zeroed();
            let api_ptr = api_uninit.as_mut_ptr();
            (*api_ptr).version = NV_ENCODE_API_FUNCTION_LIST_VER;

            let status = create_instance(api_ptr);
            crate::diag::log(&format!("nvenc: CreateInstance status={}", status));
            if status != NV_ENC_SUCCESS {
                return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259)));
            }
            let api = api_uninit.assume_init();

            let mut open_session: NvEncOpenEncodeSessionExParams = zeroed();
            open_session.version = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER;
            open_session.deviceType = NV_ENC_DEVICE_TYPE_DIRECTX;
            open_session.device = nvidia_device.as_raw() as *mut c_void;
            open_session.apiVersion = NVENCAPI_VERSION;

            let mut encoder: *mut c_void = std::ptr::null_mut();
            let status = (api.nvEncOpenEncodeSessionEx)(&mut open_session, &mut encoder);
            if status != NV_ENC_SUCCESS {
                return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259)));
            }

            let mut preset_config: NvEncPresetConfig = zeroed();
            preset_config.version = NV_ENC_PRESET_CONFIG_VER;
            preset_config.presetCfg.version = NV_ENC_CONFIG_VER;

            let status = (api.nvEncGetEncodePresetConfigEx)(
                encoder,
                codec_guid,
                preset_guid,
                NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
                &mut preset_config,
            );
            let mut encode_config = if status == NV_ENC_SUCCESS {
                let mut cfg = preset_config.presetCfg;
                cfg.rcParams.rateControlMode = NV_ENC_PARAMS_RC_VBR;
                cfg.rcParams.targetQuality = cq;
                cfg.rcParams.targetQualityLSB = 0;
                cfg.rcParams.maxBitRate = max_bitrate_mbps * 1_000_000;
                cfg.rcParams.averageBitRate = max_bitrate_mbps * 1_000_000;
                cfg.rcParams.vbvBufferSize = (max_bitrate_mbps * 1_000_000) / fps.max(1);
                cfg.rcParams.flags |= 1 << 9; // zeroReorderDelay
                cfg.gopLength = 0xFFFFFFFF;
                cfg.frameIntervalP = 1;
                Some(cfg)
            } else {
                None
            };

            let mut init_params: NvEncInitializeParams = zeroed();
            init_params.version = NV_ENC_INITIALIZE_PARAMS_VER;
            init_params.encodeGUID = codec_guid;
            init_params.presetGUID = preset_guid;
            init_params.tuningInfo = NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY;
            init_params.encodeWidth = width;
            init_params.encodeHeight = height;
            init_params.darWidth = width;
            init_params.darHeight = height;
            init_params.frameRateNum = fps;
            init_params.frameRateDen = 1;
            init_params.enablePTD = 1;
            init_params.encodeConfig = encode_config.as_mut().map(|c| c as *mut _).unwrap_or(std::ptr::null_mut());

            let status = (api.nvEncInitializeEncoder)(encoder, &mut init_params);
            crate::diag::log(&format!("nvenc: InitializeEncoder status={}", status));
            if status != NV_ENC_SUCCESS {
                return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259)));
            }

            let mut bitstream_params: NvEncCreateBitstreamBuffer = zeroed();
            bitstream_params.version = NV_ENC_CREATE_BITSTREAM_BUFFER_VER;
            bitstream_params.size = 4 * 1024 * 1024;
            let status = (api.nvEncCreateBitstreamBuffer)(encoder, &mut bitstream_params);
            if status != NV_ENC_SUCCESS {
                return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259)));
            }

            let nt = &opt_nv_tex;
            let mut reg_res: NvEncRegisterResource = zeroed();
            reg_res.version = NV_ENC_REGISTER_RESOURCE_VER;
            reg_res.resourceType = NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX;
            reg_res.width = width;
            reg_res.height = height;
            reg_res.resourceToRegister = nt.as_raw() as *mut c_void;
            reg_res.bufferFormat = NV_ENC_BUFFER_FORMAT_ARGB;
            reg_res.bufferUsage = NV_ENC_BUFFER_USAGE_INPUT_IMAGE;

            let status = (api.nvEncRegisterResource)(encoder, &mut reg_res);
            crate::diag::log(&format!("nvenc: RegisterResource status={}", status));
            if status != NV_ENC_SUCCESS {
                return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259)));
            }

            Ok(Self {
                api,
                encoder,
                bitstream_buffer: bitstream_params.bitstreamBuffer,
                registered_resource: reg_res.registeredResource,
                width,
                height,
                frame_count: 0,
                nvidia_device,
                nvidia_context,
                opt_staging_tex: Some(staging_tex),
                opt_nv_tex: Some(opt_nv_tex),
                is_cross_adapter: true,
            })
        }
    }

    pub fn new_with_preset_and_fps(
        capture_device: &ID3D11Device,
        width: u32,
        height: u32,
        cq: u8,
        max_bitrate_mbps: u32,
        fps: u32,
        preset_guid: GUID,
    ) -> Result<Self> {
        Self::new_with_codec_preset_and_fps(
            capture_device,
            width,
            height,
            cq,
            max_bitrate_mbps,
            fps,
            NV_ENC_CODEC_HEVC_GUID,
            preset_guid,
        )
    }

    pub fn new_with_codec_preset_and_fps(
        capture_device: &ID3D11Device,
        width: u32,
        height: u32,
        cq: u8,
        max_bitrate_mbps: u32,
        fps: u32,
        codec_guid: GUID,
        preset_guid: GUID,
    ) -> Result<Self> {
        unsafe {
            // 1. Находим адаптер NVIDIA (VendorId == 0x10DE)
            let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
            let mut nvidia_adapter: Option<IDXGIAdapter1> = None;

            for i in 0.. {
                let adapter = match factory.EnumAdapters1(i) {
                    Ok(a) => a,
                    Err(_) => break,
                };
                let desc = adapter.GetDesc1()?;
                if desc.VendorId == 0x10DE {
                    println!("Found NVIDIA GPU: {}", String::from_utf16_lossy(&desc.Description));
                    nvidia_adapter = Some(adapter);
                    break;
                }
            }

            crate::diag::log("nvenc: adapter search done");
            let nvidia_adapter = match nvidia_adapter {
                Some(a) => a,
                None => {
                    crate::diag::log("nvenc: no NVIDIA adapter");
                    return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259)));
                }
            };

            // Проверяем, на каком адаптере работает захват
            let capture_dxgi: IDXGIDevice = capture_device.cast()?;
            let capture_adapter = capture_dxgi.GetAdapter()?;
            let capture_desc = capture_adapter.GetDesc()?;
            let is_cross_adapter = capture_desc.VendorId != 0x10DE;

            if is_cross_adapter {
                println!(
                    "Optimus Laptop detected: Screen is on {}, NVENC is on NVIDIA RTX 3060",
                    String::from_utf16_lossy(&capture_desc.Description)
                );
            }

            // 2. Создаем D3D11 Device на NVIDIA GPU
            let mut dev_nv: Option<ID3D11Device> = None;
            let mut ctx_nv: Option<ID3D11DeviceContext> = None;
            let mut feature_level = D3D_FEATURE_LEVEL_11_0;

            D3D11CreateDevice(
                &nvidia_adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut dev_nv),
                Some(&mut feature_level),
                Some(&mut ctx_nv),
            )?;

            crate::diag::log("nvenc: D3D11 device created on NVIDIA adapter");
            let (nvidia_device, nvidia_context) = match (dev_nv, ctx_nv) {
                (Some(d), Some(c)) => (d, c),
                _ => {
                    crate::diag::log("nvenc: D3D11 device creation returned empty");
                    return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259)));
                }
            };

            let dxgi_dev: IDXGIDevice = nvidia_device.cast()?;
            let actual_adapter = dxgi_dev.GetAdapter()?;
            let actual_desc = actual_adapter.GetDesc()?;
            println!(
                "       -> Actual adapter for nvidia_device: {} (VendorId: 0x{:04X})",
                String::from_utf16_lossy(&actual_desc.Description),
                actual_desc.VendorId
            );
            println!("       -> Device pointer: {:p}", nvidia_device.as_raw());

            // 3. Загружаем nvEncodeAPI64.dll и инициализируем API
            let dll_name: Vec<u16> = "nvEncodeAPI64.dll\0".encode_utf16().collect();
            let h_module = LoadLibraryW(PCWSTR(dll_name.as_ptr()))?;

            let create_instance_name = b"NvEncodeAPICreateInstance\0";
            let create_instance_proc = GetProcAddress(
                h_module,
                windows::core::PCSTR(create_instance_name.as_ptr()),
            ).ok_or_else(|| windows::core::Error::from_thread())?;

            let create_instance: NvEncodeAPICreateInstanceFn = std::mem::transmute(create_instance_proc);

            crate::diag::log("nvenc: dll loaded, creating API instance");
            let mut api_uninit = MaybeUninit::<NvEncApiFunctionList>::zeroed();
            let api_ptr = api_uninit.as_mut_ptr();
            (*api_ptr).version = NV_ENCODE_API_FUNCTION_LIST_VER;

            let status = create_instance(api_ptr);
            crate::diag::log(&format!("nvenc: CreateInstance status={}", status));
            if status != NV_ENC_SUCCESS {
                return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259)));
            }
            let api = api_uninit.assume_init();

            // 4. Открываем сессию кодирования с NVIDIA D3D11 Device
            let mut session_params: NvEncOpenEncodeSessionExParams = zeroed();
            session_params.version = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER;
            session_params.deviceType = NV_ENC_DEVICE_TYPE_DIRECTX;
            session_params.device = nvidia_device.as_raw() as *mut c_void;
            session_params.apiVersion = NVENCAPI_VERSION;

            let mut encoder: *mut c_void = std::ptr::null_mut();
            crate::diag::log(&format!("nvenc: OpenSessionEx device={:?} size={}", session_params.device, std::mem::size_of::<NvEncOpenEncodeSessionExParams>()));
            let status = (api.nvEncOpenEncodeSessionEx)(&mut session_params, &mut encoder);
            crate::diag::log(&format!("nvenc: OpenSessionEx status={} handle={:?}", status, encoder));
            if status != NV_ENC_SUCCESS {
                return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259)));
            }

            // 5. Конфигурация пресета и CQ-VBR
            let mut preset_config: NvEncPresetConfig = zeroed();
            preset_config.version = NV_ENC_PRESET_CONFIG_VER;
            preset_config.presetCfg.version = NV_ENC_CONFIG_VER;

            let status = (api.nvEncGetEncodePresetConfigEx)(
                encoder,
                codec_guid,
                preset_guid,
                NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
                &mut preset_config,
            );

            let mut encode_config = if status == NV_ENC_SUCCESS {
                let mut cfg = preset_config.presetCfg;
                cfg.rcParams.rateControlMode = NV_ENC_PARAMS_RC_VBR;
                cfg.rcParams.targetQuality = cq;
                cfg.rcParams.targetQualityLSB = 0;
                cfg.rcParams.maxBitRate = max_bitrate_mbps * 1_000_000;
                cfg.rcParams.averageBitRate = max_bitrate_mbps * 1_000_000;
                cfg.rcParams.vbvBufferSize = (max_bitrate_mbps * 1_000_000) / fps.max(1);
                cfg.rcParams.flags |= 1 << 9; // zeroReorderDelay
                cfg.gopLength = 0xFFFFFFFF; // infinite GOP (I-frames on demand)
                cfg.frameIntervalP = 1;     // no B-frames
                Some(cfg)
            } else {
                None
            };

            let mut init_params: NvEncInitializeParams = zeroed();
            init_params.version = NV_ENC_INITIALIZE_PARAMS_VER;
            init_params.encodeGUID = codec_guid;
            init_params.presetGUID = preset_guid;
            init_params.tuningInfo = NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY;
            init_params.encodeWidth = width;
            init_params.encodeHeight = height;
            init_params.darWidth = width;
            init_params.darHeight = height;
            init_params.frameRateNum = fps;
            init_params.frameRateDen = 1;
            init_params.enablePTD = 1;
            init_params.encodeConfig = encode_config.as_mut().map(|c| c as *mut _).unwrap_or(std::ptr::null_mut());

            let status = (api.nvEncInitializeEncoder)(encoder, &mut init_params);
            crate::diag::log(&format!("nvenc: InitializeEncoder status={}", status));
            if status != NV_ENC_SUCCESS {
                return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259)));
            }

            // 7. Создаем буфер для выходного битстрима (4 МБ)
            let mut bitstream_params: NvEncCreateBitstreamBuffer = zeroed();
            bitstream_params.version = NV_ENC_CREATE_BITSTREAM_BUFFER_VER;
            bitstream_params.size = 4 * 1024 * 1024;

            let status = (api.nvEncCreateBitstreamBuffer)(encoder, &mut bitstream_params);
            if status != NV_ENC_SUCCESS {
                return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259)));
            }

            // 8. Путь загрузки кадров.
            // NVENC читает обычную DEFAULT-текстуру. Если писать прямо в
            // зарегистрированный ресурс (USAGE_DYNAMIC), часть драйверов
            // отдаёт устаревшую или недописанную память — на экране это
            // выглядит рваными полосами. Поэтому пишем в dynamic staging и
            // копируем в текстуру, которую реально кодирует NVENC.
            let mut opt_staging_tex = None;
            let mut opt_nv_tex = None;
            let mut registered_resource: *mut c_void = std::ptr::null_mut();

            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DYNAMIC,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                MiscFlags: 0,
            };
            let mut st = None;
            nvidia_device.CreateTexture2D(&staging_desc, None, Some(&mut st))?;
            let staging_tex = st.ok_or_else(|| {
                windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259))
            })?;
            crate::diag::log("nvenc: staging texture created");

            let nv_desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut nt = None;
            nvidia_device.CreateTexture2D(&nv_desc, None, Some(&mut nt))?;
            let nt = nt.ok_or_else(|| {
                windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259))
            })?;
            crate::diag::log("nvenc: encoder input texture created (DEFAULT)");

            let mut reg_res: NvEncRegisterResource = zeroed();
            reg_res.version = NV_ENC_REGISTER_RESOURCE_VER;
            reg_res.resourceType = NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX;
            reg_res.width = width;
            reg_res.height = height;
            reg_res.resourceToRegister = nt.as_raw() as *mut c_void;
            reg_res.bufferFormat = NV_ENC_BUFFER_FORMAT_ARGB;
            reg_res.bufferUsage = NV_ENC_BUFFER_USAGE_INPUT_IMAGE;

            let status = (api.nvEncRegisterResource)(encoder, &mut reg_res);
            crate::diag::log(&format!("nvenc: RegisterResource status={}", status));
            if status != NV_ENC_SUCCESS {
                return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259)));
            }
            registered_resource = reg_res.registeredResource;
            opt_staging_tex = Some(staging_tex);
            opt_nv_tex = Some(nt);

            println!(
                "NVENC H.265 (HEVC) Encoder initialized! {}x{} @ {} FPS, CQ: {}, Max: {} Mbps",
                width, height, fps, cq, max_bitrate_mbps
            );

            Ok(Self {
                api,
                encoder,
                bitstream_buffer: bitstream_params.bitstreamBuffer,
                registered_resource,
                width,
                height,
                frame_count: 0,
                nvidia_device,
                nvidia_context,
                opt_staging_tex,
                opt_nv_tex,
                is_cross_adapter,
            })
        }
    }

    /// Кодирует захваченный кадр в NAL-поток H.265.
    pub fn encode_texture(
        &mut self,
        capture_texture: &ID3D11Texture2D,
        capture_context: &ID3D11DeviceContext,
        force_idr: bool,
    ) -> Result<Vec<u8>> {
        unsafe {
            let mut reg_to_cleanup: *mut c_void = std::ptr::null_mut();
            let registered_res = if self.is_cross_adapter {
                let staging = self.opt_staging_tex.as_ref().unwrap();
                let nv_tex = self.opt_nv_tex.as_ref().unwrap();

                // 1. Копируем кадр на capture GPU в staging
                capture_context.CopyResource(staging, capture_texture);

                // 2. Map staging для чтения
                let mut mapped_src = D3D11_MAPPED_SUBRESOURCE::default();
                capture_context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped_src))?;

                // 3. Map nv_tex для записи
                let mut mapped_dst = D3D11_MAPPED_SUBRESOURCE::default();
                self.nvidia_context.Map(nv_tex, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped_dst))?;

                // 4. Построчный быстрый memcpy
                let row_bytes = self.width as usize * 4;
                for y in 0..self.height as usize {
                    let src_ptr = (mapped_src.pData as *const u8).add(y * mapped_src.RowPitch as usize);
                    let dst_ptr = (mapped_dst.pData as *mut u8).add(y * mapped_dst.RowPitch as usize);
                    std::ptr::copy_nonoverlapping(src_ptr, dst_ptr, row_bytes);
                }

                self.nvidia_context.Unmap(nv_tex, 0);
                capture_context.Unmap(staging, 0);

                self.registered_resource
            } else {
                // Прямой zero-copy режим (одинаковый GPU)
                let mut reg_res: NvEncRegisterResource = zeroed();
                reg_res.version = NV_ENC_REGISTER_RESOURCE_VER;
                reg_res.resourceType = NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX;
                reg_res.width = self.width;
                reg_res.height = self.height;
                reg_res.resourceToRegister = capture_texture.as_raw() as *mut c_void;
                reg_res.bufferFormat = NV_ENC_BUFFER_FORMAT_ARGB;
                reg_res.bufferUsage = NV_ENC_BUFFER_USAGE_INPUT_IMAGE;

                let status = (self.api.nvEncRegisterResource)(self.encoder, &mut reg_res);
                if status != NV_ENC_SUCCESS {
                    return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259)));
                }
                reg_to_cleanup = reg_res.registeredResource;
                reg_res.registeredResource
            };

            // Мапим ресурс для чтения энкодером
            let mut map_res: NvEncMapInputResource = zeroed();
            map_res.version = NV_ENC_MAP_INPUT_RESOURCE_VER;
            map_res.registeredResource = registered_res;

            let status = (self.api.nvEncMapInputResource)(self.encoder, &mut map_res);
            if status != NV_ENC_SUCCESS {
                if !reg_to_cleanup.is_null() {
                    (self.api.nvEncUnregisterResource)(self.encoder, reg_to_cleanup);
                }
                return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259)));
            }

            // Отправляем кадр на кодирование
            let mut pic_params: NvEncPicParams = zeroed();
            pic_params.version = NV_ENC_PIC_PARAMS_VER;
            pic_params.inputWidth = self.width;
            pic_params.inputHeight = self.height;
            pic_params.inputBuffer = map_res.mappedResource;
            pic_params.outputBitstream = self.bitstream_buffer;
            pic_params.bufferFmt = NV_ENC_BUFFER_FORMAT_ARGB;
            pic_params.pictureStruct = NV_ENC_PIC_STRUCT_FRAME;
            pic_params.frameIdx = self.frame_count;
            if force_idr {
                pic_params.encodePicFlags |= NV_ENC_PIC_FLAG_FORCEIDR;
            }

            let status = (self.api.nvEncEncodePicture)(self.encoder, &mut pic_params);
            (self.api.nvEncUnmapInputResource)(self.encoder, map_res.mappedResource);
            if !reg_to_cleanup.is_null() {
                (self.api.nvEncUnregisterResource)(self.encoder, reg_to_cleanup);
            }
            if status != NV_ENC_SUCCESS {
                return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259)));
            }

            // Считываем сжатый NAL битстрим
            let mut lock_params: NvEncLockBitstream = zeroed();
            lock_params.version = NV_ENC_LOCK_BITSTREAM_VER;
            lock_params.outputBitstream = self.bitstream_buffer;
            lock_params.flags = 0; // doNotWait = 0

            let status = (self.api.nvEncLockBitstream)(self.encoder, &mut lock_params);
            if status != NV_ENC_SUCCESS {
                return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259)));
            }

            let byte_slice = std::slice::from_raw_parts(
                lock_params.bitstreamBufferPtr as *const u8,
                lock_params.bitstreamSizeInBytes as usize,
            );
            let encoded_bytes = byte_slice.to_vec();

            (self.api.nvEncUnlockBitstream)(self.encoder, self.bitstream_buffer);

            self.frame_count += 1;
            Ok(encoded_bytes)
        }
    }

    /// Быстрое кодирование сырых RGBA-байт кадра в поток H.264 NAL через NVENC
    pub fn encode_rgba_bytes(&mut self, rgba: &[u8], force_idr: bool) -> Result<Vec<u8>> {
        unsafe {
            let nv_tex = match self.opt_nv_tex.as_ref() {
                Some(t) => t,
                None => return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259))),
            };

            let staging = match self.opt_staging_tex.as_ref() {
                Some(t) => t,
                None => return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259))),
            };

            let mut mapped_dst = D3D11_MAPPED_SUBRESOURCE::default();
            self.nvidia_context.Map(staging, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped_dst))?;

            let row_bytes = self.width as usize * 4;
            if rgba.len() >= row_bytes * self.height as usize {
                for dy in 0..self.height as usize {
                    let src_offset = dy * row_bytes;
                    let dst_ptr = (mapped_dst.pData as *mut u8).add(dy * mapped_dst.RowPitch as usize);
                    for dx in 0..self.width as usize {
                        let s = src_offset + dx * 4;
                        let d = dx * 4;
                        *dst_ptr.add(d) = rgba[s + 2];     // B
                        *dst_ptr.add(d + 1) = rgba[s + 1]; // G
                        *dst_ptr.add(d + 2) = rgba[s];     // R
                        *dst_ptr.add(d + 3) = 255;         // A
                    }
                }
            }
            self.nvidia_context.Unmap(staging, 0);
            // Пиксели теперь в DEFAULT-текстуре — именно её ждёт NVENC.
            self.nvidia_context.CopyResource(nv_tex, staging);

            let mut map_res: NvEncMapInputResource = zeroed();
            map_res.version = NV_ENC_MAP_INPUT_RESOURCE_VER;
            map_res.registeredResource = self.registered_resource;

            let status = (self.api.nvEncMapInputResource)(self.encoder, &mut map_res);
            if status != NV_ENC_SUCCESS {
                return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259)));
            }

            let mut pic_params: NvEncPicParams = zeroed();
            pic_params.version = NV_ENC_PIC_PARAMS_VER;
            pic_params.inputWidth = self.width;
            pic_params.inputHeight = self.height;
            pic_params.inputBuffer = map_res.mappedResource;
            pic_params.outputBitstream = self.bitstream_buffer;
            pic_params.bufferFmt = NV_ENC_BUFFER_FORMAT_ARGB;
            pic_params.pictureStruct = NV_ENC_PIC_STRUCT_FRAME;
            pic_params.frameIdx = self.frame_count;
            if force_idr {
                pic_params.encodePicFlags |= NV_ENC_PIC_FLAG_FORCEIDR;
            }

            let status = (self.api.nvEncEncodePicture)(self.encoder, &mut pic_params);
            (self.api.nvEncUnmapInputResource)(self.encoder, map_res.mappedResource);
            if status != NV_ENC_SUCCESS {
                return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259)));
            }

            let mut lock_params: NvEncLockBitstream = zeroed();
            lock_params.version = NV_ENC_LOCK_BITSTREAM_VER;
            lock_params.outputBitstream = self.bitstream_buffer;
            lock_params.flags = 0;

            let status = (self.api.nvEncLockBitstream)(self.encoder, &mut lock_params);
            if status != NV_ENC_SUCCESS {
                return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259)));
            }

            let byte_slice = std::slice::from_raw_parts(
                lock_params.bitstreamBufferPtr as *const u8,
                lock_params.bitstreamSizeInBytes as usize,
            );
            let encoded_bytes = byte_slice.to_vec();

            (self.api.nvEncUnlockBitstream)(self.encoder, self.bitstream_buffer);
            self.frame_count += 1;
            Ok(encoded_bytes)
        }
    }
}

impl Drop for NvencEncoder {
    fn drop(&mut self) {
        unsafe {
            if !self.registered_resource.is_null() {
                (self.api.nvEncUnregisterResource)(self.encoder, self.registered_resource);
            }
            if !self.bitstream_buffer.is_null() {
                (self.api.nvEncDestroyBitstreamBuffer)(self.encoder, self.bitstream_buffer);
            }
            if !self.encoder.is_null() {
                (self.api.nvEncDestroyEncoder)(self.encoder);
            }
        }
    }
}
