//! Safe wrapper for the mpv render API (embedded video rendering).
//!
//! A render context makes mpv draw video into a target you control — an
//! OpenGL framebuffer or an in-memory software surface — instead of
//! creating its own window. Two flavors expose an identical method
//! surface:
//!
//! - [`RenderContext<'mpv>`] borrows the [`Mpv`]. The borrow checker
//!   proves it is freed before the player terminates — the strictest and
//!   simplest form when your render state and player live in one scope.
//! - [`OwnedRenderContext`] holds an `Arc<Mpv>` instead of a borrow, for
//!   owning wrappers (a struct holding the player and its render state
//!   together, often behind `Arc`/`&self`) where a lifetime-carrying field
//!   would make the struct self-referential.
//!
//! (The two are distinct types on purpose — no `Deref` to a common
//! handle — so safe code can never swap the underlying renderer out from
//! under either flavor's guarantees.)
//!
//! The rules inherited from the C API (and partly enforced here):
//!
//! - Create the context before anything causes a video output to exist
//!   (i.e. before playing something with video), and at most one per player.
//!   Both flavors guarantee structurally that the context is freed before
//!   the player terminates — `RenderContext` by borrowing, and
//!   `OwnedRenderContext` by keeping the core alive through its `Arc`.
//! - Only one render API call may run at a time per context; the methods
//!   take `&mut self` to encode this (an owning wrapper shares an
//!   `OwnedRenderContext` by putting it behind a `Mutex` — that is the
//!   intended composition, not a workaround).
//! - The update callback runs on arbitrary mpv-internal threads and must
//!   only notify (e.g. wake the render thread); it must not call into
//!   libmpv or the render API.
//! - For the OpenGL backend, the GL context must be current **whenever a
//!   render API call happens — including the implicit
//!   `mpv_render_context_free` in `Drop`**. Violating this is undefined
//!   behavior in the C API; in practice it leaks or destroys GL objects in
//!   whatever context happens to be current. This is a dynamic per-call
//!   property the type system cannot capture, so both flavors make their
//!   OpenGL constructor `unsafe` and put the obligation in its contract.
//!   [`RenderContext`] is additionally `!Send` (a conservative
//!   thread-affinity guard); the movable [`OwnedRenderContext`] leaves the
//!   whole obligation to the contract. The software backend has no such
//!   requirement anywhere, and its constructors are safe.
//!
//! # Borrowing and the event loop
//!
//! A live `RenderContext` holds a shared borrow of the [`Mpv`], so `&mut
//! self` methods on that same `Mpv` — notably
//! [`wait_event`](crate::Mpv::wait_event) — become a compile error
//! (E0502) while it exists. Two ways out: drain events with the
//! non-blocking [`poll_event`](crate::Handle::poll_event) (takes `&self`),
//! or run a blocking event loop on a separate handle from
//! [`Mpv::create_client`](crate::Mpv::create_client). An
//! [`OwnedRenderContext`] avoids the borrow entirely.

