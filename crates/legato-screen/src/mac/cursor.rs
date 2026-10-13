//! Read the global cursor without changing it or including it in the video.
//!
//! AppKit exposes its image, not a semantic shape. Compare bounded, normalized pixels
//! with this OS's built-in cursors. Custom images (including animated cursors) fall back
//! to an arrow. `currentSystemCursor` is deprecated and may return nil on a future OS;
//! keeping that failure local must never interrupt sharing or hide the Windows cursor.

use legato_proto::{CursorShape, Point, Rect};
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSCursor, NSCursorFrameResizeDirections as Directions, NSCursorFrameResizePosition as Position,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGDisplayBounds, CGDisplayIsActive, CGEvent,
    CGImage, CGImageAlphaInfo, CGImageByteOrderInfo, kCGColorSpaceSRGB,
};

/// Queue one read on AppKit's thread. The caller waits for the reply before requesting
/// another, so a busy main thread cannot accumulate a backlog of cursor work.
pub fn sample_cursor(display: u32, reply: impl FnOnce(CursorShape) + Send + 'static) {
    dispatch2::DispatchQueue::main().exec_async(move || {
        objc2::rc::autoreleasepool(|_| {
            thread_local! {
                static READER: std::cell::RefCell<Option<(u32, CursorReader)>> = const { std::cell::RefCell::new(None) };
            }
            READER.with_borrow_mut(|reader| {
                if reader.as_ref().is_none_or(|(id, _)| *id != display) {
                    *reader = Some((display, CursorReader::new(MainThreadMarker::new().expect("main queue"))));
                }
                reply(reader.as_mut().expect("initialized").1.sample(display));
            });
        });
    });
}

/// Main-thread-only AppKit reader. No permissions are requested and no input is sent.
/// Construct once per display session, so reference images are not rebuilt per sample.
pub struct CursorReader {
    _main: MainThreadMarker,
    references: Vec<(Pixels, CursorShape)>,
    warned_unavailable: bool,
}

#[derive(PartialEq)]
struct Pixels {
    rgba: Vec<u8>,
    aspect: f64,
    hotspot: (f64, f64),
}

impl CursorReader {
    pub fn new(main: MainThreadMarker) -> Self {
        use CursorShape::*;
        let mut cursors = vec![
            (NSCursor::arrowCursor(), Arrow),
            (NSCursor::IBeamCursor(), Text),
            (NSCursor::IBeamCursorForVerticalLayout(), Text),
            (NSCursor::pointingHandCursor(), Pointer),
            (NSCursor::openHandCursor(), Grab),
            (NSCursor::closedHandCursor(), Grabbing),
            (NSCursor::crosshairCursor(), Crosshair),
            (NSCursor::operationNotAllowedCursor(), NotAllowed),
            (NSCursor::dragCopyCursor(), Copy),
            (NSCursor::dragLinkCursor(), Alias),
            (NSCursor::contextualMenuCursor(), ContextMenu),
            (NSCursor::columnResizeCursor(), ResizeColumn),
            (NSCursor::rowResizeCursor(), ResizeRow),
            (NSCursor::zoomInCursor(), ZoomIn),
            (NSCursor::zoomOutCursor(), ZoomOut),
        ];
        for (position, shape) in [
            (Position::Left, ResizeHorizontal),
            (Position::Right, ResizeHorizontal),
            (Position::Top, ResizeVertical),
            (Position::Bottom, ResizeVertical),
            (Position::TopRight, ResizeDiagonalUp),
            (Position::BottomLeft, ResizeDiagonalUp),
            (Position::TopLeft, ResizeDiagonalDown),
            (Position::BottomRight, ResizeDiagonalDown),
        ] {
            // At a window's minimum/maximum size macOS can use a one-sided arrow.
            for directions in [Directions::All, Directions::Inward, Directions::Outward] {
                cursors.push((
                    NSCursor::frameResizeCursorFromPosition_inDirections(position, directions),
                    shape,
                ));
            }
        }
        Self {
            _main: main,
            references: cursors
                .iter()
                .filter_map(|(cursor, shape)| pixels(cursor).map(|p| (p, *shape)))
                .collect(),
            warned_unavailable: false,
        }
    }

    /// Classify image contents, never NSCursor object identity. Nil, malformed, oversized,
    /// and unfamiliar images reset to Arrow rather than retaining a previous shape.
    pub fn classify(&self, cursor: Option<&NSCursor>) -> CursorShape {
        let Some(image) = cursor.and_then(pixels) else {
            return CursorShape::Arrow;
        };
        self.references
            .iter()
            .find_map(|(reference, shape)| (reference == &image).then_some(*shape))
            .unwrap_or_default()
    }

    /// The cursor belongs to the streamed display only while its position is inside it.
    /// Returning Arrow outside also prevents a laptop app's shape leaking into the viewer.
    pub fn sample(&mut self, display: u32) -> CursorShape {
        let Some(event) = CGEvent::new(None) else {
            return CursorShape::Arrow;
        };
        let pos = CGEvent::location(Some(&event));
        let bounds = CGDisplayBounds(display);
        if !CGDisplayIsActive(display)
            || !Rect::new(
                bounds.origin.x,
                bounds.origin.y,
                bounds.size.width,
                bounds.size.height,
            )
            .contains(Point::new(pos.x, pos.y))
        {
            return CursorShape::Arrow;
        }
        #[allow(deprecated)]
        let cursor = NSCursor::currentSystemCursor();
        if cursor.is_none() && !self.warned_unavailable {
            self.warned_unavailable = true;
            tracing::warn!("macOS cursor image unavailable; using the viewer's arrow cursor");
        }
        self.classify(cursor.as_deref())
    }
}

fn pixels(cursor: &NSCursor) -> Option<Pixels> {
    let image = cursor.image();
    let size = image.size();
    let hotspot = cursor.hotSpot();
    if ![size.width, size.height]
        .iter()
        .all(|n| n.is_finite() && *n > 0.0 && *n <= 1024.0)
        || !hotspot.x.is_finite()
        || !hotspot.y.is_finite()
        || hotspot.x < 0.0
        || hotspot.y < 0.0
        || hotspot.x >= size.width
        || hotspot.y >= size.height
    {
        return None;
    }
    // SAFETY: null proposed rect and no hints or graphics context; AppKit chooses its
    // normal image representation. The retained CGImage stays alive through drawing.
    let cg: Retained<CGImage> =
        unsafe { image.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None) }?;
    if !(1..=1024).contains(&CGImage::width(Some(&cg)))
        || !(1..=1024).contains(&CGImage::height(Some(&cg)))
    {
        return None;
    }
    let mut rgba = vec![0; 64 * 64 * 4];
    // SAFETY: the system sRGB name is a valid static CoreFoundation string.
    let color = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))?;
    // SAFETY: the buffer is large enough for 64 rows of 64 RGBA pixels. The context is
    // released before this buffer is read or moved; Quartz cannot retain its pointer.
    let context = unsafe {
        CGBitmapContextCreate(
            rgba.as_mut_ptr().cast(),
            64,
            64,
            8,
            64 * 4,
            Some(&color),
            CGImageAlphaInfo::PremultipliedLast.0 | CGImageByteOrderInfo::Order32Big.0,
        )
    }?;
    CGContext::draw_image(
        Some(&context),
        CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(64.0, 64.0)),
        Some(&cg),
    );
    drop(context);
    Some(Pixels {
        rgba,
        aspect: size.width / size.height,
        hotspot: (hotspot.x / size.width, hotspot.y / size.height),
    })
}
