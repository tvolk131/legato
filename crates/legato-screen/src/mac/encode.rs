//! Hardware H.264 encoding with VideoToolbox, tuned for latency: real-time, no frame
//! reordering, low-latency rate control, keyframes on request.

use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Result, bail};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMSampleBuffer, CMTime, CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
    kCMSampleAttachmentKey_NotSync, kCMTimeInvalid, kCMVideoCodecType_H264,
};
use objc2_core_video::CVPixelBuffer;
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, VTSessionSetProperty,
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_ProfileLevel, kVTCompressionPropertyKey_RealTime,
    kVTEncodeFrameOptionKey_ForceKeyFrame, kVTProfileLevel_H264_ConstrainedHigh_AutoLevel,
    kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
};

use crate::h264;

/// One encoded picture, as an Annex B access unit (SPS and PPS included on keyframes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub keyframe: bool,
    /// Presentation time, from the start of the stream.
    pub pts: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Average bits per second.
    pub bitrate: u32,
}

type Sink = Mutex<Box<dyn FnMut(Result<EncodedFrame>) + Send>>;

pub struct Encoder {
    session: CFRetained<VTCompressionSession>,
    sink: *mut Sink,
}

// SAFETY: VTCompressionSession may be used from any thread; the sink is only touched
// through its mutex.
unsafe impl Send for Encoder {}
// SAFETY: as above; callers serialise `encode` themselves when frame order matters.
unsafe impl Sync for Encoder {}

fn check(status: i32, what: &str) -> Result<()> {
    if status != 0 {
        bail!("{what} failed (OSStatus {status})");
    }
    Ok(())
}

fn set(session: &VTCompressionSession, key: &CFString, value: &CFType) -> Result<()> {
    // SAFETY: a valid session, key and value.
    let status = unsafe { VTSessionSetProperty(session, key, Some(value)) };
    check(status, &format!("setting {key}"))
}

impl Encoder {
    /// Starts an encoder. `on_frame` is called on a VideoToolbox thread with each encoded
    /// frame, in order.
    pub fn new(
        config: EncoderConfig,
        on_frame: impl FnMut(Result<EncodedFrame>) + Send + 'static,
    ) -> Result<Self> {
        let sink: *mut Sink = Box::into_raw(Box::new(Mutex::new(Box::new(on_frame))));
        // SAFETY: standard VideoToolbox setup; `sink` outlives the session (freed in Drop
        // after the session is invalidated).
        unsafe {
            let spec = CFDictionary::<CFString, CFBoolean>::from_slices(
                &[kVTVideoEncoderSpecification_EnableLowLatencyRateControl],
                &[CFBoolean::new(true)],
            );
            let mut out = ptr::null_mut();
            let status = VTCompressionSession::create(
                None,
                config.width as i32,
                config.height as i32,
                kCMVideoCodecType_H264,
                Some(spec.as_opaque()),
                None,
                None,
                Some(output),
                sink.cast(),
                NonNull::from(&mut out),
            );
            if status != 0 || out.is_null() {
                drop(Box::from_raw(sink));
                bail!("couldn't start the H.264 encoder (OSStatus {status})");
            }
            let session = CFRetained::from_raw(NonNull::new_unchecked(out));
            let encoder = Self { session, sink };
            let s = &encoder.session;
            set(s, kVTCompressionPropertyKey_RealTime, CFBoolean::new(true))?;
            set(
                s,
                kVTCompressionPropertyKey_AllowFrameReordering,
                CFBoolean::new(false),
            )?;
            set(
                s,
                kVTCompressionPropertyKey_ProfileLevel,
                kVTProfileLevel_H264_ConstrainedHigh_AutoLevel,
            )?;
            set(
                s,
                kVTCompressionPropertyKey_AverageBitRate,
                &CFNumber::new_i64(config.bitrate.into()),
            )?;
            set(
                s,
                kVTCompressionPropertyKey_ExpectedFrameRate,
                &CFNumber::new_i32(config.fps as i32),
            )?;
            // Keyframes are sent when a viewer asks, plus one a minute to heal any damage.
            set(
                s,
                kVTCompressionPropertyKey_MaxKeyFrameInterval,
                &CFNumber::new_i32((config.fps * 60) as i32),
            )?;
            check(s.prepare_to_encode_frames(), "preparing the encoder")?;
            Ok(encoder)
        }
    }

    /// Queues a picture. With `keyframe`, it's encoded so a decoder can start from it.
    pub fn encode(&self, image: &CVPixelBuffer, pts: Duration, keyframe: bool) -> Result<()> {
        // SAFETY: a live session and pixel buffer; the options dictionary is retained for
        // the call.
        unsafe {
            let options = keyframe.then(|| {
                CFDictionary::<CFString, CFBoolean>::from_slices(
                    &[kVTEncodeFrameOptionKey_ForceKeyFrame],
                    &[CFBoolean::new(true)],
                )
            });
            let status = self.session.encode_frame(
                image,
                CMTime::new(pts.as_micros() as i64, 1_000_000),
                kCMTimeInvalid,
                options.as_deref().map(|o| o.as_opaque()),
                ptr::null_mut(),
                ptr::null_mut(),
            );
            check(status, "encoding a frame")
        }
    }