use std::ffi::{c_void, CStr, CString};
use std::marker::PhantomData;
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
        let status = check(rsmpv_sys::mpv_render_context_create(
            &mut raw,
            mpv,
            params.as_mut_ptr(),
        ));
        match (status, NonNull::new(raw)) {
            (Ok(_), Some(raw)) => Ok(RenderInner {
                raw,
                _get_proc_address: get_proc_address,
                update_slot: CallbackSlot::new(),
            }),
            // mpv does not retain the closure pointer on failure, so
            // dropping `get_proc_address` (freeing it) here is sound.
            (Err(e), _) => Err(e),
            (Ok(_), None) => {
                // Defensive: the C API promises a context on success. If
                // that were ever violated there is no context to free, and
                // whether the closure was retained is unknowable — leak it
                // rather than risk a use-after-free.
                std::mem::forget(get_proc_address);
                Err(Error::CreateFailed)
            }
        }
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

    fn clear_update_callback(&mut self) {
        drop(self.take_update_callback());
    }

    /// Unregister the update callback and hand the removed closure to the
    /// caller instead of dropping it; `Drop` uses this to defer the
    /// closure's `Drop` (arbitrary user code) until after
    /// `mpv_render_context_free`, so a panic there cannot skip freeing the
    /// context.
    fn take_update_callback(&mut self) -> Option<crate::slot::Callback> {
        let raw = self.as_raw();
        self.update_slot.clear(|| unsafe {
            rsmpv_sys::mpv_render_context_set_update_callback(raw, None, std::ptr::null_mut());
        })
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
        format: &str,
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
        // solely on validation inside the linked C library — which is only
        // possible for formats whose pixel size we know, so the documented
        // formats are the only ones accepted here (mpv's undocumented
        // internal format names are available through the sys escape
        // hatch, at your own risk).
        let bpp: usize = match format {
            "rgb0" | "bgr0" | "0bgr" | "0rgb" => 4,
            "rgb24" => 3,
            _ => return Err(Error::InvalidParameter),
        };
        let min_stride = (width as usize)
            .checked_mul(bpp)
            .ok_or(Error::InvalidParameter)?;
        if stride < min_stride {
            return Err(Error::InvalidParameter);
        }
        let format = CString::new(format).map_err(|_| Error::InteriorNul)?;
        let mut size: [c_int; 2] = [width, height];
        let mut stride = stride;
        let mut params = [
            rsmpv_sys::mpv_render_param {
                type_: rsmpv_sys::MPV_RENDER_PARAM_SW_SIZE,
                data: size.as_mut_ptr() as *mut c_void,
            },
            rsmpv_sys::mpv_render_param {
                type_: rsmpv_sys::MPV_RENDER_PARAM_SW_FORMAT,
                data: format.as_ptr() as *mut c_void,
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
        // Unregister the update callback first (narrows the dispatch
        // window), but hold the removed closure across the free: releasing
        // it runs arbitrary user Drop code, and a panic there must not
        // skip mpv_render_context_free — the call that ends mpv's use of
        // the get_proc_address pointer before field drop frees it (the
        // fields drop after this body, unwinding or not).
        let callback = self.take_update_callback();
        unsafe { rsmpv_sys::mpv_render_context_free(self.as_raw()) }
        drop(callback);
    }
}

/// Generates the identical public method surface on both context flavors.
/// Deliberately not `Deref` to a shared handle type: `DerefMut` would let
/// safe code `mem::swap` the renderer between wrappers with different
/// guarantees (lifetime tie, `!Send` guard, the unsafe-constructor
/// obligation).
macro_rules! render_context_methods {
    ($ty:ty) => {
        impl $ty {
            /// The raw `mpv_render_context` pointer, for use with
            /// [`sys`](crate::sys) as an escape hatch. Valid as long as
            /// `self` is.
            pub fn as_raw(&self) -> *mut rsmpv_sys::mpv_render_context {
                self.inner.as_raw()
            }

            /// Set the callback notifying that a new frame is available (or
            /// a redraw is needed). It must only notify — typically waking
            /// the render thread, which then calls [`update`](Self::update)
            /// and, if requested, [`render_opengl`](Self::render_opengl) /
            /// [`render_software`](Self::render_software) — and must not
            /// call into libmpv or the render API (including this method
            /// and [`clear_update_callback`](Self::clear_update_callback)).
            /// Panics in the callback are caught and ignored.
            ///
            /// The callback runs on arbitrary mpv-internal threads, **may
            /// be invoked synchronously on the calling thread from inside
            /// this very call** (registration raises an update callback
            /// immediately), and may run on several threads at once —
            /// hence the [`Sync`] bound.
            ///
            /// Setting a new callback replaces the previous one; the old
            /// closure is freed as soon as its last in-flight invocation
            /// finishes (this method never waits for one).
            pub fn set_update_callback(&mut self, callback: impl Fn() + Send + Sync + 'static) {
                self.inner.set_update_callback(callback)
            }

            /// Remove the update callback; the closure is freed as soon as
            /// its last in-flight invocation finishes (this method does
            /// not wait for one, though the libmpv unregistration call can
            /// briefly block while a callback is being dispatched — never
            /// make the callback block on the thread that clears it or
            /// drops the context). If an invocation is in flight, that
            /// release happens on an mpv-internal thread, so captures
            /// whose `Drop` calls into libmpv or the render API (e.g. a
            /// last `Arc<Mpv>`) are forbidden, exactly like making those
            /// calls from the callback itself.
            pub fn clear_update_callback(&mut self) {
                self.inner.clear_update_callback()
            }

            /// Process pending render work after an update callback fired
            /// (never call it from the callback itself). Returns `true`
            /// when a new frame must be rendered. Optional without advanced
            /// control; mandatory with it.
            pub fn update(&mut self) -> bool {
                self.inner.update()
            }

            /// Information about the next frame to be rendered.
            pub fn next_frame_info(&mut self) -> Result<FrameInfo> {
                self.inner.next_frame_info()
            }

            /// Render the current frame (or redraw the previous one) into
            /// the given OpenGL framebuffer. Set `flip_y` when rendering to
            /// a target with a flipped coordinate system, such as the GL
            /// default framebuffer. By default this blocks until the
            /// frame's target display time; pass
            /// `block_for_target_time = false` to return immediately (then
            /// do your own timing, or set the `video-timing-offset`
            /// property to `0`).
            pub fn render_opengl(
                &mut self,
                fbo: OpenGlFbo,
                flip_y: bool,
                block_for_target_time: bool,
            ) -> Result<()> {
                self.inner.render_opengl(fbo, flip_y, block_for_target_time)
            }

            /// Render the current frame into an in-memory surface (software
            /// renderer only). `format` is a pixel format name — `"rgb0"`,
            /// `"bgr0"`, `"0bgr"`, `"0rgb"` (4 bytes per pixel, the `0`
            /// byte is garbage) or the slow `"rgb24"`. Any other `format`
            /// is rejected with [`Error::InvalidParameter`] before calling
            /// into mpv: the buffer-size check that keeps this method safe
            /// needs a known pixel size, so mpv's undocumented internal
            /// format names are only reachable through the
            /// [`sys`](crate::sys) escape hatch, at your own risk.
            /// `stride` is the byte
            /// distance between rows and must cover `width` pixels; for
            /// performance, make the stride and the buffer start multiples
            /// of 64. `pixels` must hold at least `stride * height` bytes.
            pub fn render_software(
                &mut self,
                width: i32,
                height: i32,
                format: &str,
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
    };
}

/// A live mpv renderer borrowing the player (see the [module docs](self)).
///
/// For the OpenGL backend, the GL context it was created with must be
/// current on this thread for every method call **and when the value is
/// dropped** (drop runs `mpv_render_context_free`) — the obligation
/// carried by the `unsafe` [`new_opengl`](RenderContext::new_opengl)
/// contract. `!Send` additionally pins the value to its creation thread
/// as a conservative guard. If your architecture needs to move the
/// renderer (or store it next to the `Mpv`), use [`OwnedRenderContext`].
pub struct RenderContext<'mpv> {
    inner: RenderInner,
    _core: PhantomData<&'mpv Mpv>,
}

render_context_methods!(RenderContext<'_>);

impl<'mpv> RenderContext<'mpv> {
    /// Create an OpenGL renderer. `get_proc_address` resolves GL functions
    /// by name (wrap `glXGetProcAddressARB`, `wglGetProcAddress`, your
    /// windowing library's loader, etc.); mpv uses only the pointers it
    /// returns and never loads GL itself.
    ///
    /// `advanced_control` enables direct rendering and GPU screenshots, but
    /// obligates you to follow the render API threading rules strictly and
    /// to call [`update`](Self::update) promptly after every update
    /// callback — see `MPV_RENDER_PARAM_ADVANCED_CONTROL`.
    ///
    /// # Safety
    /// GL-context currency is a dynamic, per-call rule the type system
    /// cannot capture (`!Send` only keeps the value on this thread; it
    /// cannot keep a GL context current on it), so by calling this you
    /// take the rule on as an obligation: **the GL context this renderer
    /// is created with must be current on the calling thread now, on every
    /// later method call, and when the value is dropped** (drop runs
    /// `mpv_render_context_free`). Violating it is undefined behavior. (No
    /// such obligation exists for [`new_software`](Self::new_software).)
    pub unsafe fn new_opengl(
        mpv: &'mpv Mpv,
        advanced_control: bool,
        get_proc_address: impl FnMut(&str) -> *mut c_void + 'static,
    ) -> Result<RenderContext<'mpv>> {
        // SAFETY: the handle is valid, and the borrow keeps the core alive
        // for the inner's lifetime.
        let inner = unsafe {
            RenderInner::new_opengl(mpv.as_raw(), advanced_control, Box::new(get_proc_address))?
        };
        Ok(RenderContext {
            inner,
            _core: PhantomData,
        })
    }

    /// Create a software renderer that draws into caller-provided memory
    /// via [`render_software`](Self::render_software). Simple but slow
    /// (everything runs on one CPU thread); mpv recommends it only as a
    /// last resort.
    pub fn new_software(mpv: &'mpv Mpv) -> Result<RenderContext<'mpv>> {
        // SAFETY: the handle is valid, and the borrow keeps the core alive
        // for the inner's lifetime.
        let inner = unsafe { RenderInner::new_software(mpv.as_raw())? };
        Ok(RenderContext {
            inner,
            _core: PhantomData,
        })
    }
}

/// A live mpv renderer that co-owns the player through an `Arc`, for
/// owning wrappers where [`RenderContext`]'s borrow would make a struct
/// self-referential (see the [module docs](self)).
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
/// The methods take `&mut self` because libmpv requires render calls to be
/// serialized; an owning wrapper shares the context by putting it behind a
/// `Mutex` — that is the intended composition.
///
/// # `Send` and the OpenGL contract
///
/// Unlike [`RenderContext`], this type is [`Send`] — that is its purpose.
/// The OpenGL currency rule is per-call, so `Send` cannot carry it;
/// instead, [`new_opengl`](OwnedRenderContext::new_opengl) is `unsafe` and
/// its contract makes the caller responsible for GL currency on whichever
/// threads the value is used and dropped. A context from
/// [`new_software`](OwnedRenderContext::new_software) has no thread
/// requirements at all, so the safe constructor and `Send` are
/// unconditionally sound for it.
pub struct OwnedRenderContext {
    // Declared before `core` so the renderer is freed before the Arc (and
    // thus any final player termination) drops.
    inner: RenderInner,
    core: Arc<Mpv>,
}

// SAFETY: the render API is not thread-affine — render.h permits calls from
// any thread provided they are serialized (enforced by &mut self /
// ownership) and, for OpenGL, the GL context is current — an obligation
// carried by the unsafe `new_opengl` constructor's contract (the safe
// software constructor has no thread requirements). The get_proc_address
// closure is bounded `Send` at construction (the erased type drops the
// bound), and it is only ever invoked behind the exclusive access mpv has
// during our serialized render calls.
unsafe impl Send for OwnedRenderContext {}

render_context_methods!(OwnedRenderContext);

impl OwnedRenderContext {
    /// Create an OpenGL renderer co-owning the player. Semantics match
    /// [`RenderContext::new_opengl`], including requiring the GL context to
    /// be current on the calling thread. `get_proc_address` must be `Send`
    /// because mpv may invoke it after creation, from whichever thread
    /// later render calls run on.
    ///
    /// # Safety
    /// The returned value is [`Send`], which the type system cannot
    /// reconcile with OpenGL's dynamic rule; by calling this you take on
    /// that rule as an obligation: **the GL context this renderer is
    /// created with must be current on the calling thread for every method
    /// call and when the value is dropped**, wherever the value has been
    /// moved. Violating it is undefined behavior. (No such obligation
    /// exists for [`new_software`](OwnedRenderContext::new_software).)
    pub unsafe fn new_opengl(
        core: &Arc<Mpv>,
        advanced_control: bool,
        get_proc_address: impl FnMut(&str) -> *mut c_void + Send + 'static,
    ) -> Result<OwnedRenderContext> {
        // SAFETY: the handle is valid, and the Arc we clone keeps the core
        // alive for the inner's lifetime.
        let inner = unsafe {
            RenderInner::new_opengl(core.as_raw(), advanced_control, Box::new(get_proc_address))?
        };
        Ok(OwnedRenderContext {
            inner,
            core: Arc::clone(core),
        })
    }

    /// Create a software renderer co-owning the player. Semantics match
    /// [`RenderContext::new_software`].
    pub fn new_software(core: &Arc<Mpv>) -> Result<OwnedRenderContext> {
        // SAFETY: the handle is valid, and the Arc we clone keeps the core
        // alive for the inner's lifetime.
        let inner = unsafe { RenderInner::new_software(core.as_raw())? };
        Ok(OwnedRenderContext {
            inner,
            core: Arc::clone(core),
        })
    }

    /// The player core this context keeps alive.
    pub fn core(&self) -> &Arc<Mpv> {
        &self.core
    }
}
