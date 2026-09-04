//! Typed property access.

use std::ffi::{c_void, CStr, CString};
use std::os::raw::{c_char, c_int};

use crate::error::{check, Error, Result};
use crate::node::{Node, NodeStorage};

/// The data formats a property can be accessed or observed with
/// (the safe counterpart of `mpv_format`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    /// No value (for observation: notify without carrying data).
    #[default]
    None,
    /// Raw string.
    String,
    /// Human-readable OSD string (read/observe only).
    OsdString,
    /// Boolean flag.
    Flag,
    /// 64-bit integer.
    Int64,
    /// Double.
    Double,
    /// Structured [`Node`].
    Node,
}

impl Format {
    pub(crate) fn to_raw(self) -> rsmpv_sys::mpv_format {
        match self {
            Format::None => rsmpv_sys::MPV_FORMAT_NONE,
            Format::String => rsmpv_sys::MPV_FORMAT_STRING,
            Format::OsdString => rsmpv_sys::MPV_FORMAT_OSD_STRING,
            Format::Flag => rsmpv_sys::MPV_FORMAT_FLAG,
            Format::Int64 => rsmpv_sys::MPV_FORMAT_INT64,
            Format::Double => rsmpv_sys::MPV_FORMAT_DOUBLE,
            Format::Node => rsmpv_sys::MPV_FORMAT_NODE,
        }
    }
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for bool {}
    impl Sealed for i64 {}
    impl Sealed for f64 {}
    impl Sealed for String {}
    impl Sealed for crate::Node {}
    impl Sealed for &str {}
    impl Sealed for &crate::Node {}
    impl Sealed for &String {}
}

/// Types a property can be read as: `bool`, `i64`, `f64`, `String`,
/// [`Node`]. This trait is sealed.
pub trait GetProperty: sealed::Sealed + Sized {
    /// The [`Format`] this type maps to; used by
    /// [`Handle::get_property_async`](crate::Handle::get_property_async) to
    /// pick the reply format.
    const FORMAT: Format;

    #[doc(hidden)]
    unsafe fn get_from(handle: *mut rsmpv_sys::mpv_handle, name: *const c_char) -> Result<Self>;
}

/// Types a property can be written from: `bool`, `i64`, `f64`, `&str`,
/// `String`, [`Node`] (and references). This trait is sealed.
pub trait SetProperty: sealed::Sealed {
    #[doc(hidden)]
    unsafe fn set_on(&self, handle: *mut rsmpv_sys::mpv_handle, name: *const c_char) -> Result<()>;
}

impl GetProperty for bool {
    const FORMAT: Format = Format::Flag;
    unsafe fn get_from(handle: *mut rsmpv_sys::mpv_handle, name: *const c_char) -> Result<bool> {
        let mut out: c_int = 0;
        check(rsmpv_sys::mpv_get_property(
            handle,
            name,
            rsmpv_sys::MPV_FORMAT_FLAG,
            &mut out as *mut c_int as *mut c_void,
        ))?;
        Ok(out != 0)
    }
}

impl GetProperty for i64 {
    const FORMAT: Format = Format::Int64;
    unsafe fn get_from(handle: *mut rsmpv_sys::mpv_handle, name: *const c_char) -> Result<i64> {
        let mut out: i64 = 0;
        check(rsmpv_sys::mpv_get_property(
            handle,
            name,
            rsmpv_sys::MPV_FORMAT_INT64,
            &mut out as *mut i64 as *mut c_void,
        ))?;
        Ok(out)
    }
}

impl GetProperty for f64 {
    const FORMAT: Format = Format::Double;
    unsafe fn get_from(handle: *mut rsmpv_sys::mpv_handle, name: *const c_char) -> Result<f64> {
        let mut out: f64 = 0.0;
        check(rsmpv_sys::mpv_get_property(
            handle,
            name,
            rsmpv_sys::MPV_FORMAT_DOUBLE,
            &mut out as *mut f64 as *mut c_void,
        ))?;
        Ok(out)
    }
}

