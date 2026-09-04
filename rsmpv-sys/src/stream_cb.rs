//! Bindings for the mpv custom stream API (`mpv/stream_cb.h`).
//!
//! Lets mpv read from user-defined streams (in the spirit of `funopen` /
//! `fopencookie`). Register callbacks with [`mpv_stream_cb_add_ro`]; opening
//! a `protocol://...` URI then invokes the open callback, which fills out a
//! [`mpv_stream_cb_info`]. The header warns that this API is not stable yet.
//!
//! Contract points from the header, summarized:
//!
//! - The callbacks must not call libmpv APIs (deadlock), unless targeting a
//!   different mpv instance.
//! - A stream stays valid until libmpv calls its close callback; the client
//!   cannot force-close it, only return errors until libmpv gives up.
//! - Protocols stay registered until the mpv core is destroyed — potentially
//!   outliving the registering `mpv_handle`. After `mpv_terminate_destroy()`
//!   returns, the callbacks will not be called again.

use core::ffi::{c_char, c_int, c_void};

use crate::mpv_handle;

/// Read callback; semantics of `read(2)` in blocking mode. Short reads are
/// allowed; block until data is available; return the number of bytes read,
/// `0` for EOF, `-1` on error.
pub type mpv_stream_cb_read_fn =
    Option<unsafe extern "C" fn(cookie: *mut c_void, buf: *mut c_char, nbytes: u64) -> i64>;

/// Seek callback; returns the resulting offset, or `MPV_ERROR_UNSUPPORTED` /
/// `MPV_ERROR_GENERIC` on failure. mpv seeks to position 0 right after
/// opening to probe seekability. May be null, which behaves as always
/// returning `MPV_ERROR_UNSUPPORTED`.
pub type mpv_stream_cb_seek_fn =
    Option<unsafe extern "C" fn(cookie: *mut c_void, offset: i64) -> i64>;

/// Size callback; returns the total size of the stream in bytes, or
/// `MPV_ERROR_UNSUPPORTED` if unknown. May be null, which behaves as always
/// returning `MPV_ERROR_UNSUPPORTED`.
pub type mpv_stream_cb_size_fn = Option<unsafe extern "C" fn(cookie: *mut c_void) -> i64>;

/// Close callback; terminates the stream and releases the cookie.
pub type mpv_stream_cb_close_fn = Option<unsafe extern "C" fn(cookie: *mut c_void)>;

/// Cancel callback; interrupts current and future read/seek operations. It is
/// called from a different thread than the demuxer and must not block. May be
/// null.
pub type mpv_stream_cb_cancel_fn = Option<unsafe extern "C" fn(cookie: *mut c_void)>;

/// Filled out by the open callback (see [`mpv_stream_cb_open_ro_fn`]). Only
/// valid for the duration of that callback; the callbacks and cookie cannot
/// be changed later.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct mpv_stream_cb_info {
    /// Opaque user value passed to the other callbacks; released by the close
    /// callback. Not interpreted by mpv (need not be a valid pointer).
    pub cookie: *mut c_void,
    /// Mandatory.
    pub read_fn: mpv_stream_cb_read_fn,
    /// Optional (null behaves as unseekable).
    pub seek_fn: mpv_stream_cb_seek_fn,
    /// Optional (null behaves as unknown size).
    pub size_fn: mpv_stream_cb_size_fn,
    /// Mandatory.
    pub close_fn: mpv_stream_cb_close_fn,
    /// Optional.
    pub cancel_fn: mpv_stream_cb_cancel_fn,
}

/// Open callback for a custom read-only stream. Must fill out `info`
/// (callbacks and optionally the cookie). Return `0` on success or
/// `MPV_ERROR_LOADING_FAILED` if the URI cannot be opened.
pub type mpv_stream_cb_open_ro_fn = Option<
    unsafe extern "C" fn(
        user_data: *mut c_void,
        uri: *mut c_char,
        info: *mut mpv_stream_cb_info,
    ) -> c_int,
>;

extern "C" {
    /// Register a read-only stream protocol handler under the given prefix
    /// (e.g. `"foo"` for `foo://` URIs). `user_data` is passed to `open_fn`.
    /// Returns `MPV_ERROR_INVALID_PARAMETER` if the protocol is already
    /// registered. The registration lasts until the mpv core is destroyed.
    pub fn mpv_stream_cb_add_ro(
        ctx: *mut mpv_handle,
        protocol: *const c_char,
        user_data: *mut c_void,
        open_fn: mpv_stream_cb_open_ro_fn,
    ) -> c_int;
}
