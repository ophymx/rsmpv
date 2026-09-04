//! Headless integration tests against a real libmpv.

use std::time::{Duration, Instant};

use rsmpv::{Event, Format, Mpv, Node, PropertyData};

fn headless() -> Mpv {
    Mpv::builder()
        .unwrap()
        .set_property("vo", "null")
        .unwrap()
        .set_property("ao", "null")
        .unwrap()
        .build()
        .unwrap()
}

/// Pump events until `f` returns `Some`, or panic after `secs` seconds.
fn wait_for<T>(mpv: &mut Mpv, secs: u64, mut f: impl FnMut(Event) -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if let Some(ev) = mpv.wait_event(0.2) {
            if let Some(out) = f(ev) {
                return out;
            }
        }
    }
    panic!("timed out waiting for event");
}

#[test]
fn version_and_client_identity() {
    let (major, _minor) = rsmpv::client_api_version();
    assert!(major >= 1);

    let mpv = headless();
    assert!(!mpv.client_name().is_empty());
    assert!(mpv.client_id() > 0);
    let version: String = mpv.get_property("mpv-version").unwrap();
    assert!(version.starts_with("mpv"), "version: {version}");
}

#[test]
fn typed_property_round_trips() {
    let mpv = headless();

    mpv.set_property("pause", true).unwrap();
    assert!(mpv.get_property::<bool>("pause").unwrap());

    mpv.set_property("volume", 42.5).unwrap();
    assert_eq!(mpv.get_property::<f64>("volume").unwrap(), 42.5);

    mpv.set_property("osd-level", 3i64).unwrap();
    assert_eq!(mpv.get_property::<i64>("osd-level").unwrap(), 3);

    mpv.set_property("speed", "1.5").unwrap();
    assert_eq!(mpv.get_property::<f64>("speed").unwrap(), 1.5);

    let err = mpv.get_property::<i64>("this-property-does-not-exist");
    assert_eq!(err.unwrap_err(), rsmpv::Error::PropertyNotFound);

    let osd = mpv.get_property_osd_string("volume").unwrap();
    assert!(!osd.is_empty());
}

#[test]
fn node_properties_and_commands() {
    let mpv = headless();

    // Structured read of a map-valued property.
    match mpv.get_property::<Node>("option-info/volume").unwrap() {
        Node::Map(entries) => assert!(entries.iter().any(|(k, _)| k == "name")),
        other => panic!("expected map, got {other:?}"),
    }

    // Structured write.
    mpv.set_property("volume", Node::Double(31.0)).unwrap();
    assert_eq!(mpv.get_property::<f64>("volume").unwrap(), 31.0);

    // Command with a structured return value.
    let result = mpv
        .command_node(&Node::Array(vec![
            Node::from("expand-text"),
            Node::from("vol=${volume}"),
        ]))
        .unwrap();
    assert_eq!(result, Node::String("vol=31".into()));

    // Same command through the string-argv path.
    let result = mpv.command_ret(&["expand-text", "x${volume}"]).unwrap();
    assert_eq!(result, Node::String("x31".into()));
}

#[test]
fn observe_property_delivers_changes() {
    let mut mpv = headless();
    mpv.observe_property(7, "volume", Format::Double).unwrap();

    // Initial notification.
    let initial = wait_for(&mut mpv, 10, |ev| match ev {
        Event::PropertyChange {
            userdata: 7,
            name,
            data: PropertyData::Double(v),
        } if name == "volume" => Some(v),
        _ => None,
    });
    assert_eq!(initial, 100.0);

    mpv.set_property("volume", 55.0).unwrap();
    let changed = wait_for(&mut mpv, 10, |ev| match ev {
        Event::PropertyChange {
            userdata: 7,
            data: PropertyData::Double(v),
            ..
        } if v != 100.0 => Some(v),
        _ => None,
    });
    assert_eq!(changed, 55.0);

    assert_eq!(mpv.unobserve_property(7).unwrap(), 1);
}

