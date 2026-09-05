//! Safe wrapper for the mpv render API (embedded video rendering).
//!
//! A render context makes mpv draw video into a target you control — an
//! OpenGL framebuffer or an in-memory software surface — instead of
//! creating its own window. [`RenderContext`] is generic over how it holds
//! its player core (the sealed [`CoreRef`] trait):
//!
//! - `RenderContext<&'mpv Mpv>` borrows the [`Mpv`]. The borrow checker
//!   proves it is freed before the player terminates — the strictest and
//!   simplest form when your render state and player live in one scope.
//! - [`OwnedRenderContext`] (= `RenderContext<Arc<Mpv>>`) co-owns the core,
//!   for owning wrappers (a struct holding the player and its render state
//!   together, often behind `Arc`/`&self`) where a lifetime-carrying field
//!   would make the struct self-referential.
//!
//! The rules inherited from the C API (and partly enforced here):
//!
//! - Create the context before anything causes a video output to exist
//!   (i.e. before playing something with video), and at most one per player.
//!   Both flavors guarantee structurally that the context is freed before
//!   the player terminates — the borrowed form by borrowing, and
//!   `OwnedRenderContext` by keeping the core alive through its `Arc`.
//! - Only one render API call may run at a time per context; the methods
//!   take `&mut self` to encode this (see the [`RenderContext`] docs for
//!   the intended sharing composition).
//! - The update callback runs on arbitrary mpv-internal threads and must
//!   only notify (e.g. wake the render thread); it must not call into
//!   libmpv or the render API.
//! - For the OpenGL backend, the GL context must be current **whenever a
//!   render API call happens — including the implicit
//!   `mpv_render_context_free` in `Drop`**. Violating this is undefined
//!   behavior in the C API; in practice it leaks or destroys GL objects in
//!   whatever context happens to be current. This is a dynamic per-call
//!   property the type system cannot capture, so the OpenGL constructor
//!   ([`new_opengl`](RenderContext::new_opengl)) is `unsafe`, putting the
//!   obligation in its contract. The borrowed flavor is additionally
//!   `!Send` (a conservative thread-affinity guard); the movable
//!   [`OwnedRenderContext`] leaves the whole obligation to the contract.
//!   The software backend has no such requirement anywhere, and its
//!   constructor is safe.
//!
//! # Borrowing and the event loop
//!
//! A live `RenderContext<&Mpv>` holds a shared borrow of the [`Mpv`], so
//! `&mut self` methods on that same `Mpv` — notably
//! [`wait_event`](crate::Mpv::wait_event) — become a compile error
//! (E0502) while it exists. Two ways out: drain events with the
//! non-blocking [`poll_event`](crate::Handle::poll_event) (takes `&self`),
//! or run a blocking event loop on a separate handle from
//! [`Mpv::create_client`](crate::Mpv::create_client). An
//! [`OwnedRenderContext`] avoids the borrow entirely.

use std::ffi::{c_void, CStr};
use std::os::raw::{c_char, c_int};
use std::ptr::NonNull;
use std::sync::Arc;

use crate::error::{check, Error, Result};
use crate::escaped::EscapedBox;
use crate::slot::CallbackSlot;
use crate::Mpv;

type GetProcAddress = Box<dyn FnMut(&str) -> *mut c_void>;

unsafe extern "C" fn get_proc_address_trampoline(
    ctx: *mut c_void,
    name: *const c_char,
) -> *mut c_void {
    let f = &mut *(ctx as *mut GetProcAddress);
    let name = CStr::from_ptr(name);
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        f(name.to_str().unwrap_or_default())
    }))
    .unwrap_or(std::ptr::null_mut())
}

/// Describes an OpenGL framebuffer target for `render_opengl`.
#[derive(Debug, Clone, Copy)]
pub struct OpenGlFbo {
    /// FBO name from `glGenFramebuffers()` (complete and color-renderable),
    /// or `0` for the default framebuffer.
    pub fbo: i32,
    /// Width of the framebuffer, in pixels.
    pub width: i32,
    /// Height of the framebuffer, in pixels.
    pub height: i32,
    /// Underlying texture internal format (e.g. `GL_RGBA8`), or `0` if
    /// unknown.
    pub internal_format: i32,
}

