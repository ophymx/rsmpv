//! Safe wrapper for custom stream protocols (`stream_cb`).
//!
//! Lets mpv read media through your own I/O: register a protocol with
//! [`Mpv::register_protocol`], then play `myproto://anything` URIs. mpv
//! calls your open function, and the returned [`Stream`] serves reads (and
//! optionally seeks) until mpv closes it.
//!
//! Notes inherited from the C API:
//!
//! - The upstream header warns this API is not stable yet.
//! - Stream methods are called from mpv's demuxer thread and must not call
//!   back into libmpv (deadlock).
//! - A registered protocol lives until the player is destroyed and cannot
//!   be unregistered; this wrapper keeps the open function alive on the
//!   [`Mpv`] and frees it after the player shuts down.
//! - Streams cannot be force-closed from your side; return errors until mpv
//!   gives up. (The C `cancel_fn` is not exposed, as it is called
//!   concurrently with `read`/`seek` and doesn't fit `&mut self` — use
//!   internal synchronization in your `Stream` if you need cancellation.)

use std::ffi::{c_void, CStr, CString};
use std::io;
use std::os::raw::{c_char, c_int};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Mutex;

use crate::error::{check, Error, Result};
use crate::escaped::EscapedBox;
use crate::Mpv;

/// A user-implemented, read-only media stream served to mpv.
///
/// Semantics follow POSIX `read(2)`/`lseek(2)` in blocking mode.
pub trait Stream: Send + 'static {
    /// Read up to `buf.len()` bytes. Short reads are fine (mpv retries);
    /// block until data is available; return `Ok(0)` for EOF. Errors are
    /// reported to mpv as a read error.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;

    /// Seek to the absolute position `offset` and return the resulting
    /// position. mpv probes seekability by seeking to `0` right after
    /// opening. The default implementation reports the stream as
    /// unseekable ([`io::ErrorKind::Unsupported`]).
    fn seek(&mut self, offset: u64) -> io::Result<u64> {
        let _ = offset;
        Err(io::ErrorKind::Unsupported.into())
    }

    /// Total size of the stream in bytes, if known.
    fn size(&mut self) -> Option<u64> {
        None
    }
}

/// Adapter exposing any `Read + Seek` type (e.g. `std::fs::File`,
/// `std::io::Cursor`) as a [`Stream`].
pub struct IoStream<T>(pub T);

impl<T: io::Read + io::Seek + Send + 'static> Stream for IoStream<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
    fn seek(&mut self, offset: u64) -> io::Result<u64> {
        self.0.seek(io::SeekFrom::Start(offset))
    }
    fn size(&mut self) -> Option<u64> {
        let pos = self.0.stream_position().ok()?;
        let size = self.0.seek(io::SeekFrom::End(0)).ok()?;
        self.0.seek(io::SeekFrom::Start(pos)).ok()?;
        Some(size)
    }
}

pub(crate) type OpenFn =
    Box<dyn Fn(&str) -> std::result::Result<Box<dyn Stream>, Error> + Send + Sync>;

/// Owns the open-callback allocations registered with libmpv
/// ([`EscapedBox`] because libmpv holds each pointer and calls through it
/// concurrently with moves of the owning [`Mpv`]). See the
/// `Mpv::protocols` field for why this lives on `Mpv` and not `Handle`.
///
/// [`Mpv`]: crate::Mpv
pub(crate) type ProtocolRegistry = Mutex<Vec<EscapedBox<OpenFn>>>;

type Cookie = Box<dyn Stream>;

unsafe extern "C" fn open_trampoline(
    user_data: *mut c_void,
    uri: *mut c_char,
    info: *mut rsmpv_sys::mpv_stream_cb_info,
) -> c_int {
    let open = &*(user_data as *const OpenFn);
    let uri = CStr::from_ptr(uri).to_string_lossy();
    let result = catch_unwind(AssertUnwindSafe(|| open(&uri)));
    match result {
        Ok(Ok(stream)) => {
            let cookie: *mut Cookie = Box::into_raw(Box::new(stream));
            (*info).cookie = cookie as *mut c_void;
            (*info).read_fn = Some(read_trampoline);
            (*info).seek_fn = Some(seek_trampoline);
            (*info).size_fn = Some(size_trampoline);
            (*info).close_fn = Some(close_trampoline);
            (*info).cancel_fn = None;
            0
        }
        _ => rsmpv_sys::MPV_ERROR_LOADING_FAILED,
    }
}