#[test]
fn async_property_and_command_replies() {
    let mut mpv = headless();

    mpv.get_property_async::<f64>(11, "volume").unwrap();
    let (name, data) = wait_for(&mut mpv, 10, |ev| match ev {
        Event::GetPropertyReply {
            userdata: 11,
            result,
        } => Some(result.unwrap()),
        _ => None,
    });
    assert_eq!(name, "volume");
    assert_eq!(data, PropertyData::Double(100.0));

    mpv.set_property_async(12, "volume", 60.0).unwrap();
    wait_for(&mut mpv, 10, |ev| match ev {
        Event::SetPropertyReply {
            userdata: 12,
            result,
        } => {
            result.unwrap();
            Some(())
        }
        _ => None,
    });
    assert_eq!(mpv.get_property::<f64>("volume").unwrap(), 60.0);

    mpv.command_async(13, &["expand-text", "ok"]).unwrap();
    let node = wait_for(&mut mpv, 10, |ev| match ev {
        Event::CommandReply {
            userdata: 13,
            result,
        } => Some(result.unwrap()),
        _ => None,
    });
    assert_eq!(node, Node::String("ok".into()));
}

#[test]
fn secondary_client_handles() {
    let mpv = headless();
    let client = mpv.create_client(Some("helper")).unwrap();
    assert!(client.client_name().starts_with("helper"));
    assert_ne!(client.client_id(), mpv.client_id());
    // The client controls the same core.
    client.set_property("volume", 20.0).unwrap();
    assert_eq!(mpv.get_property::<f64>("volume").unwrap(), 20.0);
}

#[test]
fn wakeup_callback_fires() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let mpv = headless();
    let woke = Arc::new(AtomicBool::new(false));
    let woke2 = woke.clone();
    mpv.set_wakeup_callback(move || woke2.store(true, Ordering::SeqCst));
    // Generate an event.
    mpv.observe_property(1, "volume", Format::Double).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !woke.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "wakeup callback never fired");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A minimal 16-bit mono PCM WAV file of silence.
#[cfg(feature = "stream-cb")]
fn tiny_wav() -> Vec<u8> {
    let sample_rate: u32 = 8000;
    let samples: u32 = 1600; // 0.2 s
    let data_len = samples * 2;
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    wav.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.resize(wav.len() + data_len as usize, 0);
    wav
}

#[cfg(feature = "stream-cb")]
#[test]
fn custom_stream_protocol_plays_to_eof() {
    use rsmpv::stream_cb::{IoStream, Stream};
    use rsmpv::EndFileReason;

    let mut mpv = headless();
    mpv.register_protocol("rsmpvtest", |uri| {
        assert_eq!(uri, "rsmpvtest://tone");
        Ok(Box::new(IoStream(std::io::Cursor::new(tiny_wav()))) as Box<dyn Stream>)
    })
    .unwrap();

    // Registering the same protocol again fails.
    let dup = mpv.register_protocol("rsmpvtest", |_| Err(rsmpv::Error::LoadingFailed));
    assert_eq!(dup.unwrap_err(), rsmpv::Error::InvalidParameter);

    mpv.command(&["loadfile", "rsmpvtest://tone"]).unwrap();
    let reason = wait_for(&mut mpv, 30, |ev| match ev {
        Event::EndFile { reason, error, .. } => {
            assert_eq!(error, None);
            Some(reason)
        }
        _ => None,
    });
    assert_eq!(reason, EndFileReason::Eof);
}

/// Soundness regression: a (safe) Stream impl that violates the read
/// contract by over-reporting the byte count must not be able to walk mpv's
/// buffer accounting past the end of its buffer. The clamp in the read
/// trampoline caps the count; the test asserts we get through the load
/// without memory corruption, whatever the decode outcome.
#[cfg(feature = "stream-cb")]
#[test]
fn overreporting_stream_is_clamped() {
    use rsmpv::stream_cb::Stream;

    struct Liar(std::io::Cursor<Vec<u8>>);
    impl Stream for Liar {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = std::io::Read::read(&mut self.0, buf)?;
            Ok(if n == 0 { 0 } else { usize::MAX })
        }
    }

    let mut mpv = headless();
    mpv.register_protocol("rsmpvliar", |_| {
        Ok(Box::new(Liar(std::io::Cursor::new(tiny_wav()))) as Box<dyn Stream>)
    })
    .unwrap();

    mpv.command(&["loadfile", "rsmpvliar://x"]).unwrap();
    wait_for(&mut mpv, 30, |ev| match ev {
        Event::EndFile { .. } => Some(()),
        _ => None,
    });
}
