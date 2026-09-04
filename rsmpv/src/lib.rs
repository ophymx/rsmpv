//! Safe, idiomatic Rust bindings for [libmpv], the embeddable mpv media
//! player, built on the ISC-licensed mpv client headers (via the
//! [`rsmpv-sys`](rsmpv_sys) crate).
//!
//! Essentially everything in mpv is done through *commands* (load a file,
//! seek), *properties* (pause state, playback position, volume), and
//! *events*; see the [mpv reference][manual] for what's available.
//!
//! ```no_run
//! use rsmpv::{Event, Format, Mpv};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut mpv = Mpv::builder()?
//!     .set_property("vo", "null")?
//!     .set_property("ao", "null")?
//!     .build()?;
//!
//! mpv.command(&["loadfile", "test.mkv"])?;
//! mpv.observe_property(1, "playback-time", Format::Double)?;
//!
//! loop {
//!     match mpv.wait_event(-1.0) {
//!         Some(Event::PropertyChange { name, data, .. }) => {
//!             println!("{name} changed: {data:?}");
//!         }
//!         Some(Event::EndFile { .. }) | Some(Event::Shutdown) => break,
//!         _ => {}
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Threading
//!
//! The libmpv client API is thread-safe, and this wrapper reflects that:
//! [`Mpv`] (and [`Client`]) are [`Send`] and [`Sync`], and everything except
//! [`Handle::wait_event`] takes `&self`. `wait_event` takes `&mut self`
//! because libmpv allows only one thread at a time to wait for events on the
//! same handle; to run the event loop on its own thread while controlling
//! playback from others, create a [`Client`] handle per thread with
//! [`Mpv::create_client`].
//!
//! # Licensing
//!
//! This crate is licensed under MIT OR Apache-2.0; the underlying
//! [`rsmpv-sys`](rsmpv_sys) crate is ISC like the mpv client headers it was
//! written from. The mpv library you link against is GPLv2+ by default
//! (LGPLv2.1+ when mpv is built with `-Dgpl=false`).
//!
//! [libmpv]: https://mpv.io/
//! [manual]: https://mpv.io/manual/stable/

#![warn(missing_docs)]

use std::ffi::{c_void, CString};
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::os::raw::{c_char, c_int};
use std::path::Path;
use std::ptr::NonNull;
use std::sync::Mutex;

mod error;
mod event;
mod node;
mod property;

#[cfg(feature = "render")]
pub mod render;
#[cfg(feature = "stream-cb")]
pub mod stream_cb;

pub use error::{Error, Result};
pub use event::{EndFileReason, Event, LogMessage, PropertyData};
pub use node::Node;
pub use property::{Format, GetProperty, SetProperty};

/// Re-export of the raw FFI bindings for escape-hatch use.
pub use rsmpv_sys as sys;

/// The client API version of the linked libmpv, as `(major, minor)`.
pub fn client_api_version() -> (u16, u16) {
    let v = unsafe { rsmpv_sys::mpv_client_api_version() };
    ((v >> 16) as u16, (v & 0xffff) as u16)
}

fn cstr(s: &str) -> Result<CString> {
    CString::new(s).map_err(|_| Error::InteriorNul)
}

/// Build a null-terminated argv of C strings from `args`.
fn cstr_args<S: AsRef<str>>(args: &[S]) -> Result<(Vec<CString>, Vec<*const c_char>)> {
    let owned: Vec<CString> = args
        .iter()
        .map(|a| cstr(a.as_ref()))
        .collect::<Result<_>>()?;
    let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
    ptrs.push(std::ptr::null());
    Ok((owned, ptrs))
}

type WakeupCallback = Box<dyn Fn() + Send + 'static>;

unsafe extern "C" fn wakeup_trampoline(d: *mut c_void) {
    // The callback contract forbids unwinding out of the callback.
    let cb = &*(d as *const WakeupCallback);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(cb));
}

