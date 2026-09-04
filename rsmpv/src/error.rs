use std::ffi::CStr;
use std::fmt;
use std::os::raw::c_int;

/// Specialized `Result` used throughout this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// An error from libmpv, or from the safe wrapper itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The event ringbuffer is full; the client can't receive more events.
    EventQueueFull,
    /// Memory allocation failed.
    OutOfMemory,
    /// The mpv core has not been initialized yet.
    Uninitialized,
    /// Invalid or unsupported parameter value.
    InvalidParameter,
    /// The option doesn't exist.
    OptionNotFound,
    /// Unsupported format for the option.
    OptionFormat,
    /// Setting the option failed (e.g. parse error).
    OptionError,
    /// The property doesn't exist.
    PropertyNotFound,
    /// Unsupported format for the property.
    PropertyFormat,
    /// The property exists but is currently unavailable.
    PropertyUnavailable,
    /// Error getting or setting the property.
    PropertyError,
    /// Error running a command.
    Command,
    /// Generic loading error.
    LoadingFailed,
    /// Initializing the audio output failed.
    AoInitFailed,
    /// Initializing the video output failed.
    VoInitFailed,
    /// No audio or video data to play.
    NothingToPlay,
    /// File format not recognized, or file too broken to open.
    UnknownFormat,
    /// System requirements not fulfilled.
    Unsupported,
    /// The called API function is only a stub.
    NotImplemented,
    /// Unspecified error.
    Generic,
    /// A negative error code from libmpv this crate doesn't know about.
    Other(i32),
    /// A string argument contained an interior NUL byte and can't be passed
    /// to libmpv (wrapper-side error; not from libmpv).
    InteriorNul,
    /// libmpv returned a null handle from a create function (wrapper-side
    /// classification; typically means out of memory or `LC_NUMERIC` is not
    /// `"C"`).
    CreateFailed,
}

impl Error {
    /// Convert a raw libmpv error code (must be negative) into an `Error`.
    pub fn from_raw(code: c_int) -> Error {
        use Error::*;
        match code {
            rsmpv_sys::MPV_ERROR_EVENT_QUEUE_FULL => EventQueueFull,
            rsmpv_sys::MPV_ERROR_NOMEM => OutOfMemory,
            rsmpv_sys::MPV_ERROR_UNINITIALIZED => Uninitialized,
            rsmpv_sys::MPV_ERROR_INVALID_PARAMETER => InvalidParameter,
            rsmpv_sys::MPV_ERROR_OPTION_NOT_FOUND => OptionNotFound,
            rsmpv_sys::MPV_ERROR_OPTION_FORMAT => OptionFormat,
            rsmpv_sys::MPV_ERROR_OPTION_ERROR => OptionError,
            rsmpv_sys::MPV_ERROR_PROPERTY_NOT_FOUND => PropertyNotFound,
            rsmpv_sys::MPV_ERROR_PROPERTY_FORMAT => PropertyFormat,
            rsmpv_sys::MPV_ERROR_PROPERTY_UNAVAILABLE => PropertyUnavailable,
            rsmpv_sys::MPV_ERROR_PROPERTY_ERROR => PropertyError,
            rsmpv_sys::MPV_ERROR_COMMAND => Command,
            rsmpv_sys::MPV_ERROR_LOADING_FAILED => LoadingFailed,
            rsmpv_sys::MPV_ERROR_AO_INIT_FAILED => AoInitFailed,
            rsmpv_sys::MPV_ERROR_VO_INIT_FAILED => VoInitFailed,
            rsmpv_sys::MPV_ERROR_NOTHING_TO_PLAY => NothingToPlay,
            rsmpv_sys::MPV_ERROR_UNKNOWN_FORMAT => UnknownFormat,
            rsmpv_sys::MPV_ERROR_UNSUPPORTED => Unsupported,
            rsmpv_sys::MPV_ERROR_NOT_IMPLEMENTED => NotImplemented,
            rsmpv_sys::MPV_ERROR_GENERIC => Generic,
            other => Other(other),
        }
    }

    /// The raw libmpv error code, if this error came from (or maps onto)
    /// libmpv.
    pub fn raw_code(&self) -> Option<i32> {
        use Error::*;
        Some(match self {
            EventQueueFull => rsmpv_sys::MPV_ERROR_EVENT_QUEUE_FULL,
            OutOfMemory => rsmpv_sys::MPV_ERROR_NOMEM,
            Uninitialized => rsmpv_sys::MPV_ERROR_UNINITIALIZED,
            InvalidParameter => rsmpv_sys::MPV_ERROR_INVALID_PARAMETER,
            OptionNotFound => rsmpv_sys::MPV_ERROR_OPTION_NOT_FOUND,
            OptionFormat => rsmpv_sys::MPV_ERROR_OPTION_FORMAT,
            OptionError => rsmpv_sys::MPV_ERROR_OPTION_ERROR,
            PropertyNotFound => rsmpv_sys::MPV_ERROR_PROPERTY_NOT_FOUND,
            PropertyFormat => rsmpv_sys::MPV_ERROR_PROPERTY_FORMAT,
            PropertyUnavailable => rsmpv_sys::MPV_ERROR_PROPERTY_UNAVAILABLE,
            PropertyError => rsmpv_sys::MPV_ERROR_PROPERTY_ERROR,
            Command => rsmpv_sys::MPV_ERROR_COMMAND,
            LoadingFailed => rsmpv_sys::MPV_ERROR_LOADING_FAILED,
            AoInitFailed => rsmpv_sys::MPV_ERROR_AO_INIT_FAILED,
            VoInitFailed => rsmpv_sys::MPV_ERROR_VO_INIT_FAILED,
            NothingToPlay => rsmpv_sys::MPV_ERROR_NOTHING_TO_PLAY,
            UnknownFormat => rsmpv_sys::MPV_ERROR_UNKNOWN_FORMAT,
            Unsupported => rsmpv_sys::MPV_ERROR_UNSUPPORTED,
            NotImplemented => rsmpv_sys::MPV_ERROR_NOT_IMPLEMENTED,
            Generic => rsmpv_sys::MPV_ERROR_GENERIC,
            Other(code) => *code,
            InteriorNul | CreateFailed => return None,
        })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InteriorNul => f.write_str("string contains an interior NUL byte"),
            Error::CreateFailed => f.write_str("mpv handle creation returned null"),
            other => {
                let code = other.raw_code().unwrap_or(rsmpv_sys::MPV_ERROR_GENERIC);
                let msg = unsafe { CStr::from_ptr(rsmpv_sys::mpv_error_string(code)) };
                write!(f, "{} ({})", msg.to_string_lossy(), code)
            }
        }
    }
}

impl std::error::Error for Error {}

/// Map a libmpv status return (`>= 0` success) to a `Result`.
pub(crate) fn check(code: c_int) -> Result<c_int> {
    if code >= 0 {
        Ok(code)
    } else {
        Err(Error::from_raw(code))
    }
}
