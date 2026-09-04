//! Safe wrapper for the mpv render API (embedded video rendering).
//!
//! A [`RenderContext`] makes mpv draw video into a target you control — an
//! OpenGL framebuffer ([`RenderContext::new_opengl`]) or an in-memory
//! software surface ([`RenderContext::new_software`]) — instead of creating
//! its own window.
//!
//! The rules inherited from the C API (and partly enforced here):
//!
//! - Create the context before anything causes a video output to exist
//!   (i.e. before playing something with video), and at most one per player.
//!   The context borrows the [`Mpv`], so it is necessarily dropped (freed)
//!   before the player — as the C API requires.
//! - `RenderContext` is not [`Send`]: all rendering calls happen from the
//!   thread that owns it, and for OpenGL the same GL context must be current
//!   on that thread. Preferably that is a dedicated render thread which
//!   makes no other libmpv calls.
//! - The update callback runs on arbitrary mpv-internal threads and must
//!   only notify (e.g. wake the render thread); it must not call into
//!   libmpv or the render API.
//! - For the OpenGL backend, the GL context must be current **whenever a
//!   render API call happens — including the implicit
//!   `mpv_render_context_free` in [`RenderContext`]'s `Drop`**. Dropping it
//!   while a different (or no) GL context is current is undefined behavior
//!   in the C API, and in practice leaks or destroys GL objects in whatever
//!   context happens to be current.
//!
//! # Borrowing and the event loop
//!
//! A live `RenderContext` holds a shared borrow of the [`Mpv`], so `&mut
//! self` methods on that same `Mpv` — notably
//! [`wait_event`](crate::Handle::wait_event) — become a compile error
//! (E0502) while it exists. That is deliberate: run the event loop on a
//! separate handle from [`Mpv::create_client`](crate::Mpv::create_client)
//! (cheap, and what the C API expects), and keep the `RenderContext` on the
//! render thread.

use std::ffi::{c_void, CStr, CString};
use std::marker::PhantomData;
use std::os::raw::{c_char, c_int};
use std::ptr::NonNull;

use crate::error::{check, Error, Result};
use crate::Mpv;

type GetProcAddress = Box<dyn FnMut(&str) -> *mut c_void>;
type UpdateCallback = Box<dyn Fn() + Send>;

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

unsafe extern "C" fn update_trampoline(ctx: *mut c_void) {
    let f = &*(ctx as *const UpdateCallback);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
}

/// Describes an OpenGL framebuffer target for
/// [`RenderContext::render_opengl`].
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

/// Information about the next frame, from
/// [`RenderContext::next_frame_info`].
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameInfo {
    /// Whether a next frame exists at all (of any kind, including redraws).
    pub present: bool,
    /// The frame is a redraw request rather than a new video frame.
    pub redraw: bool,
    /// The frame is supposed to reproduce the previous frame perfectly.
    pub repeat: bool,
    /// The player expects the render thread to block on vsync (by delaying
    /// the render call or calling [`RenderContext::report_swap`]).
    pub block_vsync: bool,
    /// Absolute target display time in microseconds, in the time base of
    /// [`Handle::get_time_us`](crate::Handle::get_time_us); `0` for redraws
    /// or vsync-locked timing.
    pub target_time: i64,
}

/// A live mpv renderer bound to a player (see the [module docs](self)).
///
/// For the OpenGL backend, the GL context it was created with must be
/// current on this thread for every method call **and when the value is
/// dropped** (drop runs `mpv_render_context_free`).
pub struct RenderContext<'mpv> {
    raw: NonNull<rsmpv_sys::mpv_render_context>,
    /// The get_proc_address closure; mpv may keep calling it after creation.
    _get_proc_address: Option<Box<GetProcAddress>>,
    /// Update callbacks ever registered; kept alive until drop because a
    /// replaced callback may still be running on an mpv thread.
    #[allow(clippy::vec_box)] // stable addresses; libmpv holds raw pointers
    update_callbacks: Vec<Box<UpdateCallback>>,
    _core: PhantomData<&'mpv Mpv>,
}

impl<'mpv> RenderContext<'mpv> {
    /// Create an OpenGL renderer. `get_proc_address` resolves GL functions
    /// by name (wrap `glXGetProcAddressARB`, `wglGetProcAddress`, your
    /// windowing library's loader, etc.); mpv uses only the pointers it
    /// returns and never loads GL itself. The GL context must be current on
    /// the calling thread, and on every later render API call.
    ///
    /// `advanced_control` enables direct rendering and GPU screenshots, but
    /// obligates you to follow the render API threading rules strictly and
    /// to call [`update`](RenderContext::update) promptly after every update
    /// callback — see `MPV_RENDER_PARAM_ADVANCED_CONTROL`.
    pub fn new_opengl(
        mpv: &'mpv Mpv,
        advanced_control: bool,
        get_proc_address: impl FnMut(&str) -> *mut c_void + 'static,
    ) -> Result<RenderContext<'mpv>> {
        let mut gpa: Box<GetProcAddress> = Box::new(Box::new(get_proc_address));
        let mut init = rsmpv_sys::mpv_opengl_init_params {
            get_proc_address: Some(get_proc_address_trampoline),
            get_proc_address_ctx: &mut *gpa as *mut GetProcAddress as *mut c_void,
        };
        let mut advanced: c_int = advanced_control as c_int;
        let params = [
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
        let raw = Self::create(mpv, params)?;
        Ok(RenderContext {
            raw,
            _get_proc_address: Some(gpa),
            update_callbacks: Vec::new(),
            _core: PhantomData,
        })
    }