/// A libmpv client handle: the API surface shared by [`Mpv`], [`Client`],
/// and [`Builder`].
///
/// You never own a bare `Handle`; you reach it through `Deref` from those
/// types.
pub struct Handle {
    raw: NonNull<rsmpv_sys::mpv_handle>,
    /// Wakeup callbacks ever registered on this handle. libmpv gives no way
    /// to synchronize with a callback that may be running concurrently when
    /// it gets replaced, so old callbacks are kept alive until the handle is
    /// dropped.
    // The outer Box keeps each callback at a stable address while the Vec
    // reallocates; libmpv holds a raw pointer to the boxed value.
    #[allow(clippy::vec_box)]
    wakeup_callbacks: Mutex<Vec<Box<WakeupCallback>>>,
}

// SAFETY: the libmpv client API is documented as thread-safe. The only
// restriction — a single concurrent mpv_wait_event caller per handle — is
// enforced by wait_event taking &mut self.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Handle {
    fn from_raw(raw: NonNull<rsmpv_sys::mpv_handle>) -> Handle {
        Handle {
            raw,
            wakeup_callbacks: Mutex::new(Vec::new()),
        }
    }

    /// The raw `mpv_handle` pointer, for use with [`sys`] as an escape
    /// hatch. The pointer is valid as long as `self` is.
    pub fn as_raw(&self) -> *mut rsmpv_sys::mpv_handle {
        self.raw.as_ptr()
    }

    /// The unique name of this client handle.
    pub fn client_name(&self) -> String {
        unsafe {
            std::ffi::CStr::from_ptr(rsmpv_sys::mpv_client_name(self.as_raw()))
                .to_string_lossy()
                .into_owned()
        }
    }

    /// The unique, never-reused ID of this client handle (never zero or
    /// negative).
    pub fn client_id(&self) -> i64 {
        unsafe { rsmpv_sys::mpv_client_id(self.as_raw()) }
    }

    /// Internal real time in nanoseconds (arbitrary start offset, never goes
    /// backwards).
    pub fn get_time_ns(&self) -> i64 {
        unsafe { rsmpv_sys::mpv_get_time_ns(self.as_raw()) }
    }

    /// Internal real time in microseconds.
    pub fn get_time_us(&self) -> i64 {
        unsafe { rsmpv_sys::mpv_get_time_us(self.as_raw()) }
    }

    /// Load a config file (absolute path recommended by mpv).
    pub fn load_config_file(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = cstr(path.as_ref().to_str().ok_or(Error::InvalidParameter)?)?;
        error::check(unsafe { rsmpv_sys::mpv_load_config_file(self.as_raw(), path.as_ptr()) })
            .map(|_| ())
    }

    /// Read a property. `T` selects the format: `bool`, `i64`, `f64`,
    /// `String`, or [`Node`].
    ///
    /// ```no_run
    /// # let mpv = rsmpv::Mpv::new().unwrap();
    /// let paused: bool = mpv.get_property("pause")?;
    /// let pos: f64 = mpv.get_property("playback-time")?;
    /// # Ok::<(), rsmpv::Error>(())
    /// ```
    pub fn get_property<T: GetProperty>(&self, name: &str) -> Result<T> {
        let name = cstr(name)?;
        unsafe { T::get_from(self.as_raw(), name.as_ptr()) }
    }

    /// Read a property as a human-readable OSD string.
    pub fn get_property_osd_string(&self, name: &str) -> Result<String> {
        let name = cstr(name)?;
        let ptr = unsafe { rsmpv_sys::mpv_get_property_osd_string(self.as_raw(), name.as_ptr()) };
        if ptr.is_null() {
            return Err(Error::PropertyError);
        }
        let value = unsafe { std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned() };
        unsafe { rsmpv_sys::mpv_free(ptr as *mut c_void) };
        Ok(value)
    }

    /// Set a property (since mpv 0.21.0 this also sets options, including on
    /// an uninitialized [`Builder`] handle). Accepts `bool`, `i64`, `f64`,
    /// `&str`, `String`, and [`Node`] values.
    pub fn set_property(&self, name: &str, value: impl SetProperty) -> Result<()> {
        let name = cstr(name)?;
        unsafe { value.set_on(self.as_raw(), name.as_ptr()) }
    }

    /// Delete a property (equivalent to the `del` command).
    pub fn del_property(&self, name: &str) -> Result<()> {
        let name = cstr(name)?;
        error::check(unsafe { rsmpv_sys::mpv_del_property(self.as_raw(), name.as_ptr()) })
            .map(|_| ())
    }

    /// Read a property asynchronously; the result arrives as
    /// [`Event::GetPropertyReply`] carrying `userdata`.
    /// `T` selects the format the reply is decoded with, exactly as in
    /// [`get_property`](Handle::get_property):
    ///
    /// ```no_run
    /// # let mpv = rsmpv::Mpv::new().unwrap();
    /// mpv.get_property_async::<f64>(1, "playback-time")?;
    /// # Ok::<(), rsmpv::Error>(())
    /// ```
    pub fn get_property_async<T: GetProperty>(&self, userdata: u64, name: &str) -> Result<()> {
        let name = cstr(name)?;
        error::check(unsafe {
            rsmpv_sys::mpv_get_property_async(
                self.as_raw(),
                userdata,
                name.as_ptr(),
                T::FORMAT.to_raw(),
            )
        })
        .map(|_| ())
    }

    /// Set a property asynchronously; the result arrives as
    /// [`Event::SetPropertyReply`] carrying `userdata`.
    pub fn set_property_async(
        &self,
        userdata: u64,
        name: &str,
        value: impl Into<Node>,
    ) -> Result<()> {
        let name = cstr(name)?;
        let mut storage = node::NodeStorage::default();
        let mut raw = storage.build(&value.into())?;
        error::check(unsafe {
            rsmpv_sys::mpv_set_property_async(
                self.as_raw(),
                userdata,
                name.as_ptr(),
                rsmpv_sys::MPV_FORMAT_NODE,
                &mut raw as *mut rsmpv_sys::mpv_node as *mut c_void,
            )
        })
        .map(|_| ())
    }

    /// Get [`Event::PropertyChange`] notifications (carrying `userdata`)
    /// whenever the property changes, plus one initial notification.
    /// [`Format::None`] omits the value from the events.
    pub fn observe_property(&self, userdata: u64, name: &str, format: Format) -> Result<()> {
        let name = cstr(name)?;
        error::check(unsafe {
            rsmpv_sys::mpv_observe_property(self.as_raw(), userdata, name.as_ptr(), format.to_raw())
        })
        .map(|_| ())
    }

    /// Undo all [`observe_property`](Handle::observe_property) registrations
    /// that used `userdata`. Returns how many were removed.
    pub fn unobserve_property(&self, userdata: u64) -> Result<u32> {
        error::check(unsafe { rsmpv_sys::mpv_unobserve_property(self.as_raw(), userdata) })
            .map(|n| n as u32)
    }

    /// Run a command with pre-split arguments (no OSD or string expansion).
    ///
    /// ```no_run
    /// # let mpv = rsmpv::Mpv::new().unwrap();
    /// mpv.command(&["loadfile", "video.mkv", "append-play"])?;
    /// # Ok::<(), rsmpv::Error>(())
    /// ```
    pub fn command<S: AsRef<str>>(&self, args: &[S]) -> Result<()> {
        let (_owned, mut ptrs) = cstr_args(args)?;
        error::check(unsafe { rsmpv_sys::mpv_command(self.as_raw(), ptrs.as_mut_ptr()) })
            .map(|_| ())
    }

    /// Like [`command`](Handle::command), but returns the command's result
    /// data ([`Node::None`] for most commands).
    pub fn command_ret<S: AsRef<str>>(&self, args: &[S]) -> Result<Node> {
        let (_owned, mut ptrs) = cstr_args(args)?;
        let mut result = unsafe { std::mem::zeroed::<rsmpv_sys::mpv_node>() };
        error::check(unsafe {
            rsmpv_sys::mpv_command_ret(self.as_raw(), ptrs.as_mut_ptr(), &mut result)
        })?;
        let node = unsafe { Node::from_raw(&result) };
        unsafe { rsmpv_sys::mpv_free_node_contents(&mut result) };
        Ok(node)
    }

    /// Run a command given as one string, split with input.conf parsing (OSD
    /// and string expansion enabled).
    pub fn command_string(&self, command: &str) -> Result<()> {
        let command = cstr(command)?;
        error::check(unsafe { rsmpv_sys::mpv_command_string(self.as_raw(), command.as_ptr()) })
            .map(|_| ())
    }

    /// Run a command asynchronously; the result arrives as
    /// [`Event::CommandReply`] carrying `userdata`.
    pub fn command_async<S: AsRef<str>>(&self, userdata: u64, args: &[S]) -> Result<()> {
        let (_owned, mut ptrs) = cstr_args(args)?;
        error::check(unsafe {
            rsmpv_sys::mpv_command_async(self.as_raw(), userdata, ptrs.as_mut_ptr())
        })
        .map(|_| ())
    }

    /// Run a command with structured arguments: a [`Node::Array`] of
    /// positional arguments (first element is the command name), or a
    /// [`Node::Map`] of named arguments (requires a `"name"` entry). Returns
    /// the command's result data.
    pub fn command_node(&self, args: &Node) -> Result<Node> {
        let mut storage = node::NodeStorage::default();
        let mut raw = storage.build(args)?;
        let mut result = unsafe { std::mem::zeroed::<rsmpv_sys::mpv_node>() };
        error::check(unsafe { rsmpv_sys::mpv_command_node(self.as_raw(), &mut raw, &mut result) })?;
        let node = unsafe { Node::from_raw(&result) };
        unsafe { rsmpv_sys::mpv_free_node_contents(&mut result) };
        Ok(node)
    }

    /// Like [`command_node`](Handle::command_node), but asynchronous; the
    /// result arrives as [`Event::CommandReply`] carrying `userdata`.
    pub fn command_node_async(&self, userdata: u64, args: &Node) -> Result<()> {
        let mut storage = node::NodeStorage::default();
        let mut raw = storage.build(args)?;
        error::check(unsafe {
            rsmpv_sys::mpv_command_node_async(self.as_raw(), userdata, &mut raw)
        })
        .map(|_| ())
    }

    /// Ask all pending async commands started with `userdata` to abort
    /// (best-effort; they still deliver an [`Event::CommandReply`]).
    pub fn abort_async_command(&self, userdata: u64) {
        unsafe { rsmpv_sys::mpv_abort_async_command(self.as_raw(), userdata) }
    }

    /// Enable or disable delivery of an event by its raw
    /// `sys::MPV_EVENT_*` ID. Some events can't be disabled.
    pub fn request_event(&self, event_id: i32, enable: bool) -> Result<()> {
        error::check(unsafe {
            rsmpv_sys::mpv_request_event(self.as_raw(), event_id, enable as c_int)
        })
        .map(|_| ())
    }

    /// Set the minimum log level for receiving [`Event::LogMessage`]s.
    /// Valid levels: `"no"` (default), `"fatal"`, `"error"`, `"warn"`,
    /// `"info"`, `"v"`, `"debug"`, `"trace"`, `"terminal-default"`.
    pub fn request_log_messages(&self, min_level: &str) -> Result<()> {
        let min_level = cstr(min_level)?;
        error::check(unsafe {
            rsmpv_sys::mpv_request_log_messages(self.as_raw(), min_level.as_ptr())
        })
        .map(|_| ())
    }

    /// Wait up to `timeout` seconds for the next event. `0.0` polls,
    /// negative waits indefinitely. Returns `None` on timeout or spurious
    /// wakeup. The event is deep-copied and owned.
    pub fn wait_event(&mut self, timeout: f64) -> Option<Event> {
        unsafe { Event::from_raw(rsmpv_sys::mpv_wait_event(self.as_raw(), timeout)) }
    }

    /// Interrupt the current (or next) [`wait_event`](Handle::wait_event)
    /// call on this handle.
    pub fn wakeup(&self) {
        unsafe { rsmpv_sys::mpv_wakeup(self.as_raw()) }
    }

    /// Set a callback invoked (from arbitrary mpv-internal threads) whenever
    /// new events are available. The callback must only notify — typically
    /// waking your event loop, which then drains events with
    /// [`wait_event`](Handle::wait_event) — and must not call back into
    /// libmpv. Panics in the callback are caught and ignored.
    ///
    /// Only one callback is active; setting a new one replaces it (the old
    /// closure is kept alive until the handle is dropped, because libmpv
    /// offers no way to synchronize with a concurrently running callback).
    pub fn set_wakeup_callback(&self, callback: impl Fn() + Send + 'static) {
        let boxed: Box<WakeupCallback> = Box::new(Box::new(callback));
        let ptr = &*boxed as *const WakeupCallback as *mut c_void;
        self.wakeup_callbacks.lock().unwrap().push(boxed);
        unsafe {
            rsmpv_sys::mpv_set_wakeup_callback(self.as_raw(), Some(wakeup_trampoline), ptr);
        }
    }

    /// Block until all pending asynchronous requests of this handle have
    /// posted their reply event.
    pub fn wait_async_requests(&self) {
        unsafe { rsmpv_sys::mpv_wait_async_requests(self.as_raw()) }
    }

    /// Register a hook handler (see the "Hooks" section of the mpv manual).
    /// Invocations arrive as [`Event::Hook`] carrying `userdata` and must be
    /// answered with [`hook_continue`](Handle::hook_continue). Lower
    /// `priority` runs first; `0` is a neutral default.
    pub fn hook_add(&self, userdata: u64, name: &str, priority: i32) -> Result<()> {
        let name = cstr(name)?;
        error::check(unsafe {
            rsmpv_sys::mpv_hook_add(self.as_raw(), userdata, name.as_ptr(), priority)
        })
        .map(|_| ())
    }

    /// Continue a hook, passing the `id` from [`Event::Hook`]. Must be
    /// called exactly once per hook event, from the handle that received it.
    pub fn hook_continue(&self, id: u64) -> Result<()> {
        error::check(unsafe { rsmpv_sys::mpv_hook_continue(self.as_raw(), id) }).map(|_| ())
    }
}

