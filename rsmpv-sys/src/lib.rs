//! Low-level FFI bindings for the libmpv client API.
//!
//! These bindings are written by hand against the ISC-licensed client headers
//! of the mpv project (`mpv/client.h`, `mpv/render.h`, `mpv/render_gl.h`,
//! `mpv/stream_cb.h`), targeting client API version 2.5. No other mpv source
//! code was used. Note that while the client API headers are ISC, the mpv
//! *library* you link against is GPLv2+ by default (LGPLv2.1+ when mpv is
//! built with `-Dgpl=false`).
//!
//! The event structs match client API 1.108 and newer (mpv 0.33+); linking
//! against an older libmpv risks out-of-bounds reads of the fields added in
//! 1.108 (e.g. in `mpv_event_end_file`).
//!
//! Everything here mirrors the C API one-to-one: C identifiers are kept
//! verbatim, enums are represented as integer type aliases plus constants
//! (matching the C ABI and staying total over values added by future mpv
//! releases), and no safety layer is added. See the mpv client API
//! documentation for the authoritative semantics of each item; the doc
//! comments here only summarize the contract relevant for FFI use
//! (ownership, lifetime, threading).
//!
//! Cargo features:
//! - `render` (default): the render API (`render.h` + `render_gl.h`)
//! - `stream-cb` (default): custom stream protocols (`stream_cb.h`)

#![no_std]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(clippy::missing_safety_doc)]

use core::ffi::{c_char, c_int, c_ulong, c_void};

#[cfg(feature = "render")]
mod render;
#[cfg(feature = "render")]
pub use render::*;

#[cfg(feature = "stream-cb")]
mod stream_cb;
#[cfg(feature = "stream-cb")]
pub use stream_cb::*;

/// Build a client API version number from a major/minor pair, mirroring the
/// C macro `MPV_MAKE_VERSION`. The high 16 bits are the major version, the
/// low 16 bits the minor version.
pub const fn MPV_MAKE_VERSION(major: c_ulong, minor: c_ulong) -> c_ulong {
    (major << 16) | minor
}

/// The client API version these bindings were written against.
///
/// Compare against [`mpv_client_api_version`] at runtime if you need to know
/// what the loaded library was built with.
pub const MPV_CLIENT_API_VERSION: c_ulong = MPV_MAKE_VERSION(2, 5);

/// Opaque per-client handle. Created by [`mpv_create`] and friends, destroyed
/// by [`mpv_destroy`] or [`mpv_terminate_destroy`].
#[repr(C)]
pub struct mpv_handle {
    _opaque: [u8; 0],
}

/// Error codes returned by API functions. `0` and positive values always mean
/// success, negative values are always errors.
pub type mpv_error = c_int;

/// No error / success.
pub const MPV_ERROR_SUCCESS: mpv_error = 0;
/// The event ringbuffer is full; the client can't receive any more events.
pub const MPV_ERROR_EVENT_QUEUE_FULL: mpv_error = -1;
/// Memory allocation failed.
pub const MPV_ERROR_NOMEM: mpv_error = -2;
/// The mpv core has not been configured and initialized yet.
pub const MPV_ERROR_UNINITIALIZED: mpv_error = -3;
/// Catch-all for invalid or unsupported parameter values.
pub const MPV_ERROR_INVALID_PARAMETER: mpv_error = -4;
/// Tried to set an option that doesn't exist.
pub const MPV_ERROR_OPTION_NOT_FOUND: mpv_error = -5;
/// Tried to set an option using an unsupported format.
pub const MPV_ERROR_OPTION_FORMAT: mpv_error = -6;
/// Setting the option failed (e.g. the value could not be parsed).
pub const MPV_ERROR_OPTION_ERROR: mpv_error = -7;
/// The accessed property doesn't exist.
pub const MPV_ERROR_PROPERTY_NOT_FOUND: mpv_error = -8;
/// Tried to get/set a property using an unsupported format.
pub const MPV_ERROR_PROPERTY_FORMAT: mpv_error = -9;
/// The property exists but is currently unavailable.
pub const MPV_ERROR_PROPERTY_UNAVAILABLE: mpv_error = -10;
/// Error getting or setting a property.
pub const MPV_ERROR_PROPERTY_ERROR: mpv_error = -11;
/// Error running a command.
pub const MPV_ERROR_COMMAND: mpv_error = -12;
/// Generic loading error (usually seen in `mpv_event_end_file.error`).
pub const MPV_ERROR_LOADING_FAILED: mpv_error = -13;
/// Initializing the audio output failed.
pub const MPV_ERROR_AO_INIT_FAILED: mpv_error = -14;
/// Initializing the video output failed.
pub const MPV_ERROR_VO_INIT_FAILED: mpv_error = -15;
/// No audio or video data to play (or no streams selected).
pub const MPV_ERROR_NOTHING_TO_PLAY: mpv_error = -16;
/// File format could not be determined, or the file was too broken to open.
pub const MPV_ERROR_UNKNOWN_FORMAT: mpv_error = -17;
/// Certain system requirements are not fulfilled.
pub const MPV_ERROR_UNSUPPORTED: mpv_error = -18;
/// The called API function is only a stub.
pub const MPV_ERROR_NOT_IMPLEMENTED: mpv_error = -19;
/// Unspecified error.
pub const MPV_ERROR_GENERIC: mpv_error = -20;

