//! H.264 decoding with Media Foundation's built-in decoder, in low-latency mode (a frame
//! comes out for every frame that goes in).

use std::mem::ManuallyDrop;
use std::{marker::PhantomData, rc::Rc};

use anyhow::{Context, Result, bail, ensure};
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::Media::MediaFoundation::{
    CLSID_MSH264DecoderMFT, CODECAPI_AVDecVideoAcceleration_H264, CODECAPI_AVLowLatencyMode,
    IMFDXGIBuffer, IMFMediaType, IMFSample, IMFTransform, MF_E_NOTACCEPTING,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_MT_DEFAULT_STRIDE,
    MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_SUBTYPE,
    MF_SA_D3D11_AWARE, MF_VERSION, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFMediaType_Video, MFSTARTUP_LITE, MFShutdown, MFStartup, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFVideoArea, MFVideoFormat_H264, MFVideoFormat_NV12,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::core::Interface;

use super::gpu::Gpu;
use crate::Nv12;

/// How decoded pictures are laid out in the decoder's output buffers.
#[derive(Debug, Clone, Copy)]
pub(super) struct Layout {
    pub width: u32,
    pub height: u32,
    stride: u32,
    /// Rows allocated per plane (the coded height, often rounded up to 16).
    rows: u32,
}

pub struct Decoder {
    inner: Transform,
    reported: bool,
}

impl Decoder {
    /// Prefer D3D11, but only after the Mac's known stream decodes correctly, with
    /// GPU surfaces and one output per input. A fresh MFT then handles the real stream.
    pub fn new() -> Result<Self> {
        let hardware = (|| {
            let mut probe = Transform::new(true)?;
            for (n, frame) in
                crate::test_pattern::unpack(include_bytes!("../../tests/fixtures/pattern.frames"))
                    .iter()
                    .enumerate()
            {
                let pictures = probe.decode(frame)?;
                ensure!(
                    pictures.len() == 1,
                    "hardware decoder buffered the test pattern"
                );
                crate::test_pattern::matches(&pictures[0], n as u32).map_err(anyhow::Error::msg)?;
            }
            tracing::info!(
                "H.264 hardware self-check passed on {}",
                probe.gpu.as_ref().unwrap().name
            );
            probe.fresh()
        })();
        match hardware {
            Ok(inner) => Ok(Self {
                inner,
                reported: false,
            }),
            Err(e) => {
                tracing::info!("H.264 hardware decoding unavailable: {e:#}; using software");
                Self::new_software()
            }
        }
    }

    /// Explicit software path, also used after a hardware failure for this stream.
    pub fn new_software() -> Result<Self> {
        Ok(Self {
            inner: Transform::new(false)?,
            reported: false,
        })
    }

    pub fn is_hardware_accelerated(&self) -> bool {
        self.inner.gpu.is_some()
    }

    /// Decodes one complete access unit. The engine associates its timing with its
    /// output, so hardware that buffers pictures must fall back rather than add lag.
    pub fn decode(&mut self, annex_b: &[u8]) -> Result<Vec<Nv12>> {
        let pictures = self.inner.decode(annex_b)?;
        if self.is_hardware_accelerated() {
            ensure!(
                pictures.len() == 1,
                "hardware decoder did not return one picture per frame"
            );
        }
        if !pictures.is_empty() && !self.reported {
            match &self.inner.gpu {
                Some(gpu) => tracing::info!(
                    "H.264 decoding: D3D11 hardware on {} (NV12 readback)",
                    gpu.name
                ),
                None => tracing::info!("H.264 decoding: software"),
            }
            self.reported = true;
        }
        Ok(pictures)
    }

    /// Abandons failed hardware for the rest of this stream (including device loss
    /// after sleep). The caller must ask for a keyframe before decoding again.
    pub fn reset_after_error(&mut self, error: &anyhow::Error) -> Result<()> {
        if self.is_hardware_accelerated() {
            tracing::warn!(
                "H.264 hardware decoding failed: {error:#}; switching to software for this stream"
            );
        }
        *self = Self::new_software()?;
        Ok(())
    }
}

struct Transform {
    mft: IMFTransform,
    gpu: Option<Gpu>,
    layout: Option<Layout>,
    time: i64,
    // Dropped after the COM objects. Keep the decoder on its creating thread so COM
    // initialization/uninitialization remain paired, including probe/fallback errors.
    _runtime: Runtime,
}

struct Runtime {
    com_initialized: bool,
    _thread: PhantomData<Rc<()>>,
}

impl Runtime {
    fn new() -> Result<Self> {
        // SAFETY: initialization is balanced on this thread, including S_FALSE when
        // COM was already initialized. A caller's different apartment is left alone.
        unsafe {
            let com = CoInitializeEx(None, COINIT_MULTITHREADED);
            if com != RPC_E_CHANGED_MODE {
                com.ok()?;
            }
            if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_LITE) {
                if com.is_ok() {
                    CoUninitialize();
                }
                return Err(e).context("Media Foundation is unavailable");
            }
            Ok(Self {
                com_initialized: com.is_ok(),
                _thread: PhantomData,
            })
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // SAFETY: the MFT/device are gone and this value cannot move across threads.
        unsafe {
            let _ = MFShutdown();
            if self.com_initialized {
                CoUninitialize();
            }
        }
    }
}

impl Transform {
    fn new(hardware: bool) -> Result<Self> {
        let runtime = Runtime::new()?;
        let gpu = hardware.then(Gpu::new).transpose()?;
        Self::create(runtime, gpu)
    }

    /// Keep the tested device, but none of the probe's codec state or timestamps.
    fn fresh(self) -> Result<Self> {
        let Self {
            mft, gpu, _runtime, ..
        } = self;
        drop(mft);
        Self::create(_runtime, gpu)
    }

    fn create(runtime: Runtime, gpu: Option<Gpu>) -> Result<Self> {
        // SAFETY: creating and configuring the built-in synchronous H.264 MFT. The
        // manager is kept alive with the transform and attached before media types.
        unsafe {
            let mft: IMFTransform =
                CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER)
                    .context("Windows has no H.264 decoder")?;
            let attributes = mft.GetAttributes()?;
            attributes.SetUINT32(&CODECAPI_AVLowLatencyMode, 1)?;
            attributes.SetUINT32(
                &CODECAPI_AVDecVideoAcceleration_H264,
                u32::from(gpu.is_some()),
            )?;
            if let Some(gpu) = &gpu {
                ensure!(
                    attributes.GetUINT32(&MF_SA_D3D11_AWARE)? != 0,
                    "H.264 MFT is not D3D11-aware"
                );
                mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, gpu.manager.as_raw() as usize)?;
            }
            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            mft.SetInputType(0, &input, 0)
                .context("the decoder refused H.264")?;
            let mut decoder = Self {
                mft,
                gpu,
                layout: None,
                time: 0,
                _runtime: runtime,
            };
            decoder.choose_output()?;
            decoder
                .mft
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            decoder
                .mft
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(decoder)
        }
    }

    /// Picks NV12 output and notes its layout (known once the stream's size is).
    fn choose_output(&mut self) -> Result<()> {
        // SAFETY: enumerating and setting the output type of a live MFT.
        unsafe {
            for i in 0.. {
                let Ok(t) = self.mft.GetOutputAvailableType(0, i) else {
                    bail!("the decoder can't output NV12");
                };
                if t.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12 {
                    self.mft.SetOutputType(0, &t, 0)?;
                    self.layout = layout(&t);
                    return Ok(());
                }
            }
        }
        unreachable!()
    }

    /// Decodes one access unit (Annex B), returning the pictures it completes.
    fn decode(&mut self, annex_b: &[u8]) -> Result<Vec<Nv12>> {
        let sample = self.input_sample(annex_b)?;
        let mut out = Vec::new();
        // SAFETY: feeding a live MFT.
        match unsafe { self.mft.ProcessInput(0, &sample, 0) } {
            Ok(()) => {}
            Err(e) if e.code() == MF_E_NOTACCEPTING => {
                self.drain(&mut out)?;
                // SAFETY: as above.
                unsafe { self.mft.ProcessInput(0, &sample, 0)? };
            }
            Err(e) => return Err(e).context("the decoder rejected a frame"),
        }
        self.drain(&mut out)?;
        Ok(out)
    }

    fn input_sample(&mut self, data: &[u8]) -> Result<IMFSample> {
        // SAFETY: fills a fresh Media Foundation buffer of the right size.
        unsafe {
            let buffer = MFCreateMemoryBuffer(data.len() as u32)?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(data.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            // 100 ns units; only the order matters.
            sample.SetSampleTime(self.time)?;
            self.time += 166_667;
            Ok(sample)
        }
    }

    fn drain(&mut self, out: &mut Vec<Nv12>) -> Result<()> {
        loop {
            // SAFETY: standard ProcessOutput loop; the output buffer's references are
            // released below.
            unsafe {
                let info = self.mft.GetOutputStreamInfo(0)?;
                let provides = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
                let sample = if provides {
                    None
                } else {
                    let sample = MFCreateSample()?;
                    sample.AddBuffer(&MFCreateMemoryBuffer(info.cbSize)?)?;
                    Some(sample)
                };
                let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(sample),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                }];
                let mut status = 0;
                let result = self.mft.ProcessOutput(0, &mut buffers, &mut status);
                let sample = ManuallyDrop::take(&mut buffers[0].pSample);
                drop(ManuallyDrop::take(&mut buffers[0].pEvents));
                match result {
                    Ok(()) => {
                        if let Some(sample) = sample {
                            out.push(self.read(&sample)?);
                        }
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.choose_output()?,
                    Err(e) => return Err(e).context("decoding failed"),
                }
            }
        }
    }

    fn read(&mut self, sample: &IMFSample) -> Result<Nv12> {
        let layout = self
            .layout
            .context("decoder produced a picture of unknown size")?;
        // SAFETY: reads a locked, contiguous NV12 buffer within its length.
        unsafe {
            if let Some(gpu) = &mut self.gpu {
                // Merely attaching a manager is not proof of hardware decoding: the
                // MFT may reject a size/profile or return software buffers instead.
                let surface = sample
                    .GetBufferByIndex(0)?
                    .cast::<IMFDXGIBuffer>()
                    .context("hardware decoder returned a non-DXGI picture")?;
                return gpu.read(&surface, layout);
            }
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            buffer.Lock(&mut ptr, None, Some(&mut len))?;
            let bytes = std::slice::from_raw_parts(ptr, len as usize);
            let result = copy_nv12(
                bytes,
                layout.width,
                layout.height,
                layout.stride,
                layout.rows,
            );
            buffer.Unlock()?;
            result
        }
    }
}

