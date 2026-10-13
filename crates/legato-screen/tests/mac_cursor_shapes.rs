//! Runs on AppKit's main thread; reads built-in images without setting/moving the cursor.
#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    use legato_proto::CursorShape::*;
    use legato_screen::mac::cursor::CursorReader;
    use objc2::{AnyThread, MainThreadMarker, rc::autoreleasepool};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSCursor, NSCursorFrameResizeDirections as D,
        NSCursorFrameResizePosition as P, NSImage,
    };
    use objc2_core_foundation::{CGPoint, CGSize};

    autoreleasepool(|_| {
        let main = MainThreadMarker::new().unwrap();
        let app = NSApplication::sharedApplication(main);
        app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
        let mut reader = CursorReader::new(main);
        let mut cases = vec![
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
            (P::Left, ResizeHorizontal),
            (P::Right, ResizeHorizontal),
            (P::Top, ResizeVertical),
            (P::Bottom, ResizeVertical),
            (P::TopLeft, ResizeDiagonalDown),
            (P::BottomRight, ResizeDiagonalDown),
            (P::TopRight, ResizeDiagonalUp),
            (P::BottomLeft, ResizeDiagonalUp),
        ] {
            for directions in [D::All, D::Inward, D::Outward] {
                cases.push((
                    NSCursor::frameResizeCursorFromPosition_inDirections(position, directions),
                    shape,
                ));
            }
        }
        for (cursor, expected) in cases {
            // A separately allocated object with identical pixels must match too:
            // the global getter doesn't return our reference NSCursor instances.
            let copy = NSCursor::initWithImage_hotSpot(
                NSCursor::alloc(),
                &cursor.image(),
                cursor.hotSpot(),
            );
            assert!(!std::ptr::eq(&*copy, &*cursor));
            assert_eq!(reader.classify(Some(&copy)), expected);
        }
        assert_eq!(reader.classify(None), Arrow);
        let empty = NSCursor::initWithImage_hotSpot(
            NSCursor::alloc(),
            &NSImage::initWithSize(NSImage::alloc(), CGSize::new(32.0, 32.0)),
            CGPoint::new(0.0, 0.0),
        );
        assert_eq!(reader.classify(Some(&empty)), Arrow);
        let huge = NSCursor::initWithImage_hotSpot(
            NSCursor::alloc(),
            &NSImage::initWithSize(NSImage::alloc(), CGSize::new(10_000.0, 10_000.0)),
            CGPoint::new(0.0, 0.0),
        );
        assert_eq!(reader.classify(Some(&huge)), Arrow);
        let unfamiliar = NSCursor::initWithImage_hotSpot(
            NSCursor::alloc(),
            &NSCursor::pointingHandCursor().image(),
            CGPoint::new(0.0, 0.0),
        );
        assert_eq!(
            reader.classify(Some(&unfamiliar)),
            Arrow,
            "don't mistake a custom hotspot for a known cursor"
        );
        assert_eq!(
            reader.sample(u32::MAX),
            Arrow,
            "a removed display must reset the cursor"
        );
        eprintln!("macOS built-in cursor image mapping and safe fallbacks passed");
    });
}
