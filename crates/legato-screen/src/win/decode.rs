//! H.264 decoding with Media Foundation's built-in decoder, in low-latency mode (a frame
//! comes out for every frame that goes in).

use std::mem::ManuallyDrop;

use anyhow::{Context, Result, bail};
use windows::Win32::Media::MediaFoundation::{
    CLSID_MSH264DecoderMFT, CODECAPI_AVLowLatencyMode, IMFMediaType, IMFSample, IMFTransform,
    MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE,
    MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_MINIMUM_DISPLAY_APERTURE,
    MF_MT_SUBTYPE, MF_VERSION, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFMediaType_Video, MFSTARTUP_LITE, MFStartup, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MFVideoArea, MFVideoFormat_H264, MFVideoFormat_NV12,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};

use crate::Nv12;

/// How decoded pictures are laid out in the decoder's output buffers.
#[derive(Debug, Clone, Copy)]
struct Layout {
    width: u32,
    height: u32,
    stride: u32,
    /// Rows allocated per plane (the coded height, often rounded up to 16).
    rows: u32,
}

pub struct Decoder {
    mft: IMFTransform,
    layout: Option<Layout>,
    time: i64,
}

// SAFETY: the decoder is created in the multithreaded apartment and used from one thread
// at a time.
unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new() -> Result<Self> {
        // SAFETY: standard COM and Media Foundation setup. Initialising COM again on the
        // same thread is harmless; a different apartment is fine for this in-process MFT.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_LITE).context("Media Foundation is unavailable")?;
            let mft: IMFTransform =
                CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER)
                    .context("Windows has no H.264 decoder")?;
            if let Ok(attributes) = mft.GetAttributes() {
                let _ = attributes.SetUINT32(&CODECAPI_AVLowLatencyMode, 1);
            }
            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            mft.SetInputType(0, &input, 0)
                .context("the decoder refused H.264")?;
            let mut decoder = Self {
                mft,
                layout: None,
                time: 0,
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
    pub fn decode(&mut self, annex_b: &[u8]) -> Result<Vec<Nv12>> {
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

    fn read(&self, sample: &IMFSample) -> Result<Nv12> {
        let layout = self
            .layout
            .context("decoder produced a picture of unknown size")?;
        // SAFETY: reads a locked, contiguous NV12 buffer within its length.
        unsafe {
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            buffer.Lock(&mut ptr, None, Some(&mut len))?;
            let bytes = std::slice::from_raw_parts(ptr, len as usize);
            let stride = layout.stride as usize;
            let (height, rows) = (layout.height as usize, layout.rows as usize);
            let luma = stride * height;
            let chroma_at = stride * rows;
            let chroma = stride * height / 2;
            let result = if bytes.len() < chroma_at + chroma {
                Err(anyhow::anyhow!("decoded picture is truncated"))
            } else {
                let mut data = Vec::with_capacity(luma + chroma);
                data.extend_from_slice(&bytes[..luma]);
                data.extend_from_slice(&bytes[chroma_at..chroma_at + chroma]);
                Ok(Nv12 {
                    width: layout.width,
                    height: layout.height,
                    stride: layout.stride,
                    data,
                })
            };
            buffer.Unlock()?;
            result
        }
    }
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