/// Strip row and coded-height padding, keeping the chroma plane at its actual offset.
pub(super) fn copy_nv12(
    bytes: &[u8],
    width: u32,
    height: u32,
    pitch: u32,
    rows: u32,
) -> Result<Nv12> {
    ensure!(
        width > 0
            && height > 0
            && width.is_multiple_of(2)
            && height.is_multiple_of(2)
            && rows.is_multiple_of(2)
            && pitch >= width
            && rows >= height,
        "invalid NV12 layout"
    );
    let (w, h, stride, rows) = (
        width as usize,
        height as usize,
        pitch as usize,
        rows as usize,
    );
    let chroma_at = stride.checked_mul(rows).context("NV12 layout overflows")?;
    let end = stride
        .checked_mul(h / 2)
        .and_then(|n| chroma_at.checked_add(n))
        .context("NV12 layout overflows")?;
    ensure!(bytes.len() >= end, "decoded picture is truncated");
    let size = w
        .checked_mul(h + h / 2)
        .filter(|&n| n <= isize::MAX as usize)
        .context("NV12 picture is too large")?;
    let mut data = Vec::with_capacity(size);
    for (start, count) in [(0, h), (chroma_at, h / 2)] {
        for row in 0..count {
            let at = start + row * stride;
            data.extend_from_slice(&bytes[at..at + w]);
        }
    }
    Ok(Nv12 {
        width,
        height,
        stride: width,
        data,
    })
}

