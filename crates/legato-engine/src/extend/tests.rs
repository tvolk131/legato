use std::future::pending;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use legato_net::VideoFrame;
use legato_proto::{
    Control,
    video_track::{MOVING, SHARP},
};
use tokio::sync::watch;

use super::{Decoded, Picture, Showing, ViewerFrame, ViewerStats, Viewing};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn picture(width: u32) -> Picture {
    Picture {
        nv12: legato_screen::Nv12 {
            width,
            height: 2,
            stride: width,
            data: vec![0; width as usize * 3],
        },
        decoded_at: Instant::now(),
        mac: ms(5),
        network: ms(1),
        decode: ms(2),
    }
}

struct Outputs {
    frames: watch::Sender<Option<ViewerFrame>>,
    stats: watch::Sender<Option<ViewerStats>>,
    cursor: watch::Sender<legato_proto::CursorShape>,
}

impl Outputs {
    fn new() -> Self {
        Self {
            frames: watch::channel(None).0,
            stats: watch::channel(None).0,
            cursor: watch::channel(legato_proto::CursorShape::Arrow).0,
        }
    }

    fn viewer(&self, peer: u8) -> Viewing {
        Viewing::new(
            iroh::SecretKey::from_bytes(&[peer; 32]).public(),
            self.frames.clone(),
            self.stats.clone(),
            self.cursor.clone(),
        )
    }

    fn width(&self) -> Option<u32> {
        self.frames.borrow().as_ref().map(|p| p.nv12.width)
    }
}

fn decoded(mac: u64, skipped: u16) -> Decoded {
    Decoded {
        bytes: 125_000,
        mac: ms(mac),
        decode: ms(3),
        skipped,
    }
}

fn publish_stats(showing: &Showing) {
    showing.inner.lock().unwrap().since = Instant::now() - Duration::from_secs(1);
    showing.record(decoded(20, 0), ms(1), false);
}

async fn cancelled(task: tokio::task::JoinHandle<()>) {
    assert!(
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap_err()
            .is_cancelled()
    );
}

#[test]
fn the_newest_picture_of_any_track_is_shown() {
    let outputs = Outputs::new();
    let view = outputs.viewer(1);
    let showing = &view.showing;
    assert!(showing.show(SHARP, ms(100), picture(3840)));
    // Dragging a window: the smaller pictures take over...
    assert!(showing.show(MOVING, ms(110), picture(1920)));
    assert_eq!(outputs.width(), Some(1920));
    // ...and a sharp one that was on its way arrives too late to show.
    assert!(!showing.show(SHARP, ms(105), picture(3840)));
    assert_eq!(outputs.width(), Some(1920));
    // Still again: sharpened.
    assert!(showing.show(SHARP, ms(300), picture(3840)));
    assert_eq!(outputs.width(), Some(3840));
}

#[test]
fn stats_cover_every_track() {
    let outputs = Outputs::new();
    let view = outputs.viewer(1);
    let showing = &view.showing;
    showing.show(SHARP, ms(10), picture(3840));
    assert!(!showing.record(decoded(20, 0), ms(1), false));
    showing.show(MOVING, ms(20), picture(1920));
    showing.inner.lock().unwrap().since = Instant::now() - Duration::from_secs(1);
    assert!(showing.record(decoded(8, 2), ms(1), false));
    let stats = outputs.stats.borrow().unwrap();
    assert!(stats.moving, "showing the moving picture");
    assert_eq!((stats.width, stats.height), (1920, 2));
    assert_eq!(stats.mac, ms(14));
    assert_eq!(stats.mac_max, ms(20));
    assert_eq!(stats.skipped, 2);
    assert!((stats.megabits_per_second - 2.0).abs() < 0.1);
}

#[tokio::test]
async fn stats_stop_with_the_last_track() {
    let outputs = Outputs::new();
    let view = outputs.viewer(1);
    let reader_a = tokio::spawn(pending());
    let reader_b = tokio::spawn(pending());
    let a = view
        .showing
        .decoder_started(reader_a.abort_handle())
        .unwrap();
    let b = view
        .showing
        .decoder_started(reader_b.abort_handle())
        .unwrap();
    publish_stats(&view.showing);
    let stats = *outputs.stats.borrow();
    assert!(stats.is_some());
    drop(a);
    assert_eq!(*outputs.stats.borrow(), stats);
    drop(b);
    assert!(outputs.stats.borrow().is_none());
    assert!(view.showing.inner.lock().unwrap().readers.is_empty());
    cancelled(reader_a).await;
    cancelled(reader_b).await;
}