    /// Create a software renderer that draws into caller-provided memory via
    /// [`render_software`](RenderContext::render_software). Simple but slow
    /// (everything runs on one CPU thread); mpv recommends it only as a last
    /// resort.
    pub fn new_software(mpv: &'mpv Mpv) -> Result<RenderContext<'mpv>> {
        let params = [
            rsmpv_sys::mpv_render_param {
                type_: rsmpv_sys::MPV_RENDER_PARAM_API_TYPE,
                data: rsmpv_sys::MPV_RENDER_API_TYPE_SW.as_ptr() as *mut c_void,
            },
            rsmpv_sys::mpv_render_param {
                type_: rsmpv_sys::MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        let raw = Self::create(mpv, params)?;
        Ok(RenderContext {
            raw,
            _get_proc_address: None,
            update_callbacks: Vec::new(),
            _core: PhantomData,
        })
    }

    fn create<const N: usize>(
        mpv: &Mpv,
        mut params: [rsmpv_sys::mpv_render_param; N],
    ) -> Result<NonNull<rsmpv_sys::mpv_render_context>> {
        let mut raw: *mut rsmpv_sys::mpv_render_context = std::ptr::null_mut();
        check(unsafe {
            rsmpv_sys::mpv_render_context_create(&mut raw, mpv.as_raw(), params.as_mut_ptr())
        })?;
        NonNull::new(raw).ok_or(Error::CreateFailed)
    }

    /// The raw `mpv_render_context` pointer, for use with
    /// [`sys`](crate::sys) as an escape hatch. Valid as long as `self` is.
    pub fn as_raw(&self) -> *mut rsmpv_sys::mpv_render_context {
        self.raw.as_ptr()
    }

    /// Set the callback notifying that a new frame is available (or a redraw
    /// is needed). It runs on arbitrary mpv-internal threads and must only
    /// notify — typically waking the render thread, which then calls
    /// [`update`](RenderContext::update) and, if requested,
    /// [`render_opengl`](RenderContext::render_opengl) /
    /// [`render_software`](RenderContext::render_software). Panics in the
    /// callback are caught and ignored. Setting a new callback replaces the
    /// previous one (which stays allocated until drop).
    pub fn set_update_callback(&mut self, callback: impl Fn() + Send + 'static) {
        let boxed: Box<UpdateCallback> = Box::new(Box::new(callback));
        let ptr = &*boxed as *const UpdateCallback as *mut c_void;
        self.update_callbacks.push(boxed);
        unsafe {
            rsmpv_sys::mpv_render_context_set_update_callback(
                self.as_raw(),
                Some(update_trampoline),
                ptr,
            );
        }
    }

    /// Process pending render work after an update callback fired (never
    /// call it from the callback itself). Returns `true` when a new frame
    /// must be rendered. Optional without advanced control; mandatory with
    /// it.
    pub fn update(&mut self) -> bool {
        let flags = unsafe { rsmpv_sys::mpv_render_context_update(self.as_raw()) };
        flags & rsmpv_sys::MPV_RENDER_UPDATE_FRAME as u64 != 0
    }

    /// Information about the next frame to be rendered.
    pub fn next_frame_info(&mut self) -> Result<FrameInfo> {
        let mut raw = rsmpv_sys::mpv_render_frame_info {
            flags: 0,
            target_time: 0,
        };
        check(unsafe {
            rsmpv_sys::mpv_render_context_get_info(
                self.as_raw(),
                rsmpv_sys::mpv_render_param {
                    type_: rsmpv_sys::MPV_RENDER_PARAM_NEXT_FRAME_INFO,
                    data: &mut raw as *mut _ as *mut c_void,
                },
            )
        })?;
        let flag = |f: c_int| raw.flags & f as u64 != 0;
        Ok(FrameInfo {
            present: flag(rsmpv_sys::MPV_RENDER_FRAME_INFO_PRESENT),
            redraw: flag(rsmpv_sys::MPV_RENDER_FRAME_INFO_REDRAW),
            repeat: flag(rsmpv_sys::MPV_RENDER_FRAME_INFO_REPEAT),
            block_vsync: flag(rsmpv_sys::MPV_RENDER_FRAME_INFO_BLOCK_VSYNC),
            target_time: raw.target_time,
        })
    }

    /// Render the current frame (or redraw the previous one) into the given
    /// OpenGL framebuffer. Set `flip_y` when rendering to a target with a
    /// flipped coordinate system, such as the GL default framebuffer. By
    /// default this blocks until the frame's target display time; pass
    /// `block_for_target_time = false` to return immediately (then do your
    /// own timing, or set the `video-timing-offset` property to `0`).
    pub fn render_opengl(
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

    /// Render the current frame into an in-memory surface (software renderer
    /// only). `format` is a pixel format name — `"rgb0"`, `"bgr0"`,
    /// `"0bgr"`, `"0rgb"` (4 bytes per pixel, the `0` byte is garbage) or
    /// the slow `"rgb24"`. `stride` is the byte distance between rows and
    /// must cover `width` pixels; for performance, make the stride and the
    /// buffer start multiples of 64. `pixels` must hold at least
    /// `stride * height` bytes.
    pub fn render_software(
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

    /// Tell mpv a frame was just flipped/presented, improving frame timing.
    /// Optional — but once used, use it consistently.
    pub fn report_swap(&mut self) {
        unsafe { rsmpv_sys::mpv_render_context_report_swap(self.as_raw()) }
    }
}

impl Drop for RenderContext<'_> {
    /// Frees the renderer (`mpv_render_context_free`), forcefully disabling
    /// video if still active. For the OpenGL backend the associated GL
    /// context must be current when this runs — see the
    /// [module docs](self).
    fn drop(&mut self) {
        unsafe { rsmpv_sys::mpv_render_context_free(self.as_raw()) }
    }
}
