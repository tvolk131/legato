//! Scaling pictures down on the GPU with VideoToolbox, for adaptive quality's smaller
//! picture while the screen moves.

use std::ptr::{self, NonNull};

use anyhow::{Result, bail};
use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferPool, kCVPixelBufferHeightKey,
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
    kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
};
use objc2_video_toolbox::{
    VTPixelTransferSession, VTSessionSetProperty, kVTDownsamplingMode_Average,
    kVTPixelTransferPropertyKey_DownsamplingMode,
};

/// Scales NV12 pictures to one size.
pub struct Scaler {
    session: CFRetained<VTPixelTransferSession>,
    pool: CFRetained<CVPixelBufferPool>,
    pub width: u32,
    pub height: u32,
}

// SAFETY: VideoToolbox sessions and pixel buffer pools may be used from any thread;
// callers serialise `scale`.
unsafe impl Send for Scaler {}

impl Scaler {
    pub fn new(width: u32, height: u32) -> Result<Self> {
        // SAFETY: standard VideoToolbox and CoreVideo setup; every object passed in lives
        // for the call.
        unsafe {
            let mut out = ptr::null_mut();
            let status = VTPixelTransferSession::create(None, NonNull::from(&mut out));
            if status != 0 || out.is_null() {
                bail!("couldn't start scaling pictures (OSStatus {status})");
            }
            let session = CFRetained::from_raw(NonNull::new_unchecked(out));
            // Averaging, not skipping, pixels keeps small text legible at half size.
            let status = VTSessionSetProperty(
                &session,
                kVTPixelTransferPropertyKey_DownsamplingMode,
                Some(kVTDownsamplingMode_Average),
            );
            if status != 0 {
                tracing::debug!("smooth downscaling isn't available (OSStatus {status})");
            }
            let format = CFNumber::new_i32(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange as i32);
            let (w, h) = (
                CFNumber::new_i32(width as i32),
                CFNumber::new_i32(height as i32),
            );
            // Backed by IOSurfaces, so the encoder reads them without a copy.
            let surface = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
            let attributes = CFDictionary::<CFString, CFType>::from_slices(
                &[
                    kCVPixelBufferPixelFormatTypeKey,
                    kCVPixelBufferWidthKey,
                    kCVPixelBufferHeightKey,
                    kCVPixelBufferIOSurfacePropertiesKey,
                ],
                &[&format, &w, &h, &surface],
            );
            let mut out = ptr::null_mut();
            let status = CVPixelBufferPool::create(
                None,
                None,
                Some(attributes.as_opaque()),
                NonNull::from(&mut out),
            );
            if status != 0 || out.is_null() {
                session.invalidate();
                bail!("couldn't make buffers for scaled pictures (CVReturn {status})");
            }
            Ok(Self {
                session,
                pool: CFRetained::from_raw(NonNull::new_unchecked(out)),
                width,
                height,
            })
        }
    }

    /// `image`, scaled to this size.
    pub fn scale(&self, image: &CVPixelBuffer) -> Result<CFRetained<CVPixelBuffer>> {
        // SAFETY: a live pool and session, and valid buffers for the calls.
        unsafe {
            let mut out = ptr::null_mut();
            let status =
                CVPixelBufferPool::create_pixel_buffer(None, &self.pool, NonNull::from(&mut out));
            if status != 0 || out.is_null() {
                bail!("no buffer for a scaled picture (CVReturn {status})");
            }
            let scaled = CFRetained::from_raw(NonNull::new_unchecked(out));
            let status = self.session.transfer_image(image, &scaled);
            if status != 0 {
                bail!("scaling a picture failed (OSStatus {status})");
            }
            Ok(scaled)
        }
    }
}

impl Drop for Scaler {
    fn drop(&mut self) {
        // SAFETY: tearing down a live session nothing else uses.
        unsafe { self.session.invalidate() };
    }
}
