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
//! [`wait_event`](Mpv::wait_event) takes `&self`. `wait_event` takes `&mut
//! self` because libmpv allows only one thread at a time to wait for events
//! on the same handle; to run a blocking event loop on its own thread while
//! controlling playback from others, create a [`Client`] handle per thread
//! with [`Mpv::create_client`]. Shared-state code that can't hold `&mut`
//! (an `Arc<Mpv>` behind a `&self` facade) drains the queue with the
//! non-blocking [`Handle::poll_event`] instead. Pair it with a wakeup
//! callback that only *signals* (wakes a thread, queues a task); the drain
//! itself must happen outside the callback — calling `poll_event` from
//! inside it is forbidden by libmpv and deadlocks.
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
#[cfg(any(feature = "render", feature = "stream-cb"))]
mod escaped;
mod event;
mod node;
mod property;
mod slot;

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

/// Lock a mutex, ignoring poisoning: none of the crate's mutexes guard an
/// invariant a panic could break, and a panicking callback or event decode
/// must not wedge the handle forever.
pub(crate) fn lock_ignore_poison<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// A libmpv client handle: the API surface shared by [`Mpv`], [`Client`],
/// and [`Builder`].
///
/// You never own a bare `Handle`; you reach it through `Deref` from those
/// types.
pub struct Handle {
    raw: NonNull<rsmpv_sys::mpv_handle>,
    /// The wakeup callback slot registered with mpv (see
    /// [`slot::CallbackSlot`] for the sharing and locking story).
    wakeup_slot: slot::CallbackSlot,
    /// Serializes the mpv_wait_event calls made by poll_event(&self).
    /// wait_event(&mut self) doesn't take it: the exclusive borrow already
    /// excludes concurrent poll_event borrows on the same handle.
    event_lock: Mutex<()>,
}