    /// Encodes a picture and waits until it has been delivered to `on_frame`.
    ///
    /// Left to itself the encoder keeps about five frames in flight, which adds about
    /// five frame intervals of lag (80 ms at 60 fps) and holds the last pictures back
    /// when the screen goes still. Waiting costs throughput instead: about 16 ms per
    /// 4K frame and 8 ms per 1440p frame on Apple silicon.
    pub fn encode_now(&self, image: &CVPixelBuffer, pts: Duration, keyframe: bool) -> Result<()> {
        self.encode(image, pts, keyframe)?;
        self.flush();
        Ok(())
    }

    /// Waits until every queued picture has been delivered.
    pub fn flush(&self) {
        // SAFETY: a live session.
        unsafe { self.session.complete_frames(kCMTimeInvalid) };
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: after invalidation no more callbacks arrive, so the sink can go.
        unsafe {
            self.session.complete_frames(kCMTimeInvalid);
            self.session.invalidate();
            drop(Box::from_raw(self.sink));
        }
    }
}

unsafe extern "C-unwind" fn output(
    refcon: *mut c_void,
    _frame_refcon: *mut c_void,
    status: i32,
    _flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    // SAFETY: `refcon` is the sink passed at creation, alive until the session is gone.
    let sink = unsafe { &*(refcon as *const Sink) };
    let result = if status != 0 {
        Err(anyhow::anyhow!("encoding failed (OSStatus {status})"))
    } else if sample.is_null() {
        // The encoder dropped the frame.
        return;
    } else {
        // SAFETY: VideoToolbox passes a valid sample buffer for the call's duration.
        unsafe { to_annex_b(&*sample) }
    };
    if let Ok(mut sink) = sink.lock() {
        sink(result);
    }
}

/// Converts an encoded sample buffer (AVCC) to an Annex B access unit.
unsafe fn to_annex_b(sample: &CMSampleBuffer) -> Result<EncodedFrame> {
    // SAFETY: CoreMedia accessors on a valid sample buffer.
    unsafe {
        let Some(block) = sample.data_buffer() else {
            bail!("encoded frame has no data");
        };
        let len = block.data_length();
        let mut avcc = vec![0u8; len];
        check(
            block.copy_data_bytes(0, len, NonNull::new_unchecked(avcc.as_mut_ptr().cast())),
            "reading an encoded frame",
        )?;
        let keyframe = !not_sync(sample);
        let Some(format) = sample.format_description() else {
            bail!("encoded frame has no format");
        };
        let mut out = Vec::with_capacity(len + 64);
        let mut count = 0usize;
        let mut nal_len = 0i32;
        check(
            CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                &format,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut count,
                &mut nal_len,
            ),
            "reading H.264 parameter sets",
        )?;
        if keyframe {
            for i in 0..count {
                let mut set = ptr::null();
                let mut size = 0usize;
                check(
                    CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                        &format,
                        i,
                        &mut set,
                        &mut size,
                        ptr::null_mut(),
                        ptr::null_mut(),
                    ),
                    "reading an H.264 parameter set",
                )?;
                h264::push_nal(std::slice::from_raw_parts(set, size), &mut out);
            }
        }
        if h264::avcc_to_annex_b(&avcc, nal_len as usize, &mut out).is_none() {
            bail!("malformed encoded frame");
        }
        let pts = sample.presentation_time_stamp();
        let pts = if pts.timescale > 0 && pts.value >= 0 {
            Duration::from_secs_f64(pts.value as f64 / pts.timescale as f64)
        } else {
            Duration::ZERO
        };
        Ok(EncodedFrame {
            data: out,
            keyframe,
            pts,
        })
    }
}

/// Whether the sample is marked as depending on earlier ones.
unsafe fn not_sync(sample: &CMSampleBuffer) -> bool {
    // SAFETY: attachments of a valid sample buffer; the dictionary lives as long as it.
    unsafe {
        let Some(attachments) = sample.sample_attachments_array(false) else {
            return false;
        };
        if attachments.count() == 0 {
            return false;
        }
        let dict = attachments.value_at_index(0) as *const CFDictionary;
        let Some(dict) = dict.as_ref() else {
            return false;
        };
        let key: *const CFString = kCMSampleAttachmentKey_NotSync;
        let value = dict.value(key.cast()) as *const CFBoolean;
        value.as_ref().is_some_and(CFBoolean::as_bool)
    }
}