/// Data format used by the property/option accessors and [`mpv_node`].
pub type mpv_format = c_int;

/// Invalid/empty. Guaranteed to be `0`, so zero-initialized values read as
/// "none".
pub const MPV_FORMAT_NONE: mpv_format = 0;
/// Basic type `char*` — the raw property string. Strings returned by mpv for
/// this format are freed with [`mpv_free`]. Not guaranteed to be valid UTF-8
/// (filenames, tags).
pub const MPV_FORMAT_STRING: mpv_format = 1;
/// Basic type `char*` — the human-readable OSD string. Read access only.
pub const MPV_FORMAT_OSD_STRING: mpv_format = 2;
/// Basic type `int` — only `0` ("no") and `1` ("yes") are allowed.
pub const MPV_FORMAT_FLAG: mpv_format = 3;
/// Basic type `int64_t`.
pub const MPV_FORMAT_INT64: mpv_format = 4;
/// Basic type `double`.
pub const MPV_FORMAT_DOUBLE: mpv_format = 5;
/// The type is [`mpv_node`]. Values read from mpv must be released with
/// [`mpv_free_node_contents`]; values you construct yourself are owned and
/// freed by you.
pub const MPV_FORMAT_NODE: mpv_format = 6;
/// Only used inside [`mpv_node`] (the node holds an array `mpv_node_list`).
pub const MPV_FORMAT_NODE_ARRAY: mpv_format = 7;
/// Only used inside [`mpv_node`] (the node holds a map `mpv_node_list`).
pub const MPV_FORMAT_NODE_MAP: mpv_format = 8;
/// A raw, untyped byte array; only used inside [`mpv_node`].
pub const MPV_FORMAT_BYTE_ARRAY: mpv_format = 9;

/// The value union of [`mpv_node`]. Which field is valid is determined by
/// `mpv_node.format`.
#[repr(C)]
#[derive(Clone, Copy)]
pub union mpv_node_u {
    /// Valid if `format == MPV_FORMAT_STRING`.
    pub string: *mut c_char,
    /// Valid if `format == MPV_FORMAT_FLAG`.
    pub flag: c_int,
    /// Valid if `format == MPV_FORMAT_INT64`.
    pub int64: i64,
    /// Valid if `format == MPV_FORMAT_DOUBLE`.
    pub double_: f64,
    /// Valid if `format == MPV_FORMAT_NODE_ARRAY` or `MPV_FORMAT_NODE_MAP`.
    pub list: *mut mpv_node_list,
    /// Valid if `format == MPV_FORMAT_BYTE_ARRAY`.
    pub ba: *mut mpv_byte_array,
}

/// Generic data storage for properties, command arguments, and command
/// results.
///
/// If mpv wrote the node (e.g. `mpv_get_property` with [`MPV_FORMAT_NODE`]),
/// do not mutate it and release it with [`mpv_free_node_contents`]. If you
/// filled it yourself, you own the memory and must not call
/// `mpv_free_node_contents` on it.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct mpv_node {
    pub u: mpv_node_u,
    /// Discriminant for `u`. One of `MPV_FORMAT_NONE`, `MPV_FORMAT_STRING`,
    /// `MPV_FORMAT_FLAG`, `MPV_FORMAT_INT64`, `MPV_FORMAT_DOUBLE`,
    /// `MPV_FORMAT_NODE_ARRAY`, `MPV_FORMAT_NODE_MAP`,
    /// `MPV_FORMAT_BYTE_ARRAY`. Treat unknown values as opaque.
    pub format: mpv_format,
}