/// An owned mpv player instance (the main client handle).
///
/// Create one with [`Mpv::new`] for defaults, or [`Mpv::builder`] to set
/// options first. Dropping the `Mpv` quits the player and blocks until the
/// core and all clients are destroyed (`mpv_terminate_destroy`).
///
/// All player functionality is on the derefed [`Handle`].
pub struct Mpv {
    inner: Handle,
    /// Open functions of registered stream protocols. Registrations last
    /// until the core dies, so these must outlive the terminate_destroy in
    /// Drop — fields drop after the Drop impl body runs, which is exactly
    /// the required order.
    #[cfg(feature = "stream-cb")]
    #[allow(clippy::vec_box)] // stable addresses; libmpv holds raw pointers
    protocols: Mutex<Vec<Box<stream_cb::OpenFn>>>,
}

impl std::ops::Deref for Mpv {
    type Target = Handle;
    fn deref(&self) -> &Handle {
        &self.inner
    }
}

impl std::ops::DerefMut for Mpv {
    fn deref_mut(&mut self) -> &mut Handle {
        &mut self.inner
    }
}

impl Mpv {
    /// Create and initialize an mpv instance with default (embedding-
    /// friendly) settings: no config files, no terminal access, idle mode
    /// enabled.
    ///
    /// mpv requires the `LC_NUMERIC` locale category to be `"C"` (the
    /// default for Rust programs that don't call `setlocale`).
    pub fn new() -> Result<Mpv> {
        Mpv::builder()?.build()
    }

