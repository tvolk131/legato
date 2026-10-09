//! D3D11 decoding surfaces copied back to NV12 for the existing viewer. Media
//! Foundation owns the decoder and its texture array; we reuse one staging texture.
//! See Microsoft's "Supporting Direct3D 11 Video Decoding in Media Foundation".

use anyhow::{Context, Result, ensure};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Media::MediaFoundation::{
    IMFDXGIBuffer, IMFDXGIDeviceManager, MFCreateDXGIDeviceManager,
};
use windows::core::Interface;

use super::decode::{Layout, copy_nv12};
use crate::Nv12;

pub(super) struct Gpu {
    pub name: String,
    pub manager: IMFDXGIDeviceManager,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    staging: Option<(u32, u32, ID3D11Texture2D)>,
}

impl Gpu {
    pub fn new() -> Result<Self> {
        // SAFETY: creates a hardware device with video support. The immediate context
        // is protected because Media Foundation may also use it on its worker threads.
        unsafe {
            let (mut device, mut context) = (None, None);
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .context("no D3D11 hardware device with video support")?;
            let device = device.context("D3D11 returned no device")?;
            let context = context.context("D3D11 returned no context")?;
            Self::from_device(device, context)
        }
    }

    pub(super) fn from_device(device: ID3D11Device, context: ID3D11DeviceContext) -> Result<Self> {
        // SAFETY: the context belongs to this device. The manager retains its own
        // device reference, and all uses of the shared context are protected.
        unsafe {
            let _ = context
                .cast::<ID3D11Multithread>()?
                .SetMultithreadProtected(true);
            let desc = device.cast::<IDXGIDevice>()?.GetAdapter()?.GetDesc()?;
            let end = desc.Description.iter().position(|&c| c == 0).unwrap_or(128);
            let name = String::from_utf16_lossy(&desc.Description[..end]);
            let (mut token, mut manager) = (0, None);
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.context("no DXGI device manager")?;
            manager.ResetDevice(&device, token)?;
            Ok(Self {
                name,
                manager,
                device,
                context,
                staging: None,
            })
        }
    }

    pub fn read(&mut self, buffer: &IMFDXGIBuffer, layout: Layout) -> Result<Nv12> {
        // SAFETY: GetResource adds a reference for the requested texture interface.
        // The sample (held by the caller) keeps its array slice alive until the GPU
        // copy and blocking Map have finished. No surface is retained by the viewer.
        unsafe {
            let mut raw = std::ptr::null_mut();
            buffer.GetResource(&ID3D11Texture2D::IID, &mut raw)?;
            ensure!(!raw.is_null(), "hardware decoder returned no texture");
            let texture = ID3D11Texture2D::from_raw(raw);
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            texture.GetDesc(&mut desc);
            let subresource = buffer.GetSubresourceIndex()?;
            ensure!(
                desc.Format == DXGI_FORMAT_NV12
                    && desc.Width.is_multiple_of(2)
                    && desc.Height.is_multiple_of(2)
                    && desc.MipLevels == 1
                    && desc.SampleDesc.Count == 1
                    && subresource < desc.ArraySize
                    && layout.width <= desc.Width
                    && layout.height <= desc.Height,
                "unsupported hardware decoder surface"
            );
            if self
                .staging
                .as_ref()
                .is_none_or(|&(w, h, _)| (w, h) != (desc.Width, desc.Height))
            {
                let staging_desc = D3D11_TEXTURE2D_DESC {
                    Width: desc.Width,
                    Height: desc.Height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_NV12,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_STAGING,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    ..Default::default()
                };
                let mut staging = None;
                self.device
                    .CreateTexture2D(&staging_desc, None, Some(&mut staging))?;
                self.staging = Some((
                    desc.Width,
                    desc.Height,
                    staging.context("no readback texture")?,
                ));
            }
            let staging = &self.staging.as_ref().unwrap().2;
            self.context
                .CopySubresourceRegion(staging, 0, 0, 0, 0, &texture, subresource, None);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let result = (|| {
                ensure!(!mapped.pData.is_null(), "empty hardware readback");
                ensure!(mapped.RowPitch >= desc.Width, "invalid hardware row pitch");
                // NV12's UV plane begins after the TEXTURE's rows, not the visible
                // height or the media type's stride. Drivers pad both rows and pitch.
                let rows = desc.Height as usize;
                let len = (mapped.RowPitch as usize)
                    .checked_mul(rows + rows / 2)
                    .filter(|&n| n <= isize::MAX as usize)
                    .context("hardware picture is too large")?;
                let bytes = std::slice::from_raw_parts(mapped.pData.cast::<u8>(), len);
                copy_nv12(
                    bytes,
                    layout.width,
                    layout.height,
                    mapped.RowPitch,
                    desc.Height,
                )
            })();
            self.context.Unmap(staging, 0);
            result
        }
    }
}