// SAFETY: the libmpv client API is documented as thread-safe. The only
// restriction — a single concurrent mpv_wait_event caller per handle — is
// upheld two ways that cannot overlap: wait_event takes &mut self (an
// exclusive borrow), and poll_event serializes its &self callers through
// event_lock; the borrow rules exclude a wait_event call coexisting with
// any poll_event borrow of the same handle.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Handle {
    fn from_raw(raw: NonNull<rsmpv_sys::mpv_handle>) -> Handle {
        Handle {
            raw,
            wakeup_slot: slot::CallbackSlot::new(),
            event_lock: Mutex::new(()),
        }
    }

    /// The raw `mpv_handle` pointer, for use with [`sys`] as an escape
    /// hatch. The pointer is valid as long as `self` is.
    ///
    /// If you pump `mpv_wait_event` through this pointer yourself, note
    /// that safe code can also reach it via
    /// [`poll_event`](Handle::poll_event) (`&self`) — external
    /// serialization schemes that assume the only path is
    /// [`wait_event`](Mpv::wait_event)'s `&mut self` are not valid.
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

    /// Shared implementation of [`Mpv::wait_event`] and
    /// [`Client::wait_event`] (not public: no public path yields the
    /// `&mut Handle` it would need — the wrappers deliberately lack
    /// `DerefMut`). `&mut self` upholds libmpv's one-waiting-thread-per-
    /// handle rule.
    fn wait_event_impl(&mut self, timeout: f64) -> Option<Event> {
        unsafe { Event::from_raw(rsmpv_sys::mpv_wait_event(self.as_raw(), timeout)) }
    }

    /// Return the next pending event, or `None` if the queue is empty,
    /// without waiting for new events.
    ///
    /// Unlike [`wait_event`](Mpv::wait_event) this takes `&self`, so it
    /// composes with shared ownership (`Arc<Mpv>`, `&self` facades, GUI
    /// callbacks) — it is the natural way to drain the queue in response to
    /// a [`set_wakeup_callback`](Handle::set_wakeup_callback) notification.
    ///
    /// Calls are internally serialized; concurrent callers never block on
    /// mpv's event wait (the underlying call uses a zero timeout), only
    /// briefly on another caller's event decode. Concurrent pollers split
    /// the stream: each event is delivered to exactly one caller.
    ///
    /// Never call this from inside a wakeup callback (it calls
    /// `mpv_wait_event`, which deadlocks there); the callback should only
    /// signal the code that polls. And note for raw-FFI users: this method
    /// reaches `mpv_wait_event` through `&self`, so external serialization
    /// built on [`as_raw`](Handle::as_raw) must account for it.
    pub fn poll_event(&self) -> Option<Event> {
        let _guard = lock_ignore_poison(&self.event_lock);
        // The decode to an owned Event must finish before the lock
        // releases: mpv's event storage is only valid until the next
        // mpv_wait_event call on this handle.
        unsafe { Event::from_raw(rsmpv_sys::mpv_wait_event(self.as_raw(), 0.0)) }
    }

    /// Interrupt the current (or next) [`wait_event`](Mpv::wait_event)
    /// call on this handle.
    pub fn wakeup(&self) {
        unsafe { rsmpv_sys::mpv_wakeup(self.as_raw()) }
    }

    /// Set a callback invoked whenever new events are available. The
    /// callback must only notify — typically waking your event loop, which
    /// then drains events with [`wait_event`](Mpv::wait_event) or
    /// [`poll_event`](Handle::poll_event) — and must not call back into
    /// libmpv (including this method and
    /// [`clear_wakeup_callback`](Handle::clear_wakeup_callback)). Panics
    /// in the callback are caught and ignored.
    ///
    /// The callback runs on arbitrary mpv-internal threads, **may be
    /// invoked synchronously on the calling thread from inside this very
    /// call** (libmpv raises a wakeup immediately on registration), and
    /// may run on several threads at once — hence the [`Sync`] bound.
    ///
    /// Only one callback is active; setting a new one replaces it, and the
    /// old closure is freed as soon as its last in-flight invocation
    /// finishes (this method never waits for one).
    pub fn set_wakeup_callback(&self, callback: impl Fn() + Send + Sync + 'static) {
        self.wakeup_slot.set(callback, |ctx| unsafe {
            rsmpv_sys::mpv_set_wakeup_callback(
                self.as_raw(),
                Some(slot::CallbackSlot::trampoline),
                ctx,
            );
        });
    }

    /// Remove the wakeup callback set with
    /// [`set_wakeup_callback`](Handle::set_wakeup_callback). The closure
    /// is freed as soon as its last in-flight invocation finishes; this
    /// method does not wait for one. (The libmpv unregistration call
    /// itself can briefly block inside libmpv while a callback is being
    /// dispatched, so never make the callback block on the thread that
    /// clears it or drops the handle.)
    ///
    /// Call this to break reference cycles: a wakeup closure capturing an
    /// `Arc<Mpv>` of its own player keeps the core alive forever until the
    /// callback is cleared. Prefer capturing a `Weak<Mpv>`, though: if an
    /// invocation is in flight, the closure's actual release happens when
    /// that dispatch ends, on an mpv-internal thread — and a capture
    /// holding the last `Arc<Mpv>` would run player termination there,
    /// which the callback contract forbids just as it forbids calling
    /// into libmpv directly. The same applies to any capture whose `Drop`
    /// calls into libmpv.
    pub fn clear_wakeup_callback(&self) {
        drop(self.take_wakeup_callback());
    }

    /// Unregister the wakeup callback and hand the removed closure to the
    /// caller instead of dropping it. The wrapper `Drop` impls use this to
    /// defer the closure's `Drop` (arbitrary user code) until after their
    /// destroy/terminate call, so a panic in that `Drop` cannot skip
    /// destroying the handle — for `Mpv`, core termination is also what
    /// lets the protocol registry drop safely.
    fn take_wakeup_callback(&self) -> Option<slot::Callback> {
        self.wakeup_slot.clear(|| unsafe {
            rsmpv_sys::mpv_set_wakeup_callback(self.as_raw(), None, std::ptr::null_mut());
        })
    }

    /// Shared teardown for the wrapper `Drop` impls: unregister the wakeup
    /// callback (narrowing its dispatch window), run `destroy` (the
    /// wrapper's `mpv_destroy`/`mpv_terminate_destroy` call), and only
    /// then release the removed closure. The release runs arbitrary user
    /// `Drop` code, and a panic there must not skip `destroy` — for
    /// [`Mpv`], core termination is also what lets the protocol registry
    /// drop safely after the `Drop` body, unwinding or not. Nothing here
    /// waits for an in-flight callback; `destroy` is what synchronizes
    /// with those.
    fn teardown(&self, destroy: impl FnOnce()) {
        let callback = self.take_wakeup_callback();
        destroy();
        drop(callback);
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
/// core and all clients are destroyed (`mpv_terminate_destroy`); it also
/// unregisters any wakeup callback, which can briefly block while a
/// callback is being dispatched — never make a wakeup callback block on
/// the thread that drops the handle.
///
/// All player functionality is on the derefed [`Handle`].
pub struct Mpv {
    inner: Handle,
    /// Open functions of registered stream protocols. On `Mpv` — the one
    /// wrapper whose `Drop` terminates the core — and deliberately not on
    /// [`Handle`], so a [`Client`] structurally cannot carry registrations
    /// that its non-terminating `mpv_destroy` drop would free while the
    /// live core still calls them. Registrations last until the core dies,
    /// and fields drop after the `Drop` body's terminate_destroy — exactly
    /// the required order.
    #[cfg(feature = "stream-cb")]
    pub(crate) protocols: stream_cb::ProtocolRegistry,
}

impl std::ops::Deref for Mpv {
    type Target = Handle;
    fn deref(&self) -> &Handle {
        &self.inner
    }
}

// No DerefMut: handing out `&mut Handle` would let safe code `mem::swap`
// the underlying handles (with their wakeup slots) between wrappers with
// different teardown semantics — e.g. swapping the main handle into a
// `Client`, whose non-terminating `mpv_destroy` drop is the wrong teardown
// for it, while the `Mpv` wrapper would terminate the core through the
// client's handle. `wait_event`, the one `&mut` method, is provided
// inherently below.

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

    /// Wait up to `timeout` seconds for the next event. `0.0` polls,
    /// negative waits indefinitely. Returns `None` on timeout or spurious
    /// wakeup. The event is deep-copied and owned.
    ///
    /// Takes `&mut self` because libmpv allows only one thread at a time
    /// waiting for events on the same handle; drain from `&self` with the
    /// non-blocking [`poll_event`](Handle::poll_event), or give each
    /// event-loop thread its own [`Client`].
    pub fn wait_event(&mut self, timeout: f64) -> Option<Event> {
        self.inner.wait_event_impl(timeout)
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
        self.inner
            .teardown(|| unsafe { rsmpv_sys::mpv_terminate_destroy(self.as_raw()) });
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
                protocols: stream_cb::ProtocolRegistry::default(),
            }),
            Err(e) => {
                // Hand the builder back to its own Drop impl, whose
                // teardown (unregister, terminate, then release the
                // closure) is exactly what this failure needs.
                drop(ManuallyDrop::into_inner(this));
                Err(e)
            }
        }
    }
}

impl Drop for Builder {
    fn drop(&mut self) {
        self.inner
            .teardown(|| unsafe { rsmpv_sys::mpv_terminate_destroy(self.as_raw()) });
    }
}

/// A secondary client handle created with [`Mpv::create_client`].
///
/// Has its own event queue, observed properties, and async request state,
/// but controls the same player. Dropping it merely detaches this client
/// (`mpv_destroy`); as with [`Mpv`], the drop unregisters any wakeup
/// callback, which can briefly block while a callback is being dispatched.
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

// No DerefMut — see the comment on `Mpv`. `wait_event` is inherent.

impl Client<'_> {
    /// Wait up to `timeout` seconds for the next event; see
    /// [`Mpv::wait_event`]. Takes `&mut self` because libmpv allows
    /// only one waiting thread per handle.
    pub fn wait_event(&mut self, timeout: f64) -> Option<Event> {
        self.inner.wait_event_impl(timeout)
    }
}

impl Drop for Client<'_> {
    fn drop(&mut self) {
        self.inner
            .teardown(|| unsafe { rsmpv_sys::mpv_destroy(self.as_raw()) });
    }
}