#[tokio::test]
async fn late_worker_cannot_publish_clear_stats_or_send_controls_after_close_or_replacement() {
    // Closing, reopening the same Mac, and switching to another Mac all invalidate
    // the old worker, even when it finishes decoding after the new viewer starts.
    for replacement in [None, Some(1), Some(2)] {
        let outputs = Outputs::new();
        let old = outputs.viewer(1);
        old.showing.show(SHARP, ms(1), picture(1920));
        publish_stats(&old.showing);
        assert_eq!(outputs.width(), Some(1920));
        assert!(outputs.stats.borrow().is_some());
        let reader = tokio::spawn(pending());
        let decoding = old.showing.decoder_started(reader.abort_handle()).unwrap();
        let showing = old.showing.clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let (controls, received) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _decoding = decoding;
            let frame = picture(3840);
            ready_tx.send(()).unwrap();
            resume_rx.recv().unwrap();
            showing.show(SHARP, ms(100), frame);
            publish_stats(&showing);
            for msg in [
                Control::Keyframe { track: SHARP },
                Control::ExtendStop {
                    reason: "old decoder failed".into(),
                },
            ] {
                showing.when_open(|| {
                    controls.send(msg).unwrap();
                });
            }
            // Also verify repeated cleanup can't clear a new viewer's outputs.
            showing.close();
        });
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(old);
        assert_eq!(outputs.width(), None);
        assert!(outputs.stats.borrow().is_none());
        let new = replacement.map(|peer| outputs.viewer(peer));
        if let Some(view) = &new {
            view.showing.show(MOVING, ms(1), picture(1280));
            publish_stats(&view.showing);
            let mut sent = false;
            view.showing.when_open(|| sent = true);
            assert!(sent, "the new viewer can still request keyframes");
        }
        let stats = *outputs.stats.borrow();
        resume_tx.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(outputs.width(), replacement.map(|_| 1280));
        assert_eq!(*outputs.stats.borrow(), stats);
        assert_eq!(received.try_iter().count(), 0);
        cancelled(reader).await;
    }
}

#[tokio::test]
async fn closing_wakes_idle_decoders_and_discards_queued_frames() {
    for queued in [false, true] {
        let outputs = Outputs::new();
        let view = outputs.viewer(1);
        let showing = view.showing.clone();
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let frame = || VideoFrame {
            data: vec![1],
            keyframe: true,
            sender_time: ms(0),
            skipped: 0,
            pts: ms(1),
        };
        if queued {
            tx.try_send(frame()).unwrap();
        }
        let (started, reading) = tokio::sync::oneshot::channel();
        // Exercise readers stalled both on a full queue and waiting for network data.
        let reader = tokio::spawn(async move {
            started.send(()).unwrap();
            if queued {
                tx.send(frame()).await.unwrap();
            }
            let _tx = tx;
            pending::<()>().await;
        });
        let decoding = showing.decoder_started(reader.abort_handle()).unwrap();
        reading.await.unwrap();
        let worker = if queued {
            drop(view);
            tokio::task::spawn_blocking(move || decoding.next(&mut rx).is_none())
        } else {
            let worker = tokio::task::spawn_blocking(move || decoding.next(&mut rx).is_none());
            drop(view);
            worker
        };
        cancelled(reader).await;
        assert!(
            tokio::time::timeout(Duration::from_secs(5), worker)
                .await
                .unwrap()
                .unwrap()
        );
        assert!(showing.inner.lock().unwrap().readers.is_empty());
        let late_reader = tokio::spawn(pending());
        assert!(
            showing
                .decoder_started(late_reader.abort_handle())
                .is_none()
        );
        cancelled(late_reader).await;
    }
}

#[tokio::test]
async fn unstarted_and_early_returning_workers_clean_up_their_reader_and_stats() {
    for start in [false, true] {
        let outputs = Outputs::new();
        let view = outputs.viewer(1);
        publish_stats(&view.showing);
        let reader = tokio::spawn(pending());
        let decoding = view.showing.decoder_started(reader.abort_handle()).unwrap();
        let worker = move || {
            // This guard is acquired before spawning, so it also covers decoder
            // initialization returning an error before the frame loop begins.
            let _decoding = decoding;
            Err::<(), _>("decoder initialization failed")
        };
        if start {
            assert!(std::thread::spawn(worker).join().unwrap().is_err());
        } else {
            // An OS spawn failure drops the supplied closure without calling it.
            drop(worker);
        }
        cancelled(reader).await;
        assert!(view.showing.inner.lock().unwrap().readers.is_empty());
        assert!(outputs.stats.borrow().is_none());
    }
}