    /// Create an uninitialized instance whose options can be set before the
    /// player starts.
    pub fn builder() -> Result<Builder> {
        let raw = unsafe { rsmpv_sys::mpv_create() };
        let raw = NonNull::new(raw).ok_or(Error::CreateFailed)?;
        Ok(Builder {
            inner: Handle::from_raw(raw),
        })
    }

    /// Create an additional client handle connected to the same player core,
    /// with its own event queue, observed properties, and async state.
    ///
    /// The client borrows the `Mpv`, so it can't outlive the core (dropping
    /// the `Mpv` last is what terminates the player).
    pub fn create_client(&self, name: Option<&str>) -> Result<Client<'_>> {
        self.new_client(name, false)
    }

    /// Like [`create_client`](Mpv::create_client), but the handle is a weak
    /// reference: it doesn't keep the core alive on its own and receives
    /// [`Event::Shutdown`] when the last strong handle goes away.
    pub fn create_weak_client(&self, name: Option<&str>) -> Result<Client<'_>> {
        self.new_client(name, true)
    }

    fn new_client(&self, name: Option<&str>, weak: bool) -> Result<Client<'_>> {
        let name = name.map(cstr).transpose()?;
        let name_ptr = name.as_ref().map_or(std::ptr::null(), |n| n.as_ptr());
        let raw = unsafe {
            if weak {
                rsmpv_sys::mpv_create_weak_client(self.as_raw(), name_ptr)
            } else {
                rsmpv_sys::mpv_create_client(self.as_raw(), name_ptr)
            }
        };
        let raw = NonNull::new(raw).ok_or(Error::CreateFailed)?;
        Ok(Client {
            inner: Handle::from_raw(raw),
            _core: PhantomData,
        })
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        unsafe { rsmpv_sys::mpv_terminate_destroy(self.as_raw()) }
    }
}