/// The picture layout an NV12 output type describes, if its size is known yet.
fn layout(t: &IMFMediaType) -> Option<Layout> {
    // SAFETY: reading attributes of a media type.
    unsafe {
        let size = t.GetUINT64(&MF_MT_FRAME_SIZE).ok()?;
        let (coded_w, coded_h) = ((size >> 32) as u32, size as u32);
        if coded_w == 0 || coded_h == 0 {
            return None;
        }
        let stride = t
            .GetUINT32(&MF_MT_DEFAULT_STRIDE)
            .map_or(coded_w, |s| (s as i32).unsigned_abs());
        let mut area = MFVideoArea::default();
        let visible = t
            .GetBlob(
                &MF_MT_MINIMUM_DISPLAY_APERTURE,
                std::slice::from_raw_parts_mut(
                    (&mut area as *mut MFVideoArea).cast(),
                    size_of::<MFVideoArea>(),
                ),
                None,
            )
            .ok()
            .map(|()| (area.Area.cx as u32, area.Area.cy as u32))
            .filter(|&(w, h)| w > 0 && h > 0 && w <= coded_w && h <= coded_h);
        let (width, height) = visible.unwrap_or((coded_w, coded_h));
        Some(Layout {
            width,
            height,
            stride,
            rows: coded_h,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nv12_strips_pitch_and_coded_height_padding() {
        let mut bytes = [255; 48]; // pitch 8, coded height 4, visible 4 × 2
        bytes[..4].copy_from_slice(&[1, 2, 3, 4]);
        bytes[8..12].copy_from_slice(&[5, 6, 7, 8]);
        bytes[32..36].copy_from_slice(&[9, 10, 11, 12]);
        let picture = copy_nv12(&bytes, 4, 2, 8, 4).unwrap();
        assert_eq!((picture.width, picture.height, picture.stride), (4, 2, 4));
        assert_eq!(picture.data, (1..=12).collect::<Vec<_>>());
    }

    #[test]
    fn nv12_rejects_invalid_or_truncated_layouts() {
        for (w, h, pitch, rows) in [
            (0, 2, 4, 2),
            (4, 0, 4, 2),
            (3, 2, 4, 2),
            (4, 3, 4, 4),
            (4, 2, 3, 2),
            (4, 4, 4, 2),
            (4, 2, 4, 3),
            (4, 2, u32::MAX, u32::MAX - 1),
        ] {
            assert!(copy_nv12(&[0; 48], w, h, pitch, rows).is_err());
        }
        assert!(copy_nv12(&[0; 39], 4, 2, 8, 4).is_err());
    }

    #[test]
    fn gpu_readback_uses_the_selected_slice_and_texture_height() {
        use windows::Win32::Foundation::HMODULE;
        use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
        use windows::Win32::Graphics::Direct3D11::*;
        use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
        use windows::Win32::Media::MediaFoundation::MFCreateDXGISurfaceBuffer;

        let _runtime = Runtime::new().unwrap();
        // WARP exercises the real D3D11 texture/Map path in hosted CI. It does not
        // claim to test hardware H.264 decoding, which needs a physical video device.
        // SAFETY: COM/MF live longer than the device and buffers; texture data stays
        // alive through CreateTexture2D, which copies it into its own resources.
        unsafe {
            let (mut device, mut context) = (None, None);
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_FLAG(0),
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .unwrap();
            let device = device.unwrap();
            let mut gpu = Gpu::from_device(device.clone(), context.unwrap()).unwrap();
            for (width, height) in [(64, 32), (128, 48)] {
                let pitch = width + 8;
                let plane = (pitch * height) as usize;
                let mut data = vec![123u8; plane * 3 / 2];
                for pair in data[plane..].as_chunks_mut::<2>().0 {
                    pair.copy_from_slice(&[45, 178]);
                }
                let other = vec![6u8; data.len()];
                let initial = [&other, &data].map(|bytes| D3D11_SUBRESOURCE_DATA {
                    pSysMem: bytes.as_ptr().cast(),
                    SysMemPitch: pitch,
                    SysMemSlicePitch: bytes.len() as u32,
                });
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: width,
                    Height: height,
                    MipLevels: 1,
                    ArraySize: 2,
                    Format: DXGI_FORMAT_NV12,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_DEFAULT,
                    ..Default::default()
                };
                let mut texture = None;
                device
                    .CreateTexture2D(&desc, Some(initial.as_ptr()), Some(&mut texture))
                    .unwrap();
                let texture = texture.unwrap();
                let layout = Layout {
                    width: width - 4,
                    height: height - 2,
                    stride: 0,
                    rows: 0,
                };
                for slice in [1, 0, 1] {
                    let buffer =
                        MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &texture, slice, false)
                            .unwrap()
                            .cast::<IMFDXGIBuffer>()
                            .unwrap();
                    let picture = gpu.read(&buffer, layout).unwrap();
                    let visible = (layout.width * layout.height) as usize;
                    assert_eq!(picture.stride, layout.width);
                    assert_eq!(picture.data.len(), visible * 3 / 2);
                    let y = if slice == 1 { 123 } else { 6 };
                    let uv = if slice == 1 { [45, 178] } else { [6, 6] };
                    assert!(picture.data[..visible].iter().all(|&v| v == y));
                    assert!(
                        picture.data[visible..]
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .all(|p| *p == uv)
                    );
                }
            }
        }
    }
}