/// An array or map of [`mpv_node`] values (see [`MPV_FORMAT_NODE_ARRAY`] and
/// [`MPV_FORMAT_NODE_MAP`]).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct mpv_node_list {
    /// Number of entries. Never negative.
    pub num: c_int,
    /// For arrays and maps: `values[0..num]` are the item/entry values. May be
    /// null if `num == 0`.
    pub values: *mut mpv_node,
    /// For maps only: `keys[0..num]` are the entry keys (never null entries,
    /// unordered, `keys[n]` pairs with `values[n]`). Unused/null for arrays.
    pub keys: *mut *mut c_char,
}

/// A raw byte array (see [`MPV_FORMAT_BYTE_ARRAY`]).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct mpv_byte_array {
    /// Pointer to the data. The interpretation is context-dependent.
    pub data: *mut c_void,
    /// Size of the data in bytes.
    pub size: usize,
}

/// Event IDs returned in [`mpv_event`]. Future mpv releases can add new
/// events; unknown IDs should be ignored.
pub type mpv_event_id = c_int;

/// Nothing happened (timeout or spurious wakeup).
pub const MPV_EVENT_NONE: mpv_event_id = 0;
/// The player is quitting; the client should call [`mpv_destroy`] soon.
pub const MPV_EVENT_SHUTDOWN: mpv_event_id = 1;
/// A log message (see [`mpv_request_log_messages`]); data is
/// `*mut mpv_event_log_message`.
pub const MPV_EVENT_LOG_MESSAGE: mpv_event_id = 2;
/// Reply to [`mpv_get_property_async`]; data is `*mut mpv_event_property`.
pub const MPV_EVENT_GET_PROPERTY_REPLY: mpv_event_id = 3;
/// Reply to [`mpv_set_property_async`] (no event data).
pub const MPV_EVENT_SET_PROPERTY_REPLY: mpv_event_id = 4;
/// Reply to [`mpv_command_async`] / [`mpv_command_node_async`]; data is
/// `*mut mpv_event_command`.
pub const MPV_EVENT_COMMAND_REPLY: mpv_event_id = 5;
/// Sent before playback start of a file; data is `*mut mpv_event_start_file`.
pub const MPV_EVENT_START_FILE: mpv_event_id = 6;
/// Sent after playback end of a file; data is `*mut mpv_event_end_file`.
pub const MPV_EVENT_END_FILE: mpv_event_id = 7;
/// The file has been loaded and decoding starts.
pub const MPV_EVENT_FILE_LOADED: mpv_event_id = 8;
/// Idle mode was entered.
#[deprecated(note = "observe the \"idle-active\" property instead")]
pub const MPV_EVENT_IDLE: mpv_event_id = 11;
/// Sent after a video frame was displayed.
#[deprecated(note = "observe a property such as \"playback-time\" instead")]
pub const MPV_EVENT_TICK: mpv_event_id = 14;
/// Triggered by the `script-message` input command; data is
/// `*mut mpv_event_client_message`.
pub const MPV_EVENT_CLIENT_MESSAGE: mpv_event_id = 16;
/// Video was reconfigured (resolution, pixel format, or filter changes).
pub const MPV_EVENT_VIDEO_RECONFIG: mpv_event_id = 17;
/// Audio was reconfigured.
pub const MPV_EVENT_AUDIO_RECONFIG: mpv_event_id = 18;
/// A seek was initiated; playback resumes with
/// [`MPV_EVENT_PLAYBACK_RESTART`] once done.
pub const MPV_EVENT_SEEK: mpv_event_id = 20;
/// Playback was reinitialized after a discontinuity (start of playback,
/// after seeking).
pub const MPV_EVENT_PLAYBACK_RESTART: mpv_event_id = 21;
/// An observed property (see [`mpv_observe_property`]) may have changed;
/// data is `*mut mpv_event_property`.
pub const MPV_EVENT_PROPERTY_CHANGE: mpv_event_id = 22;
/// The per-handle event ringbuffer overflowed and at least one event was
/// dropped.
pub const MPV_EVENT_QUEUE_OVERFLOW: mpv_event_id = 24;
/// A hook registered with [`mpv_hook_add`] was invoked; must be continued
/// with [`mpv_hook_continue`]. Data is `*mut mpv_event_hook`.
pub const MPV_EVENT_HOOK: mpv_event_id = 25;

