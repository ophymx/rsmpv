//! Software-renderer integration tests: decode a synthetic video and render
//! frames into memory, with each context flavor and event-drain mechanism.

#![cfg(feature = "render")]

use std::sync::mpsc;
use std::time::{Duration, Instant};

use rsmpv::{Error, Event, Mpv};

const W: usize = 64;
const H: usize = 64;

/// A core configured for embedded video: render-API output, no audio, and
/// the file looping forever so the render loop can't race EOF.
fn video_core() -> Mpv {
    let mpv = Mpv::builder()
        .unwrap()
        .set_property("vo", "libmpv")
        .unwrap()
        .set_property("ao", "null")
        .unwrap()
        .build()
        .unwrap();
    mpv.set_property("loop-file", "inf").unwrap();
    mpv
}

/// Queue the synthetic lavfi source. `loadfile` only queues the load, so
/// this succeeds even on mpv builds without lavfi — those surface as an
/// [`Event::EndFile`] carrying [`Error::LoadingFailed`] later, which
/// [`pump_until_rendered`] reports as [`Pump::LoadFailed`] (the skip
/// path).
fn load_test_video(mpv: &Mpv) {
    mpv.command(&["loadfile", "av://lavfi:testsrc=duration=1:size=64x64"])
        .unwrap();
}

/// Outcome of [`pump_until_rendered`].
enum Pump {
    /// A non-black frame landed in the pixel buffer.
    Rendered,
    /// The synthetic source failed to load — this mpv build can't play it
    /// (no lavfi); the test should skip.
    LoadFailed,
    /// The deadline expired without a non-black frame.
    TimedOut,
}

/// Drive rendering until a non-black frame lands in memory, draining
/// events through `drain` along the way. `step` should run
/// `update()` + `render_software()` into the buffer (panicking on render
/// errors) and return whether it rendered. Playback errors other than a
/// failed load panic.
fn pump_until_rendered(
    rx: &mpsc::Receiver<()>,
    mut drain: impl FnMut() -> Option<Event>,
    mut step: impl FnMut(&mut [u8]) -> bool,
) -> Pump {
    let mut pixels = vec![0u8; W * H * 4];
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        while let Some(ev) = drain() {
            if let Event::EndFile { error: Some(e), .. } = ev {
                // A load failure means the build can't play the source at
                // all (skip); anything else is a real playback regression.
                // A clean EndFile is NOT "no more frames coming": mpv
                // builds that loop an unseekable stream (like av://lavfi)
                // by reloading it emit one per lap, so keep pumping.
                assert_eq!(e, Error::LoadingFailed, "playback failed");
                return Pump::LoadFailed;
            }
        }
        if rx.recv_timeout(Duration::from_millis(100)).is_ok()
            && step(&mut pixels)
            && pixels.iter().any(|&b| b != 0)
        {
            return Pump::Rendered;
        }
    }
    Pump::TimedOut
}

/// A `step` closure for [`pump_until_rendered`]: process pending render
/// work and, when a frame is due, render it (unwrap surfaces real renderer
/// regressions immediately instead of a timeout).
macro_rules! render_step {
    ($render:ident) => {
        |pixels: &mut [u8]| {
            if $render.update() {
                $render
                    .render_software(W as i32, H as i32, "rgb0", W * 4, pixels)
                    .unwrap();
                true
            } else {
                false
            }
        }
    };
}

/// Borrowed context, blocking event loop on a secondary client handle.
#[test]
fn software_render_produces_pixels() {
    use rsmpv::render::RenderContext;

    let mpv = video_core();
    let mut render = RenderContext::new_software(&mpv).unwrap();
    let (tx, rx) = mpsc::channel::<()>();
    render.set_update_callback(move || {
        let _ = tx.send(());
    });

    // Create the event-drain client BEFORE queueing the load: libmpv only
    // delivers events to clients that exist when the event fires, so a
    // client created after loadfile could miss an early EndFile.
    let mut events = mpv.create_client(None).unwrap();
    load_test_video(&mpv);

    match pump_until_rendered(&rx, || events.wait_event(0.0), render_step!(render)) {
        Pump::Rendered => {}
        Pump::LoadFailed => eprintln!("skipping: this mpv can't play the lavfi source"),
        Pump::TimedOut => panic!("no non-black frame was rendered"),
    }
}

/// The owning-consumer shape the owned handles exist for: one Arc<Mpv>
/// shared between the render context and the event drain, no `&mut Mpv`
/// (and no secondary Client) anywhere.
#[test]
fn owned_render_context_with_shared_event_drain() {
    use rsmpv::render::OwnedRenderContext;
    use std::sync::Arc;

    let mpv = Arc::new(video_core());
    let mut render = OwnedRenderContext::new_software(&mpv).unwrap();
    let (tx, rx) = mpsc::channel::<()>();
    render.set_update_callback(move || {
        let _ = tx.send(());
    });

    // The main handle exists from the start, so its queue can't miss an
    // early EndFile the way a late-created client could.
    load_test_video(&mpv);

    match pump_until_rendered(&rx, || mpv.poll_event(), render_step!(render)) {
        Pump::Rendered => {}
        Pump::LoadFailed => eprintln!("skipping: this mpv can't play the lavfi source"),
        Pump::TimedOut => panic!("no non-black frame was rendered"),
    }
}

/// Structural teardown ordering: dropping the last user-visible Arc<Mpv>
/// while the context is alive defers termination; dropping the context
/// then frees the renderer and terminates the player, without deadlock.
#[test]
fn owned_render_context_defers_termination() {
    use rsmpv::render::OwnedRenderContext;
    use std::sync::Arc;

    let mpv = Arc::new(video_core());
    let render = OwnedRenderContext::new_software(&mpv).unwrap();
    drop(mpv); // core stays alive through the context's Arc
    assert!(render.core().get_property::<f64>("volume").is_ok());
    drop(render); // frees the renderer, then terminates the player
}

#[test]
fn owned_render_context_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<rsmpv::render::OwnedRenderContext>();
}