impl GetProperty for String {
    const FORMAT: Format = Format::String;
    unsafe fn get_from(handle: *mut rsmpv_sys::mpv_handle, name: *const c_char) -> Result<String> {
        let mut out: *mut c_char = std::ptr::null_mut();
        check(rsmpv_sys::mpv_get_property(
            handle,
            name,
            rsmpv_sys::MPV_FORMAT_STRING,
            &mut out as *mut *mut c_char as *mut c_void,
        ))?;
        let value = CStr::from_ptr(out).to_string_lossy().into_owned();
        rsmpv_sys::mpv_free(out as *mut c_void);
        Ok(value)
    }
}

impl GetProperty for Node {
    const FORMAT: Format = Format::Node;
    unsafe fn get_from(handle: *mut rsmpv_sys::mpv_handle, name: *const c_char) -> Result<Node> {
        let mut out = std::mem::zeroed::<rsmpv_sys::mpv_node>();
        check(rsmpv_sys::mpv_get_property(
            handle,
            name,
            rsmpv_sys::MPV_FORMAT_NODE,
            &mut out as *mut rsmpv_sys::mpv_node as *mut c_void,
        ))?;
        let value = Node::from_raw(&out);
        rsmpv_sys::mpv_free_node_contents(&mut out);
        Ok(value)
    }
}

unsafe fn set_raw(
    handle: *mut rsmpv_sys::mpv_handle,
    name: *const c_char,
    format: rsmpv_sys::mpv_format,
    data: *mut c_void,
) -> Result<()> {
    check(rsmpv_sys::mpv_set_property(handle, name, format, data)).map(|_| ())
}

impl SetProperty for bool {
    unsafe fn set_on(&self, handle: *mut rsmpv_sys::mpv_handle, name: *const c_char) -> Result<()> {
        let mut v: c_int = *self as c_int;
        set_raw(
            handle,
            name,
            rsmpv_sys::MPV_FORMAT_FLAG,
            &mut v as *mut c_int as *mut c_void,
        )
    }
}

impl SetProperty for i64 {
    unsafe fn set_on(&self, handle: *mut rsmpv_sys::mpv_handle, name: *const c_char) -> Result<()> {
        let mut v = *self;
        set_raw(
            handle,
            name,
            rsmpv_sys::MPV_FORMAT_INT64,
            &mut v as *mut i64 as *mut c_void,
        )
    }
}

impl SetProperty for f64 {
    unsafe fn set_on(&self, handle: *mut rsmpv_sys::mpv_handle, name: *const c_char) -> Result<()> {
        let mut v = *self;
        set_raw(
            handle,
            name,
            rsmpv_sys::MPV_FORMAT_DOUBLE,
            &mut v as *mut f64 as *mut c_void,
        )
    }
}

impl SetProperty for &str {
    unsafe fn set_on(&self, handle: *mut rsmpv_sys::mpv_handle, name: *const c_char) -> Result<()> {
        let value = CString::new(*self).map_err(|_| Error::InteriorNul)?;
        check(rsmpv_sys::mpv_set_property_string(
            handle,
            name,
            value.as_ptr(),
        ))
        .map(|_| ())
    }
}

impl SetProperty for String {
    unsafe fn set_on(&self, handle: *mut rsmpv_sys::mpv_handle, name: *const c_char) -> Result<()> {
        self.as_str().set_on(handle, name)
    }
}

impl SetProperty for &String {
    unsafe fn set_on(&self, handle: *mut rsmpv_sys::mpv_handle, name: *const c_char) -> Result<()> {
        self.as_str().set_on(handle, name)
    }
}

impl SetProperty for Node {
    unsafe fn set_on(&self, handle: *mut rsmpv_sys::mpv_handle, name: *const c_char) -> Result<()> {
        let mut storage = NodeStorage::default();
        let mut raw = storage.build(self)?;
        set_raw(
            handle,
            name,
            rsmpv_sys::MPV_FORMAT_NODE,
            &mut raw as *mut rsmpv_sys::mpv_node as *mut c_void,
        )
    }
}

impl SetProperty for &Node {
    unsafe fn set_on(&self, handle: *mut rsmpv_sys::mpv_handle, name: *const c_char) -> Result<()> {
        (*self).set_on(handle, name)
    }
}