/// Event data for [`MPV_EVENT_GET_PROPERTY_REPLY`] and
/// [`MPV_EVENT_PROPERTY_CHANGE`].
#[repr(C)]
#[derive(Clone, Copy)]
pub struct mpv_event_property {
    /// Name of the property.
    pub name: *const c_char,
    /// Format of `data`. [`MPV_FORMAT_NONE`] if retrieving the property
    /// failed or it is unavailable (in which case `data` is invalid).
    pub format: mpv_format,
    /// The property value, laid out exactly like the out-pointer argument of
    /// [`mpv_get_property`] for the given format (e.g. for
    /// [`MPV_FORMAT_STRING`] this points to a `*mut c_char`). Null when
    /// `format` is [`MPV_FORMAT_NONE`].
    pub data: *mut c_void,
}

/// Numeric log levels. Lower values are more important. The string variants
/// used by [`mpv_request_log_messages`] are noted for each constant.
pub type mpv_log_level = c_int;

/// `"no"` — disable all messages (never seen on received messages).
pub const MPV_LOG_LEVEL_NONE: mpv_log_level = 0;
/// `"fatal"` — critical/aborting errors.
pub const MPV_LOG_LEVEL_FATAL: mpv_log_level = 10;
/// `"error"` — simple errors.
pub const MPV_LOG_LEVEL_ERROR: mpv_log_level = 20;
/// `"warn"` — possible problems.
pub const MPV_LOG_LEVEL_WARN: mpv_log_level = 30;
/// `"info"` — informational messages.
pub const MPV_LOG_LEVEL_INFO: mpv_log_level = 40;
/// `"v"` — noisy informational messages.
pub const MPV_LOG_LEVEL_V: mpv_log_level = 50;
/// `"debug"` — very noisy technical information.
pub const MPV_LOG_LEVEL_DEBUG: mpv_log_level = 60;
/// `"trace"` — extremely noisy.
pub const MPV_LOG_LEVEL_TRACE: mpv_log_level = 70;

/// Event data for [`MPV_EVENT_LOG_MESSAGE`].
#[repr(C)]
#[derive(Clone, Copy)]
pub struct mpv_event_log_message {
    /// Module prefix identifying the sender. The special value `"overflow"`
    /// indicates the message buffer overflowed.
    pub prefix: *const c_char,
    /// The log level as a string (never `"no"`).
    pub level: *const c_char,
    /// One line of text, terminated with a newline character.
    pub text: *const c_char,
    /// Same as `level`, as a numeric [`mpv_log_level`].
    pub log_level: mpv_log_level,
}

/// Reason codes for [`mpv_event_end_file`].
pub type mpv_end_file_reason = c_int;

/// The end of the file was reached (also possible for broken files or
/// interrupted network streams, or a restricted playback range).
pub const MPV_END_FILE_REASON_EOF: mpv_end_file_reason = 0;
/// Playback was stopped by an external action (e.g. playlist controls).
pub const MPV_END_FILE_REASON_STOP: mpv_end_file_reason = 2;
/// Playback was stopped by the `quit` command or player shutdown.
pub const MPV_END_FILE_REASON_QUIT: mpv_end_file_reason = 3;
/// An error caused playback to abort; `mpv_event_end_file.error` is set.
pub const MPV_END_FILE_REASON_ERROR: mpv_end_file_reason = 4;
/// The file was a playlist (or similar) whose entries replaced it in the
/// playlist.
pub const MPV_END_FILE_REASON_REDIRECT: mpv_end_file_reason = 5;