#[test]
fn close_waits_for_control_publication_before_allowing_a_new_viewer() {
    let outputs = Outputs::new();
    let view = outputs.viewer(1);
    let showing = view.showing.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let effects = order.clone();
    let worker = std::thread::spawn(move || {
        showing.when_open(|| {
            entered_tx.send(()).unwrap();
            resume_rx.recv().unwrap();
            effects.lock().unwrap().push("old control");
        });
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(view.showing.inner.try_lock().is_err());
    let effects = order.clone();
    let close = std::thread::spawn(move || {
        drop(view);
        effects.lock().unwrap().push("closed");
    });
    resume_tx.send(()).unwrap();
    worker.join().unwrap();
    close.join().unwrap();
    assert_eq!(*order.lock().unwrap(), ["old control", "closed"]);
}

#[cfg(target_os = "macos")]
#[test]
fn the_mac_streams_what_was_asked_within_what_it_can_encode() {
    let request = legato_proto::ExtendRequest {
        width: 3840,
        height: 2160,
        hidpi: true,
        stream_width: 0,
        stream_height: 0,
        moving_width: 1920,
        moving_height: 1080,
        fps: 144,
        bitrate: 40_000_000,
    };
    let c = super::host::stream_config(request);
    assert_eq!((c.stream_width, c.stream_height), (3840, 2160));
    assert_eq!(c.moving, Some((1920, 1080)));
    assert_eq!((c.refresh, c.fps, c.sharp_fps), (144, 144, 60));
    // A moving size no smaller than the stream is no moving track at all.
    let c = super::host::stream_config(legato_proto::ExtendRequest {
        stream_width: 1920,
        stream_height: 1080,
        ..request
    });
    assert_eq!((c.moving, c.fps), (None, 144));
    let c = super::host::stream_config(legato_proto::ExtendRequest {
        moving_width: 0,
        moving_height: 0,
        ..request
    });
    assert_eq!((c.moving, c.refresh, c.fps), (None, 144, 60));
}

fn layout(offset: legato_proto::Point) -> legato_core::Layout {
    use legato_proto::{Display, Rect, Screens};
    let screens = Screens {
        native_per_desk: 1.0,
        displays: vec![Display {
            id: 1,
            bounds: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            pixel_scale: 1.0,
            ui_scale: 1.0,
            primary: true,
            name: String::new(),
        }],
    };
    let mut layout = legato_core::Layout::new(screens.clone());
    layout.set_machine(legato_core::MachineId(1), screens, offset);
    layout
}

#[test]
fn arrangement_refreshes_when_peer_moves_without_window_resize() {
    use legato_proto::{Point, Rect};
    let outputs = Outputs::new();
    let mut viewer = outputs.viewer(1);
    viewer.bounds = Some(Rect::new(0.0, -1080.0, 1920.0, 1080.0));
    viewer.area = Some(Rect::new(0.0, 0.0, 1920.0, 1080.0));
    let peer = legato_core::MachineId(1);
    let old = layout(Point::new(0.0, 1080.0));
    let new = layout(Point::new(1920.0, 0.0));
    let first = viewer.arrangement(&old, peer, None).unwrap();
    assert_eq!(viewer.arrangement(&old, peer, None), None);
    viewer.bounds.as_mut().unwrap().x += 3.0;
    assert_eq!(viewer.arrangement(&old, peer, None), None);
    let expected = legato_core::extend::place_extra_display(
        &new,
        peer,
        viewer.area.unwrap(),
        (1920.0, 1080.0),
    )
    .unwrap();
    assert_ne!(first.0, expected, "moving peer changes the required origin");
    assert_eq!(
        viewer.arrangement(&new, peer, None).map(|a| a.0),
        Some(expected)
    );
}

#[test]
fn arrangement_refreshes_when_native_client_rect_changes() {
    use legato_proto::{Point, Rect};
    let outputs = Outputs::new();
    let mut viewer = outputs.viewer(1);
    viewer.bounds = Some(Rect::new(0.0, -1080.0, 1920.0, 1080.0));
    let area = Rect::new(0.0, 0.0, 1920.0, 1080.0);
    viewer.area = Some(area);
    let peer = legato_core::MachineId(1);
    let layout = layout(Point::new(0.0, 1080.0));
    viewer.arrangement(&layout, peer, Some(area)).unwrap();
    let native = Rect::new(0.0, 30.0, 1920.0, 1050.0);
    let expected = legato_core::controller::fit_picture((1920.0, 1080.0), native);
    assert_eq!(
        viewer.arrangement(&layout, peer, Some(native)).map(|a| a.1),
        Some(expected)
    );
}

#[tokio::test]
async fn cursor_changes_are_independent_of_video_and_scoped_to_the_viewer() {
    use legato_proto::CursorShape::*;
    let outputs = Outputs::new();
    let mut cursor = outputs.cursor.subscribe();
    let old = outputs.viewer(1);
    cursor.borrow_and_update();
    let generation = old.generation;
    old.cursor(old.peer, generation, Text);
    cursor.changed().await.unwrap();
    assert_eq!(*cursor.borrow_and_update(), Text);
    assert_eq!(outputs.width(), None, "no video needed to publish a shape");
    old.cursor(old.peer, generation, Text);
    assert!(
        !cursor.has_changed().unwrap(),
        "unchanged shapes don't wake the UI"
    );
    old.cursor(old.peer, generation, Arrow);
    assert_eq!(
        *cursor.borrow_and_update(),
        Arrow,
        "an unavailable/custom shape clears the last one"
    );
    old.cursor(old.peer, generation, ResizeDiagonalDown);
    let late_worker = old.showing.clone();
    drop(old);
    assert_eq!(
        *cursor.borrow_and_update(),
        Arrow,
        "closing/disconnecting restores an arrow"
    );
    let new = outputs.viewer(1);
    assert_ne!(generation, new.generation);
    new.cursor(new.peer, new.generation, Pointer);
    assert_eq!(*cursor.borrow_and_update(), Pointer);
    new.cursor(new.peer, generation, Grabbing);
    new.cursor(
        iroh::SecretKey::from_bytes(&[2; 32]).public(),
        new.generation,
        Text,
    );
    late_worker.set_cursor(ResizeVertical);
    late_worker.close();
    assert_eq!(
        *cursor.borrow_and_update(),
        Pointer,
        "stale generation, wrong peer and old cleanup cannot change the new viewer"
    );
    new.cursor(new.peer, new.generation, ResizeHorizontal);
    assert_eq!(*cursor.borrow_and_update(), ResizeHorizontal);
    drop(new);
    assert_eq!(*cursor.borrow(), Arrow);
}

#[tokio::test(start_paused = true)]
async fn cursor_sampling_coalesces_changes_bounds_pending_reads_and_recovers_from_a_stall() {
    use legato_proto::CursorShape::*;
    let (reads, mut pending) = tokio::sync::mpsc::unbounded_channel();
    let (changes, mut received) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(super::mirror_cursor(
        move |reply| {
            reads.send(reply).unwrap();
        },
        move |shape| {
            changes.send(shape).unwrap();
        },
    ));
    pending.recv().await.unwrap().send(Pointer).unwrap();
    assert_eq!(received.recv().await, Some(Pointer));
    pending.recv().await.unwrap().send(Pointer).unwrap();
    tokio::task::yield_now().await;
    assert!(
        received.try_recv().is_err(),
        "don't resend an unchanged cursor"
    );
    let delayed = pending.recv().await.unwrap();
    tokio::time::advance(ms(250)).await;
    assert_eq!(
        received.recv().await,
        Some(Arrow),
        "a stalled AppKit read clears the previous shape"
    );
    tokio::time::advance(Duration::from_secs(10)).await;
    assert!(
        pending.try_recv().is_err(),
        "at most one main-thread read may be pending"
    );
    assert!(
        received.try_recv().is_err(),
        "don't repeat the fallback while waiting"
    );
    delayed.send(Text).unwrap();
    assert_eq!(
        received.recv().await,
        Some(Text),
        "resume when the main thread recovers"
    );
    let abandoned = pending.recv().await.unwrap();
    drop(abandoned);
    assert_eq!(
        received.recv().await,
        Some(Arrow),
        "a failed read resets before the task ends"
    );
    task.await.unwrap();
}