/// An uninitialized mpv instance, for setting options that must be
/// configured before the player starts (and any other initial properties).
///
/// Created by [`Mpv::builder`]; call [`build`](Builder::build) to start the
/// player. The full [`Handle`] API is available, but most of it returns
/// [`Error::Uninitialized`] until then.
pub struct Builder {
    inner: Handle,
}

impl std::ops::Deref for Builder {
    type Target = Handle;
    fn deref(&self) -> &Handle {
        &self.inner
    }
}

impl Builder {
    /// Set an initial option/property, consuming and returning the builder
    /// for chaining.
    pub fn set_property(self, name: &str, value: impl SetProperty) -> Result<Builder> {
        self.inner.set_property(name, value)?;
        Ok(self)
    }

    /// Initialize the player.
    pub fn build(self) -> Result<Mpv> {
        let this = ManuallyDrop::new(self);
        let raw = this.inner.raw;
        match error::check(unsafe { rsmpv_sys::mpv_initialize(raw.as_ptr()) }) {
            Ok(_) => Ok(Mpv {
                // Move the handle (with its registered callbacks) out.
                inner: unsafe { std::ptr::read(&this.inner) },
                #[cfg(feature = "stream-cb")]
                protocols: Mutex::new(Vec::new()),
            }),
            Err(e) => {
                unsafe { rsmpv_sys::mpv_terminate_destroy(raw.as_ptr()) };
                // Handle has no Drop of its own; this just frees its storage.
                drop(unsafe { std::ptr::read(&this.inner) });
                Err(e)
            }
        }
    }
}

impl Drop for Builder {
    fn drop(&mut self) {
        unsafe { rsmpv_sys::mpv_terminate_destroy(self.as_raw()) }
    }
}

/// A secondary client handle created with [`Mpv::create_client`].
///
/// Has its own event queue, observed properties, and async request state,
/// but controls the same player. Dropping it merely detaches this client
/// (`mpv_destroy`).
pub struct Client<'core> {
    inner: Handle,
    _core: PhantomData<&'core Mpv>,
}

impl std::ops::Deref for Client<'_> {
    type Target = Handle;
    fn deref(&self) -> &Handle {
        &self.inner
    }
}

impl std::ops::DerefMut for Client<'_> {
    fn deref_mut(&mut self) -> &mut Handle {
        &mut self.inner
    }
}

impl Drop for Client<'_> {
    fn drop(&mut self) {
        unsafe { rsmpv_sys::mpv_destroy(self.as_raw()) }
    }
}