/// Event data for [`MPV_EVENT_START_FILE`].
#[repr(C)]
#[derive(Clone, Copy)]
pub struct mpv_event_start_file {
    /// Playlist entry ID of the file being loaded.
    pub playlist_entry_id: i64,
}

/// Event data for [`MPV_EVENT_END_FILE`].
#[repr(C)]
#[derive(Clone, Copy)]
pub struct mpv_event_end_file {
    /// One of the [`mpv_end_file_reason`] values; treat unknown values as
    /// unknown.
    pub reason: mpv_end_file_reason,
    /// An `MPV_ERROR_*` code if `reason == MPV_END_FILE_REASON_ERROR`,
    /// otherwise `0`.
    pub error: c_int,
    /// Playlist entry ID of the file that was played (matches the ID from the
    /// corresponding [`mpv_event_start_file`]).
    pub playlist_entry_id: i64,
    /// If the entry was replaced by other entries (see
    /// [`MPV_END_FILE_REASON_REDIRECT`]), the ID of the first inserted entry,
    /// otherwise `0`.
    pub playlist_insert_id: i64,
    /// Number of inserted playlist entries; only non-zero when
    /// `playlist_insert_id` is valid. Never negative.
    pub playlist_insert_num_entries: c_int,
}

/// Event data for [`MPV_EVENT_CLIENT_MESSAGE`].
#[repr(C)]
#[derive(Clone, Copy)]
pub struct mpv_event_client_message {
    /// Number of entries in `args`.
    pub num_args: c_int,
    /// Arbitrary sender-chosen arguments; `args[0..num_args]` are valid and
    /// never null.
    pub args: *mut *const c_char,
}

/// Event data for [`MPV_EVENT_HOOK`].
#[repr(C)]
#[derive(Clone, Copy)]
pub struct mpv_event_hook {
    /// The hook name as passed to [`mpv_hook_add`].
    pub name: *const c_char,
    /// Internal ID that must be passed to [`mpv_hook_continue`].
    pub id: u64,
}

/// Event data for [`MPV_EVENT_COMMAND_REPLY`].
#[repr(C)]
#[derive(Clone, Copy)]
pub struct mpv_event_command {
    /// Result data of the command on success ([`MPV_FORMAT_NONE`] for most
    /// commands, and always on failure). Success/failure itself is signaled
    /// via `mpv_event.error`.
    pub result: mpv_node,
}

/// An event returned by [`mpv_wait_event`].
#[repr(C)]
#[derive(Clone, Copy)]
pub struct mpv_event {
    /// The event type. Unknown IDs from newer mpv versions should be ignored.
    pub event_id: mpv_event_id,
    /// For reply events (`MPV_EVENT_GET_PROPERTY_REPLY`,
    /// `MPV_EVENT_SET_PROPERTY_REPLY`, `MPV_EVENT_COMMAND_REPLY`): a status
    /// code that is `>= 0` on success and an [`mpv_error`] on failure.
    pub error: c_int,
    /// The `reply_userdata` value of the request this event replies to
    /// (also set for `MPV_EVENT_PROPERTY_CHANGE` and `MPV_EVENT_HOOK`),
    /// otherwise `0`.
    pub reply_userdata: u64,
    /// Event-specific data; the pointee type depends on `event_id` (see the
    /// documentation of each `MPV_EVENT_*` constant). Null for events without
    /// data.
    pub data: *mut c_void,
}