/// Information about the next frame, from `next_frame_info`.
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameInfo {
    /// Whether a next frame exists at all (of any kind, including redraws).
    pub present: bool,
    /// The frame is a redraw request rather than a new video frame.
    pub redraw: bool,
    /// The frame is supposed to reproduce the previous frame perfectly.
    pub repeat: bool,
    /// The player expects the render thread to block on vsync (by delaying
    /// the render call or calling `report_swap`).
    pub block_vsync: bool,
    /// Absolute target display time in microseconds, in the time base of
    /// [`Handle::get_time_us`](crate::Handle::get_time_us); `0` for redraws
    /// or vsync-locked timing.
    pub target_time: i64,
}

/// Pixel format of the in-memory surface `render_software` draws into.
///
/// A closed set on purpose: the buffer-size check that keeps
/// `render_software` a safe fn needs a known pixel size. mpv's
/// undocumented internal format names are only reachable through the
/// [`sys`](crate::sys) escape hatch, at your own risk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SwPixelFormat {
    /// `"rgb0"`: 4 bytes per pixel, R-G-B order, the `0` byte is garbage.
    Rgb0,
    /// `"bgr0"`: 4 bytes per pixel, B-G-R order, the `0` byte is garbage.
    Bgr0,
    /// `"0bgr"`: 4 bytes per pixel, garbage byte first, then B-G-R.
    ZeroBgr,
    /// `"0rgb"`: 4 bytes per pixel, garbage byte first, then R-G-B.
    ZeroRgb,
    /// `"rgb24"`: 3 bytes per pixel, R-G-B; notably slower in mpv.
    Rgb24,
}

impl SwPixelFormat {
    fn bytes_per_pixel(self) -> usize {
        // Exhaustive on purpose: this feeds the buffer-size check that
        // keeps render_software a safe fn, so a future variant must state
        // its size here instead of inheriting a wildcard's.
        match self {
            SwPixelFormat::Rgb0
            | SwPixelFormat::Bgr0
            | SwPixelFormat::ZeroBgr
            | SwPixelFormat::ZeroRgb => 4,
            SwPixelFormat::Rgb24 => 3,
        }
    }

    fn as_cstr(self) -> &'static CStr {
        match self {
            SwPixelFormat::Rgb0 => c"rgb0",
            SwPixelFormat::Bgr0 => c"bgr0",
            SwPixelFormat::ZeroBgr => c"0bgr",
            SwPixelFormat::ZeroRgb => c"0rgb",
            SwPixelFormat::Rgb24 => c"rgb24",
        }
    }
}

/// The private renderer state shared by both flavors. Never public: safe
/// code must not be able to move it between wrappers with different
/// guarantees.
struct RenderInner {
    raw: NonNull<rsmpv_sys::mpv_render_context>,
    /// The get_proc_address closure, or `None` for backends without one.
    /// mpv keeps the pointer and may call through it while this struct
    /// moves — hence [`EscapedBox`] rather than a live `Box`. Declared
    /// after `raw` but freed by field drop only after `Drop`'s
    /// `mpv_render_context_free`, which is what ends mpv's use of it.
    _get_proc_address: Option<EscapedBox<GetProcAddress>>,
    /// The update callback slot registered with mpv (see
    /// [`CallbackSlot`] for the sharing and locking story).
    update_slot: CallbackSlot,
}

