//! Software-renderer integration test: decode a synthetic video and render
//! frames into memory.

#![cfg(feature = "render")]

use std::sync::mpsc;
use std::time::{Duration, Instant};

use rsmpv::render::RenderContext;
use rsmpv::{Event, Mpv};

#[test]
fn software_render_produces_pixels() {
    let mpv = Mpv::builder()
        .unwrap()
        .set_property("vo", "libmpv")
        .unwrap()
        .set_property("ao", "null")
        .unwrap()
        .build()
        .unwrap();

    let mut render = RenderContext::new_software(&mpv).unwrap();
    let (tx, rx) = mpsc::channel::<()>();
    render.set_update_callback(move || {
        let _ = tx.send(());
    });

    // Synthetic video via libavfilter; skip the test if this mpv build
    // doesn't support it.
    if mpv
        .command(&["loadfile", "av://lavfi:testsrc=duration=1:size=64x64"])
        .is_err()
    {
        eprintln!("skipping: lavfi not available");
        return;
    }

    // Drive events on another handle so this thread can keep rendering.
    let mut events = mpv.create_client(None).unwrap();

    const W: usize = 64;
    const H: usize = 64;
    let mut pixels = vec![0u8; W * H * 4];
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut rendered_something = false;

    'outer: while Instant::now() < deadline {
        // A failed load ends the test; EOF means we're done feeding frames.
        while let Some(ev) = events.wait_event(0.0) {
            if let Event::EndFile { error, .. } = ev {
                assert_eq!(error, None, "playback failed");
                break 'outer;
            }
        }
        if rx.recv_timeout(Duration::from_millis(100)).is_ok() && render.update() {
            render
                .render_software(W as i32, H as i32, "rgb0", W * 4, &mut pixels)
                .unwrap();
            if pixels.iter().any(|&b| b != 0) {
                rendered_something = true;
            }
        }
    }

    assert!(rendered_something, "no non-black frame was rendered");
}