extern "C" {
    /// Return the `MPV_CLIENT_API_VERSION` the linked mpv library was
    /// compiled with.
    pub fn mpv_client_api_version() -> c_ulong;

    /// Return a static, never-freed string describing the given error code.
    pub fn mpv_error_string(error: c_int) -> *const c_char;

    /// Free memory returned by API functions that document it. `data` may be
    /// null. Calling this on memory not owned by the caller is undefined
    /// behavior.
    pub fn mpv_free(data: *mut c_void);

    /// Return the unique name of this client handle. The string is read-only
    /// and valid until the handle is destroyed.
    pub fn mpv_client_name(ctx: *mut mpv_handle) -> *const c_char;

    /// Return the unique ID of this client handle. IDs are never `0` or
    /// negative and are never reused by the core.
    pub fn mpv_client_id(ctx: *mut mpv_handle) -> i64;

    /// Create a new mpv instance and client handle in an uninitialized state.
    /// Set initial options, then call [`mpv_initialize`]. Returns null on
    /// error (out of memory, or `LC_NUMERIC` is not `"C"`).
    pub fn mpv_create() -> *mut mpv_handle;

    /// Initialize an uninitialized mpv instance. Returns an error if the
    /// instance is already running.
    pub fn mpv_initialize(ctx: *mut mpv_handle) -> c_int;

    /// Disconnect and destroy the handle; `ctx` is deallocated. Destroying
    /// the last (strong) handle brings down the core.
    pub fn mpv_destroy(ctx: *mut mpv_handle);

    /// Like [`mpv_destroy`], but also quits the player and blocks until the
    /// core and all clients are destroyed.
    pub fn mpv_terminate_destroy(ctx: *mut mpv_handle);

    /// Create a new client handle connected to the same core as `ctx` (which
    /// must be initialized). The new handle has its own event queue and
    /// per-handle state. `name` may be null for an arbitrary name. Returns
    /// null on error.
    pub fn mpv_create_client(ctx: *mut mpv_handle, name: *const c_char) -> *mut mpv_handle;

    /// Like [`mpv_create_client`], but the new handle is a weak reference: if
    /// only weak handles remain, the core is destroyed and they receive
    /// [`MPV_EVENT_SHUTDOWN`].
    pub fn mpv_create_weak_client(ctx: *mut mpv_handle, name: *const c_char) -> *mut mpv_handle;

    /// Load and parse a config file (absolute path recommended), applying its
    /// default section as option assignments.
    pub fn mpv_load_config_file(ctx: *mut mpv_handle, filename: *const c_char) -> c_int;

    /// Return the internal real time in nanoseconds, with an arbitrary start
    /// offset; never wraps or goes backwards. Callable at any time while the
    /// context is valid, including from wakeup callbacks and render threads.
    pub fn mpv_get_time_ns(ctx: *mut mpv_handle) -> i64;

    /// Same as [`mpv_get_time_ns`], but in microseconds.
    pub fn mpv_get_time_us(ctx: *mut mpv_handle) -> i64;

    /// Free any data referenced by the node (not the node itself). Call only
    /// on nodes that were written by the client API.
    pub fn mpv_free_node_contents(node: *mut mpv_node);

    /// Set an option. Semi-deprecated: since mpv 0.21.0 most options can be
    /// set with [`mpv_set_property`], even before [`mpv_initialize`].
    /// `name` is the option name without the leading `--`; `data` points to a
    /// value of the type implied by `format`.
    pub fn mpv_set_option(
        ctx: *mut mpv_handle,
        name: *const c_char,
        format: mpv_format,
        data: *mut c_void,
    ) -> c_int;

    /// Convenience wrapper: set an option from a string value
    /// ([`MPV_FORMAT_STRING`]).
    pub fn mpv_set_option_string(
        ctx: *mut mpv_handle,
        name: *const c_char,
        data: *const c_char,
    ) -> c_int;

    /// Run a command with pre-split arguments. `args` is a null-terminated
    /// array of strings; the first item is usually the command name. No OSD
    /// or string expansion by default.
    pub fn mpv_command(ctx: *mut mpv_handle, args: *mut *const c_char) -> c_int;

    /// Like [`mpv_command`], but takes structured arguments
    /// ([`MPV_FORMAT_NODE_ARRAY`] for positional or [`MPV_FORMAT_NODE_MAP`]
    /// for named arguments). On success, `result` (if non-null) receives
    /// command-specific return data that must be released with
    /// [`mpv_free_node_contents`].
    pub fn mpv_command_node(
        ctx: *mut mpv_handle,
        args: *mut mpv_node,
        result: *mut mpv_node,
    ) -> c_int;

    /// Like [`mpv_command`], but on success `result` (if non-null) receives
    /// command-specific return data that must be released with
    /// [`mpv_free_node_contents`].
    pub fn mpv_command_ret(
        ctx: *mut mpv_handle,
        args: *mut *const c_char,
        result: *mut mpv_node,
    ) -> c_int;

    /// Run a command using input.conf parsing for splitting arguments. OSD
    /// and string expansion are enabled by default.
    pub fn mpv_command_string(ctx: *mut mpv_handle, args: *const c_char) -> c_int;

    /// Run a command asynchronously; the result arrives as an
    /// [`MPV_EVENT_COMMAND_REPLY`] event carrying `reply_userdata`. Safe to
    /// call from render API threads.
    pub fn mpv_command_async(
        ctx: *mut mpv_handle,
        reply_userdata: u64,
        args: *mut *const c_char,
    ) -> c_int;

    /// Like [`mpv_command_node`], but asynchronous (see
    /// [`mpv_command_async`]). Safe to call from render API threads.
    pub fn mpv_command_node_async(
        ctx: *mut mpv_handle,
        reply_userdata: u64,
        args: *mut mpv_node,
    ) -> c_int;

    /// Ask all pending async commands with the given `reply_userdata` to
    /// abort. Best-effort and itself asynchronous; the commands still
    /// complete with an [`MPV_EVENT_COMMAND_REPLY`].
    pub fn mpv_abort_async_command(ctx: *mut mpv_handle, reply_userdata: u64);

    /// Set a property (or, since mpv 0.21.0, an option — including before
    /// [`mpv_initialize`]). `data` points to a value of the type implied by
    /// `format`.
    pub fn mpv_set_property(
        ctx: *mut mpv_handle,
        name: *const c_char,
        format: mpv_format,
        data: *mut c_void,
    ) -> c_int;

    /// Convenience wrapper: set a property from a string value
    /// ([`MPV_FORMAT_STRING`]).
    pub fn mpv_set_property_string(
        ctx: *mut mpv_handle,
        name: *const c_char,
        data: *const c_char,
    ) -> c_int;

    /// Delete a property (equivalent to the `del` command).
    pub fn mpv_del_property(ctx: *mut mpv_handle, name: *const c_char) -> c_int;

    /// Set a property asynchronously; the result status arrives as an
    /// [`MPV_EVENT_SET_PROPERTY_REPLY`] event. The value is copied. Safe to
    /// call from render API threads.
    pub fn mpv_set_property_async(
        ctx: *mut mpv_handle,
        reply_userdata: u64,
        name: *const c_char,
        format: mpv_format,
        data: *mut c_void,
    ) -> c_int;

    /// Read a property value into `*data`, which must point to a variable of
    /// the type implied by `format`. Dynamically allocated results are freed
    /// with [`mpv_free`] (strings) or [`mpv_free_node_contents`] (nodes).
    pub fn mpv_get_property(
        ctx: *mut mpv_handle,
        name: *const c_char,
        format: mpv_format,
        data: *mut c_void,
    ) -> c_int;

    /// Return a property as a raw string ([`MPV_FORMAT_STRING`]), or null on
    /// error. Free the result with [`mpv_free`].
    pub fn mpv_get_property_string(ctx: *mut mpv_handle, name: *const c_char) -> *mut c_char;

    /// Return a property as an OSD-formatted string
    /// ([`MPV_FORMAT_OSD_STRING`]), or null on error. Free the result with
    /// [`mpv_free`].
    pub fn mpv_get_property_osd_string(ctx: *mut mpv_handle, name: *const c_char) -> *mut c_char;

    /// Read a property asynchronously; the result arrives as an
    /// [`MPV_EVENT_GET_PROPERTY_REPLY`] event. Safe to call from render API
    /// threads.
    pub fn mpv_get_property_async(
        ctx: *mut mpv_handle,
        reply_userdata: u64,
        name: *const c_char,
        format: mpv_format,
    ) -> c_int;

    /// Get [`MPV_EVENT_PROPERTY_CHANGE`] notifications (carrying
    /// `reply_userdata`) whenever the property changes, including one initial
    /// notification. `format` may be [`MPV_FORMAT_NONE`] to omit the value
    /// from the events. Safe to call from render API threads.
    pub fn mpv_observe_property(
        mpv: *mut mpv_handle,
        reply_userdata: u64,
        name: *const c_char,
        format: mpv_format,
    ) -> c_int;

    /// Undo all [`mpv_observe_property`] registrations that used the given
    /// userdata value. Returns the number of removed properties on success
    /// (>= 0), or an error (< 0). Safe to call from render API threads.
    pub fn mpv_unobserve_property(mpv: *mut mpv_handle, registered_reply_userdata: u64) -> c_int;

    /// Return a static, never-freed symbolic name for the event ID (e.g.
    /// suitable for scripting interfaces), or null for unknown events.
    pub fn mpv_event_name(event: mpv_event_id) -> *const c_char;

    /// Convert an event to an [`MPV_FORMAT_NODE_MAP`] written to `*dst`
    /// (fully overwritten, not read). Release `*dst` with
    /// [`mpv_free_node_contents`]; copy it if you need it past the lifetime
    /// of `src`. Safe to call from render API threads.
    pub fn mpv_event_to_node(dst: *mut mpv_node, src: *mut mpv_event) -> c_int;

    /// Enable (`enable = 1`) or disable (`0`) delivery of the given event.
    /// Some events can't be disabled. Safe to call from render API threads.
    pub fn mpv_request_event(ctx: *mut mpv_handle, event: mpv_event_id, enable: c_int) -> c_int;

    /// Set the minimum log level for receiving [`MPV_EVENT_LOG_MESSAGE`]
    /// events. Valid levels: `"no"` (default, disables messages), `"fatal"`,
    /// `"error"`, `"warn"`, `"info"`, `"v"`, `"debug"`, `"trace"`, and
    /// `"terminal-default"`.
    pub fn mpv_request_log_messages(ctx: *mut mpv_handle, min_level: *const c_char) -> c_int;

    /// Wait for the next event, until `timeout` (in seconds) expires, or
    /// until [`mpv_wakeup`] is called. `timeout == 0` polls; negative waits
    /// forever. Returns a never-null pointer to an event that stays valid
    /// until the next `mpv_wait_event` call or handle destruction; do not
    /// write to it. Only one thread may call this per handle at a time.
    pub fn mpv_wait_event(ctx: *mut mpv_handle, timeout: f64) -> *mut mpv_event;

    /// Interrupt the current (or next) [`mpv_wait_event`] call on this
    /// handle. Safe to call from render API threads.
    pub fn mpv_wakeup(ctx: *mut mpv_handle);

    /// Set a callback invoked from arbitrary internal threads whenever new
    /// events are available. The callback must only notify (no client API
    /// calls, no blocking, no unwinding); consume events elsewhere with
    /// [`mpv_wait_event`]. Only one wakeup callback can be set.
    pub fn mpv_set_wakeup_callback(
        ctx: *mut mpv_handle,
        cb: Option<unsafe extern "C" fn(d: *mut c_void)>,
        d: *mut c_void,
    );

    /// Block until all pending asynchronous requests of this handle have
    /// posted their reply event (the event queue is not emptied).
    pub fn mpv_wait_async_requests(ctx: *mut mpv_handle);

    /// Register a hook handler; hook invocations arrive as
    /// [`MPV_EVENT_HOOK`] events (carrying `reply_userdata`) and must be
    /// continued with [`mpv_hook_continue`]. Handlers with lower `priority`
    /// run first; `0` is a neutral default. Hooks can only be removed by
    /// destroying the handle.
    pub fn mpv_hook_add(
        ctx: *mut mpv_handle,
        reply_userdata: u64,
        name: *const c_char,
        priority: c_int,
    ) -> c_int;

    /// Respond to an [`MPV_EVENT_HOOK`] event, unblocking the player. `id`
    /// must be the `mpv_event_hook.id` value; calling this more than once per
    /// event, or from a different handle, is undefined behavior.
    pub fn mpv_hook_continue(ctx: *mut mpv_handle, id: u64) -> c_int;

    /// Return the read end of a non-blocking wakeup pipe (a UNIX file
    /// descriptor) for integrating with `poll()`-based event loops, or `-1`
    /// on error (always `-1` on Windows). Drain the pipe, then call
    /// [`mpv_wait_event`] with timeout `0` until [`MPV_EVENT_NONE`].
    #[deprecated(
        note = "create a pipe manually and write to it from a mpv_set_wakeup_callback callback"
    )]
    pub fn mpv_get_wakeup_pipe(ctx: *mut mpv_handle) -> c_int;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_macro() {
        assert_eq!(MPV_MAKE_VERSION(2, 5), (2 << 16) | 5);
        assert_eq!(MPV_CLIENT_API_VERSION >> 16, 2);
    }
}