impl RenderInner {
    /// # Safety
    /// `mpv` must be a valid client handle whose core outlives the
    /// returned value.
    unsafe fn new_opengl(
        mpv: *mut rsmpv_sys::mpv_handle,
        advanced_control: bool,
        get_proc_address: GetProcAddress,
    ) -> Result<RenderInner> {
        // The double box is load-bearing: `ctx` must be a thin pointer,
        // so mpv is handed the address of the fat `Box<dyn>` itself (the
        // trampoline reads it as `&mut GetProcAddress`). The outer
        // allocation is what EscapedBox owns.
        let gpa = EscapedBox::new(Box::new(get_proc_address));
        let mut init = rsmpv_sys::mpv_opengl_init_params {
            get_proc_address: Some(get_proc_address_trampoline),
            get_proc_address_ctx: gpa.as_ptr() as *mut c_void,
        };
        let mut advanced: c_int = advanced_control as c_int;
        let mut params = [
            rsmpv_sys::mpv_render_param {
                type_: rsmpv_sys::MPV_RENDER_PARAM_API_TYPE,
                data: rsmpv_sys::MPV_RENDER_API_TYPE_OPENGL.as_ptr() as *mut c_void,
            },
            rsmpv_sys::mpv_render_param {
                type_: rsmpv_sys::MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                data: &mut init as *mut _ as *mut c_void,
            },
            rsmpv_sys::mpv_render_param {
                type_: rsmpv_sys::MPV_RENDER_PARAM_ADVANCED_CONTROL,
                data: &mut advanced as *mut c_int as *mut c_void,
            },
            rsmpv_sys::mpv_render_param {
                type_: rsmpv_sys::MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        Self::create(mpv, &mut params, Some(gpa))
    }

    /// # Safety
    /// `mpv` must be a valid client handle whose core outlives the
    /// returned value.
    unsafe fn new_software(mpv: *mut rsmpv_sys::mpv_handle) -> Result<RenderInner> {
        let mut params = [
            rsmpv_sys::mpv_render_param {
                type_: rsmpv_sys::MPV_RENDER_PARAM_API_TYPE,
                data: rsmpv_sys::MPV_RENDER_API_TYPE_SW.as_ptr() as *mut c_void,
            },
            rsmpv_sys::mpv_render_param {
                type_: rsmpv_sys::MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        Self::create(mpv, &mut params, None)
    }

    /// # Safety
    /// See `new_opengl`/`new_software`.
    unsafe fn create(
        mpv: *mut rsmpv_sys::mpv_handle,
        params: &mut [rsmpv_sys::mpv_render_param],
        get_proc_address: Option<EscapedBox<GetProcAddress>>,
    ) -> Result<RenderInner> {
        let mut raw: *mut rsmpv_sys::mpv_render_context = std::ptr::null_mut();
        // On failure mpv does not retain the closure pointer, so the `?`
        // dropping `get_proc_address` (freeing it) is sound.
        check(rsmpv_sys::mpv_render_context_create(
            &mut raw,
            mpv,
            params.as_mut_ptr(),
        ))?;
        let Some(raw) = NonNull::new(raw) else {
            // Contract violated (success without a context): whether mpv
            // retained the closure pointer is unknowable, so leak the
            // closure rather than free it during the unwind.
            std::mem::forget(get_proc_address);
            panic!("mpv_render_context_create returned success without a context");
        };
        Ok(RenderInner {
            raw,
            _get_proc_address: get_proc_address,
            update_slot: CallbackSlot::new(),
        })
    }

    fn as_raw(&self) -> *mut rsmpv_sys::mpv_render_context {
        self.raw.as_ptr()
    }

    fn set_update_callback(&mut self, callback: impl Fn() + Send + Sync + 'static) {
        let raw = self.as_raw();
        self.update_slot.set(callback, |ctx| unsafe {
            rsmpv_sys::mpv_render_context_set_update_callback(
                raw,
                Some(CallbackSlot::trampoline),
                ctx,
            );
        });
    }

    /// Update-slot teardown for `clear_update_callback` (no-op `destroy`)
    /// and `Drop` (the context free); see [`CallbackSlot::teardown`] for
    /// the ordering rationale.
    fn teardown(&mut self, destroy: impl FnOnce()) {
        let raw = self.as_raw();
        self.update_slot.teardown(
            || unsafe {
                rsmpv_sys::mpv_render_context_set_update_callback(raw, None, std::ptr::null_mut());
            },
            destroy,
        );
    }

    fn update(&mut self) -> bool {
        let flags = unsafe { rsmpv_sys::mpv_render_context_update(self.as_raw()) };
        flags & rsmpv_sys::MPV_RENDER_UPDATE_FRAME as u64 != 0
    }

    fn next_frame_info(&mut self) -> Result<FrameInfo> {
        let mut info = rsmpv_sys::mpv_render_frame_info {
            flags: 0,
            target_time: 0,
        };
        check(unsafe {
            rsmpv_sys::mpv_render_context_get_info(
                self.as_raw(),
                rsmpv_sys::mpv_render_param {
                    type_: rsmpv_sys::MPV_RENDER_PARAM_NEXT_FRAME_INFO,
                    data: &mut info as *mut _ as *mut c_void,
                },
            )
        })?;
        let flag = |f: c_int| info.flags & f as u64 != 0;
        Ok(FrameInfo {
            present: flag(rsmpv_sys::MPV_RENDER_FRAME_INFO_PRESENT),
            redraw: flag(rsmpv_sys::MPV_RENDER_FRAME_INFO_REDRAW),
            repeat: flag(rsmpv_sys::MPV_RENDER_FRAME_INFO_REPEAT),
            block_vsync: flag(rsmpv_sys::MPV_RENDER_FRAME_INFO_BLOCK_VSYNC),
            target_time: info.target_time,
        })
    }

    fn render_opengl(
        &mut self,
        fbo: OpenGlFbo,
        flip_y: bool,
        block_for_target_time: bool,
    ) -> Result<()> {
        let mut raw_fbo = rsmpv_sys::mpv_opengl_fbo {
            fbo: fbo.fbo,
            w: fbo.width,
            h: fbo.height,
            internal_format: fbo.internal_format,
        };
        let mut flip: c_int = flip_y as c_int;
        let mut block: c_int = block_for_target_time as c_int;
        let mut params = [
            rsmpv_sys::mpv_render_param {
                type_: rsmpv_sys::MPV_RENDER_PARAM_OPENGL_FBO,
                data: &mut raw_fbo as *mut _ as *mut c_void,
            },
            rsmpv_sys::mpv_render_param {
                type_: rsmpv_sys::MPV_RENDER_PARAM_FLIP_Y,
                data: &mut flip as *mut c_int as *mut c_void,
            },
            rsmpv_sys::mpv_render_param {
                type_: rsmpv_sys::MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME,
                data: &mut block as *mut c_int as *mut c_void,
            },
            rsmpv_sys::mpv_render_param {
                type_: rsmpv_sys::MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        check(unsafe { rsmpv_sys::mpv_render_context_render(self.as_raw(), params.as_mut_ptr()) })
            .map(|_| ())
    }

    fn render_software(
        &mut self,
        width: i32,
        height: i32,
        format: SwPixelFormat,
        stride: usize,
        pixels: &mut [u8],
    ) -> Result<()> {
        let needed = stride
            .checked_mul(height.max(0) as usize)
            .ok_or(Error::InvalidParameter)?;
        if width < 0 || height < 0 || pixels.len() < needed {
            return Err(Error::InvalidParameter);
        }
        // mpv's software renderer (video/out/libmpv_sw.c) rejects
        // stride < width * bytes-per-pixel (and misaligned strides) with
        // MPV_ERROR_INVALID_PARAMETER before touching the buffer, which
        // together with the length check above bounds every row it writes
        // (stride * (height - 1) + width * bpp <= stride * height).
        // Duplicate the check so this safe fn's soundness doesn't rest
        // solely on validation inside the linked C library; the closed
        // SwPixelFormat set is what makes the pixel size knowable here.
        let min_stride = (width as usize)
            .checked_mul(format.bytes_per_pixel())
            .ok_or(Error::InvalidParameter)?;
        if stride < min_stride {
            return Err(Error::InvalidParameter);
        }
        let mut size: [c_int; 2] = [width, height];
        let mut stride = stride;
        let mut params = [
            rsmpv_sys::mpv_render_param {
                type_: rsmpv_sys::MPV_RENDER_PARAM_SW_SIZE,
                data: size.as_mut_ptr() as *mut c_void,
            },
            rsmpv_sys::mpv_render_param {
                type_: rsmpv_sys::MPV_RENDER_PARAM_SW_FORMAT,
                data: format.as_cstr().as_ptr() as *mut c_void,
            },
            rsmpv_sys::mpv_render_param {
                type_: rsmpv_sys::MPV_RENDER_PARAM_SW_STRIDE,
                data: &mut stride as *mut usize as *mut c_void,
            },
            rsmpv_sys::mpv_render_param {
                type_: rsmpv_sys::MPV_RENDER_PARAM_SW_POINTER,
                data: pixels.as_mut_ptr() as *mut c_void,
            },
            rsmpv_sys::mpv_render_param {
                type_: rsmpv_sys::MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        check(unsafe { rsmpv_sys::mpv_render_context_render(self.as_raw(), params.as_mut_ptr()) })
            .map(|_| ())
    }

    fn report_swap(&mut self) {
        unsafe { rsmpv_sys::mpv_render_context_report_swap(self.as_raw()) }
    }
}

impl Drop for RenderInner {
    fn drop(&mut self) {
        let raw = self.as_raw();
        self.teardown(|| unsafe { rsmpv_sys::mpv_render_context_free(raw) });
    }
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for &crate::Mpv {}
    impl Sealed for std::sync::Arc<crate::Mpv> {}
}

/// How a [`RenderContext`] holds its player core: a borrow (`&Mpv`) or
/// shared ownership (`Arc<Mpv>`). Sealed — exactly those two forms exist;
/// see the [module docs](self) for choosing between them.
pub trait CoreRef: sealed::Sealed {
    /// The (boxed) `get_proc_address` closure
    /// [`new_opengl`](RenderContext::new_opengl) accepts for this core
    /// form: plain for a borrow (the context is `!Send`, so the closure
    /// never changes threads), `+ Send` for `Arc` (the context — and with
    /// it the closure, which mpv may invoke during any later render
    /// call — can move threads).
    type GetProcAddress;

    #[doc(hidden)]
    fn handle_ptr(&self) -> *mut rsmpv_sys::mpv_handle;
    #[doc(hidden)]
    fn erase_gpa(gpa: Self::GetProcAddress) -> GetProcAddress;
}

impl CoreRef for &Mpv {
    type GetProcAddress = Box<dyn FnMut(&str) -> *mut c_void + 'static>;
    fn handle_ptr(&self) -> *mut rsmpv_sys::mpv_handle {
        self.as_raw()
    }
    fn erase_gpa(gpa: Self::GetProcAddress) -> GetProcAddress {
        gpa
    }
}

impl CoreRef for Arc<Mpv> {
    type GetProcAddress = Box<dyn FnMut(&str) -> *mut c_void + Send + 'static>;
    fn handle_ptr(&self) -> *mut rsmpv_sys::mpv_handle {
        self.as_raw()
    }
    fn erase_gpa(gpa: Self::GetProcAddress) -> GetProcAddress {
        gpa
    }
}

/// A live mpv renderer bound to a player (see the [module docs](self) for
/// the two [`CoreRef`] flavors and the rules both must follow).
///
/// For the OpenGL backend, the GL context it was created with must be
/// current for every method call **and when the value is dropped** (drop
/// runs `mpv_render_context_free`) — the obligation carried by the
/// `unsafe` `new_opengl` constructors. The borrowed flavor is `!Send` as
/// a conservative additional guard; [`OwnedRenderContext`] is [`Send`].
///
/// The methods take `&mut self` because libmpv requires render calls to
/// be serialized; an owning wrapper shares an `OwnedRenderContext` by
/// putting it behind a `Mutex` — that is the intended composition.
pub struct RenderContext<C: CoreRef> {
    // Declared before `core` so the renderer is freed before the core
    // reference (and, for `Arc`, any final player termination) drops.
    inner: RenderInner,
    core: C,
}

/// A [`RenderContext`] that co-owns the player through an `Arc`, for
/// owning wrappers where a borrow would make the struct self-referential.
///
/// The `Arc` makes the required teardown ordering structural instead of
/// disciplinary: dropping this context frees the renderer first, and the
/// player terminates only when the last `Arc<Mpv>` clone — possibly the
/// context's own — goes away. One behavioral consequence: dropping the
/// last *user-visible* `Arc<Mpv>` while a context is alive defers player
/// termination until the context drops (for a wrapper that drops both
/// together, a non-event). If this held the last `Arc<Mpv>`, its drop also
/// runs the (blocking) player termination.
///
/// Unlike the borrowed flavor this type is [`Send`] — that is its purpose.
/// The OpenGL currency rule is per-call, so `Send` cannot carry it; the
/// `unsafe` [`new_opengl`](OwnedRenderContext::new_opengl) contract makes
/// the caller responsible for GL currency on whichever threads the value
/// is used and dropped. A context from
/// [`new_software`](OwnedRenderContext::new_software) has no thread
/// requirements at all, so the safe constructor and `Send` are
/// unconditionally sound for it.
pub type OwnedRenderContext = RenderContext<Arc<Mpv>>;

// SAFETY: the render API is not thread-affine — render.h permits calls from
// any thread provided they are serialized (enforced by &mut self /
// ownership) and, for OpenGL, the GL context is current — an obligation
// carried by the unsafe `new_opengl` constructor's contract (the safe
// software constructor has no thread requirements). The get_proc_address
// closure is bounded `Send` at construction (the erased type drops the
// bound), it is only ever invoked behind the exclusive access mpv has
// during our serialized render calls, and Arc<Mpv> is itself Send + Sync.
// The borrowed flavor gets no such impl: RenderContext<&Mpv> stays !Send
// as a conservative thread-affinity guard.
unsafe impl Send for RenderContext<Arc<Mpv>> {}

// Structurally tie the impl above to the premises it relies on: this
// stops compiling if <Arc<Mpv> as CoreRef>::GetProcAddress ever loses
// `+ Send` or if Arc<Mpv> ever stops being Send + Sync (an auto-trait
// conclusion that a future !Send/!Sync field in Mpv would silently
// change), and pins the public promise that OwnedRenderContext is Send.
const _: () = {
    const fn assert_send<T: Send>() {}
    const fn assert_sync<T: Sync>() {}
    assert_send::<<Arc<Mpv> as CoreRef>::GetProcAddress>();
    assert_send::<Arc<Mpv>>();
    assert_sync::<Arc<Mpv>>();
    assert_send::<OwnedRenderContext>();
};

impl<C: CoreRef> RenderContext<C> {
    /// The raw `mpv_render_context` pointer, for use with
    /// [`sys`](crate::sys) as an escape hatch. Valid as long as `self` is.
    pub fn as_raw(&self) -> *mut rsmpv_sys::mpv_render_context {
        self.inner.as_raw()
    }

    /// Set the callback notifying that a new frame is available (or a
    /// redraw is needed). It must only notify — typically waking the
    /// render thread, which then calls [`update`](Self::update) and, if
    /// requested, [`render_opengl`](Self::render_opengl) /
    /// [`render_software`](Self::render_software) — and must not call into
    /// libmpv or the render API (including this method and
    /// [`clear_update_callback`](Self::clear_update_callback)). Panics in
    /// the callback are caught and ignored.
    ///
    /// The callback runs on arbitrary mpv-internal threads, **may be
    /// invoked synchronously on the calling thread from inside this very
    /// call** (registration raises an update callback immediately), and
    /// may run on several threads at once — hence the [`Sync`] bound.
    ///
    /// Setting a new callback replaces the previous one; the replaced
    /// closure is released under the same rules as
    /// [`clear_update_callback`](Self::clear_update_callback) — freed
    /// when its last in-flight invocation finishes, possibly on an
    /// mpv-internal thread.
    pub fn set_update_callback(&mut self, callback: impl Fn() + Send + Sync + 'static) {
        self.inner.set_update_callback(callback)
    }

    /// Remove the update callback; the closure is freed as soon as its
    /// last in-flight invocation finishes (this method does not wait for
    /// one, though the libmpv unregistration call can briefly block while
    /// a callback is being dispatched — never make the callback block on
    /// the thread that clears it or drops the context). If an invocation
    /// is in flight, that release happens on an mpv-internal thread, so
    /// captures whose `Drop` calls into libmpv or the render API (e.g. a
    /// last `Arc<Mpv>`) are forbidden, exactly like making those calls
    /// from the callback itself.
    pub fn clear_update_callback(&mut self) {
        self.inner.teardown(|| ())
    }

    /// Process pending render work after an update callback fired (never
    /// call it from the callback itself). Returns `true` when a new frame
    /// must be rendered. Optional without advanced control; mandatory
    /// with it.
    pub fn update(&mut self) -> bool {
        self.inner.update()
    }

    /// Information about the next frame to be rendered.
    pub fn next_frame_info(&mut self) -> Result<FrameInfo> {
        self.inner.next_frame_info()
    }

    /// Render the current frame (or redraw the previous one) into the
    /// given OpenGL framebuffer. Set `flip_y` when rendering to a target
    /// with a flipped coordinate system, such as the GL default
    /// framebuffer. By default this blocks until the frame's target
    /// display time; pass `block_for_target_time = false` to return
    /// immediately (then do your own timing, or set the
    /// `video-timing-offset` property to `0`).
    pub fn render_opengl(
        &mut self,
        fbo: OpenGlFbo,
        flip_y: bool,
        block_for_target_time: bool,
    ) -> Result<()> {
        self.inner.render_opengl(fbo, flip_y, block_for_target_time)
    }

    /// Render the current frame into an in-memory surface (software
    /// renderer only), in the given [`SwPixelFormat`]. `stride` is the
    /// byte distance between rows and must cover `width` pixels; for
    /// performance, make the stride and the buffer start multiples of 64.
    /// `pixels` must hold at least `stride * height` bytes.
    pub fn render_software(
        &mut self,
        width: i32,
        height: i32,
        format: SwPixelFormat,
        stride: usize,
        pixels: &mut [u8],
    ) -> Result<()> {
        self.inner
            .render_software(width, height, format, stride, pixels)
    }

    /// Tell mpv a frame was just flipped/presented, improving frame
    /// timing. Optional — but once used, use it consistently.
    pub fn report_swap(&mut self) {
        self.inner.report_swap()
    }
}

impl<C: CoreRef> RenderContext<C> {
    /// Create an OpenGL renderer. `core` is a `&Mpv` borrow or an
    /// `Arc<Mpv>` (see [`CoreRef`]). `get_proc_address` resolves GL
    /// functions by name (wrap `glXGetProcAddressARB`,
    /// `wglGetProcAddress`, your windowing library's loader, etc., in a
    /// `Box`); mpv uses only the pointers it returns and never loads GL
    /// itself. For an `Arc` core the closure must be `Send`, because mpv
    /// may invoke it during any later render call, from whichever thread
    /// the movable context has ended up on.
    ///
    /// `advanced_control` enables direct rendering and GPU screenshots, but
    /// obligates you to follow the render API threading rules strictly and
    /// to call [`update`](Self::update) promptly after every update
    /// callback — see `MPV_RENDER_PARAM_ADVANCED_CONTROL`.
    ///
    /// On error the passed `core` is dropped with the rest of the failed
    /// construction — for an `Arc` core, pass a clone (as usual) so a
    /// failure can't release your last reference, whose drop would run
    /// blocking player termination.
    ///
    /// # Safety
    /// GL-context currency is a dynamic, per-call rule the type system
    /// cannot capture, so by calling this you take it on as an obligation:
    /// **the GL context this renderer is created with must be current on
    /// the calling thread now, on every later method call, and when the
    /// value is dropped** (drop runs `mpv_render_context_free`) — wherever
    /// a movable (`Arc`-core) context has been moved; the borrowed flavor
    /// is `!Send`, but that only pins the thread, it cannot keep a GL
    /// context current on it. Violating the rule is undefined behavior.
    /// (No such obligation exists for
    /// [`new_software`](Self::new_software).)
    pub unsafe fn new_opengl(
        core: C,
        advanced_control: bool,
        get_proc_address: C::GetProcAddress,
    ) -> Result<RenderContext<C>> {
        // SAFETY: the handle is valid, and the `core` field (borrow or
        // Arc) keeps the core alive for the inner's lifetime.
        let inner = unsafe {
            RenderInner::new_opengl(
                core.handle_ptr(),
                advanced_control,
                C::erase_gpa(get_proc_address),
            )?
        };
        Ok(RenderContext { inner, core })
    }

    /// Create a software renderer that draws into caller-provided memory
    /// via [`render_software`](Self::render_software). `core` is a `&Mpv`
    /// borrow or an `Arc<Mpv>` (see [`CoreRef`]); on error it is dropped —
    /// see [`new_opengl`](Self::new_opengl)'s note about passing an `Arc`
    /// clone. Simple but slow (everything runs on one CPU thread); mpv
    /// recommends it only as a last resort.
    pub fn new_software(core: C) -> Result<RenderContext<C>> {
        // SAFETY: the handle is valid, and the `core` field (borrow or
        // Arc) keeps the core alive for the inner's lifetime.
        let inner = unsafe { RenderInner::new_software(core.handle_ptr())? };
        Ok(RenderContext { inner, core })
    }
}

impl RenderContext<Arc<Mpv>> {
    /// The player core this context keeps alive.
    pub fn core(&self) -> &Arc<Mpv> {
        &self.core
    }
}