unsafe extern "C" fn read_trampoline(cookie: *mut c_void, buf: *mut c_char, nbytes: u64) -> i64 {
    let stream = &mut *(cookie as *mut Cookie);
    // Slices must not exceed isize::MAX bytes.
    let len = usize::try_from(nbytes)
        .unwrap_or(usize::MAX)
        .min(isize::MAX as usize);
    let buf = std::slice::from_raw_parts_mut(buf as *mut u8, len);
    match catch_unwind(AssertUnwindSafe(|| stream.read(buf))) {
        // A safe Stream impl may (wrongly) report more bytes than the buffer
        // holds; never forward a count mpv would trust past the buffer end.
        Ok(Ok(n)) => n.min(buf.len()) as i64,
        _ => -1,
    }
}

unsafe extern "C" fn seek_trampoline(cookie: *mut c_void, offset: i64) -> i64 {
    let stream = &mut *(cookie as *mut Cookie);
    let offset = match u64::try_from(offset) {
        Ok(o) => o,
        Err(_) => return rsmpv_sys::MPV_ERROR_GENERIC as i64,
    };
    match catch_unwind(AssertUnwindSafe(|| stream.seek(offset))) {
        Ok(Ok(pos)) => i64::try_from(pos).unwrap_or(rsmpv_sys::MPV_ERROR_GENERIC as i64),
        Ok(Err(e)) if e.kind() == io::ErrorKind::Unsupported => {
            rsmpv_sys::MPV_ERROR_UNSUPPORTED as i64
        }
        _ => rsmpv_sys::MPV_ERROR_GENERIC as i64,
    }
}

unsafe extern "C" fn size_trampoline(cookie: *mut c_void) -> i64 {
    let stream = &mut *(cookie as *mut Cookie);
    match catch_unwind(AssertUnwindSafe(|| stream.size())) {
        Ok(Some(size)) => i64::try_from(size).unwrap_or(rsmpv_sys::MPV_ERROR_UNSUPPORTED as i64),
        _ => rsmpv_sys::MPV_ERROR_UNSUPPORTED as i64,
    }
}

unsafe extern "C" fn close_trampoline(cookie: *mut c_void) {
    let stream = Box::from_raw(cookie as *mut Cookie);
    let _ = catch_unwind(AssertUnwindSafe(move || drop(stream)));
}

impl Mpv {
    /// Register a read-only stream protocol. Playing a
    /// `<protocol>://<rest>` URI invokes `open` with the full URI; return a
    /// [`Stream`] to serve it, or an error to make the load fail.
    ///
    /// Fails with [`Error::InvalidParameter`] if the protocol is already
    /// registered (by you or mpv). The registration lasts for the lifetime
    /// of the player and cannot be undone.
    ///
    /// ```no_run
    /// # use rsmpv::{Mpv, stream_cb::IoStream};
    /// # let mpv = Mpv::new().unwrap();
    /// mpv.register_protocol("myfs", |uri| {
    ///     let path = uri.strip_prefix("myfs://").unwrap_or(uri);
    ///     let file = std::fs::File::open(path).map_err(|_| rsmpv::Error::LoadingFailed)?;
    ///     Ok(Box::new(IoStream(file)) as Box<dyn rsmpv::stream_cb::Stream>)
    /// })?;
    /// mpv.command(&["loadfile", "myfs:///data/video.mkv"])?;
    /// # Ok::<(), rsmpv::Error>(())
    /// ```
    pub fn register_protocol<F>(&self, protocol: &str, open: F) -> Result<()>
    where
        F: Fn(&str) -> std::result::Result<Box<dyn Stream>, Error> + Send + Sync + 'static,
    {
        let protocol = CString::new(protocol).map_err(|_| Error::InteriorNul)?;
        let open = EscapedBox::new(Box::new(Box::new(open) as OpenFn));
        check(unsafe {
            rsmpv_sys::mpv_stream_cb_add_ro(
                self.as_raw(),
                protocol.as_ptr(),
                open.as_ptr() as *mut c_void,
                Some(open_trampoline),
            )
        })?;
        // Success: mpv now holds the pointer and the registration can't be
        // undone; the registry keeps the closure alive until after the
        // core is destroyed. (On failure mpv does not retain the pointer,
        // so `open` dropping above just frees it.)
        crate::lock_ignore_poison(&self.protocols).push(open);
        Ok(())
    }
}
