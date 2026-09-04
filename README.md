# rsmpv

Clean-room Rust bindings for [libmpv](https://mpv.io/), the embeddable mpv
media player.

These crates were written **only** from the ISC-licensed client API headers
of the mpv project (`client.h`, `render.h`, `render_gl.h`, `stream_cb.h`) —
which exist expressly to enable third-party wrappers — without reference to
any existing LGPL Rust binding. The crates themselves are ISC licensed, so
the license encumbrance added on top of libmpv itself is minimal.

> **Note**: the mpv *library* you link against is GPLv2+ by default, or
> LGPLv2.1+ when mpv is built with `-Dgpl=false`. These bindings don't (and
> can't) change that.

## Crates

| Crate | Description |
|---|---|
| [`rsmpv-sys`](rsmpv-sys/) | Hand-written, `#![no_std]` FFI declarations mirroring the C API one-to-one (client API 2.5). No bindgen, no libclang, no vendored headers. |
| [`rsmpv`](rsmpv/) | Safe, idiomatic wrapper: typed properties, owned events and nodes, RAII handles, custom stream protocols, and the render API. |

## Quick start

```rust
use rsmpv::{Event, Format, Mpv};

let mut mpv = Mpv::builder()?
    .set_property("vo", "null")?
    .set_property("ao", "null")?
    .build()?;

mpv.command(&["loadfile", "video.mkv"])?;
mpv.observe_property(1, "playback-time", Format::Double)?;

loop {
    // None = timeout or spurious wakeup; keep waiting.
    match mpv.wait_event(-1.0) {
        Some(Event::PropertyChange { name, data, .. }) => println!("{name}: {data:?}"),
        Some(Event::EndFile { .. }) | Some(Event::Shutdown) => break,
        _ => {}
    }
}
```

## Cargo features

`rsmpv` (opt-in) / `rsmpv-sys` (on by default):

- `render` — the render API for embedding video (OpenGL or software
  surfaces; `mpv/render.h`, `mpv/render_gl.h`)
- `stream-cb` — custom stream protocols backed by Rust I/O
  (`mpv/stream_cb.h`)

## Building

`rsmpv-sys` links against the system libmpv found via `pkg-config`
(package `mpv`). Overrides:

- `MPV_LIBRARY_DIR` — directory containing the mpv library; skips pkg-config
- `MPV_NO_PKG_CONFIG` — skip pkg-config and emit a plain `-lmpv`

The bindings are hand-written, so no C headers, bindgen, or libclang are
needed at build time. Minimum supported Rust version: 1.77. Minimum
supported libmpv: client API 1.108 (mpv 0.33); the bindings are written
against client API 2.5.

## Testing

`cargo test --workspace --all-features` runs against the real libmpv,
including ABI layout checks (verified against the C headers with
`sizeof`/`offsetof`), headless playback through a custom Rust stream
protocol, and software rendering of a synthetic video.

## License

- `rsmpv-sys`: [ISC](rsmpv-sys/LICENSE), preserving the mpv developers'
  copyright notice from the headers the bindings were derived from.
- `rsmpv`: [MIT](rsmpv/LICENSE-MIT) OR [Apache-2.0](rsmpv/LICENSE-APACHE),
  at your option, following Rust ecosystem convention.
