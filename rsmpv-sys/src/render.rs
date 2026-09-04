//! Bindings for the mpv render API (`mpv/render.h` and `mpv/render_gl.h`).
//!
//! The render API drives mpv's video output through a graphics API controlled
//! by the caller (OpenGL, or a software surface). Key contract points from
//! the header, summarized:
//!
//! - Create the context with [`mpv_render_context_create`] before playback
//!   causes a VO to be created; at most one render context can exist per mpv
//!   core, and it must be freed with [`mpv_render_context_free`] before the
//!   core is destroyed.
//! - Only one `mpv_render_*` call may run at a time per context, none of them
//!   may be made from inside wakeup/update callbacks, and for the OpenGL
//!   backend the same GL context must be current on the calling thread.
//! - Rendering should live on its own thread that never blocks on other
//!   libmpv calls; only functions documented as "safe to be called from mpv
//!   render API threads" may be used there. Violations degrade playback, and
//!   with [`MPV_RENDER_PARAM_ADVANCED_CONTROL`] enabled they deadlock.

use core::ffi::{c_char, c_int, c_void, CStr};

use crate::mpv_handle;

/// Opaque render context, created by [`mpv_render_context_create`].
#[repr(C)]
pub struct mpv_render_context {
    _opaque: [u8; 0],
}

/// Parameter types used with [`mpv_render_param`]. Each constant documents
/// the pointee type that `mpv_render_param.data` must have.
pub type mpv_render_param_type = c_int;

/// Not a valid value; terminates a params array. Always `0`.
pub const MPV_RENDER_PARAM_INVALID: mpv_render_param_type = 0;
/// The render API to use (for `mpv_render_context_create`). Type: `char*`;
/// see [`MPV_RENDER_API_TYPE_OPENGL`] and [`MPV_RENDER_API_TYPE_SW`].
pub const MPV_RENDER_PARAM_API_TYPE: mpv_render_param_type = 1;
/// Required OpenGL init parameters (for `mpv_render_context_create`).
/// Type: `mpv_opengl_init_params*`.
pub const MPV_RENDER_PARAM_OPENGL_INIT_PARAMS: mpv_render_param_type = 2;
/// The GL render target (for `mpv_render_context_render`).
/// Type: `mpv_opengl_fbo*`.
pub const MPV_RENDER_PARAM_OPENGL_FBO: mpv_render_param_type = 3;
/// Render flipped if the pointed-to int is non-zero (for
/// `mpv_render_context_render`, e.g. when targeting the GL default
/// framebuffer). Type: `int*`.
pub const MPV_RENDER_PARAM_FLIP_Y: mpv_render_param_type = 4;
/// Surface depth in bits per channel, `0` meaning 8; controls dithering (for
/// `mpv_render_context_render`). Type: `int*`.
pub const MPV_RENDER_PARAM_DEPTH: mpv_render_param_type = 5;
/// ICC profile blob for the `icc-profile-auto` option (for
/// `mpv_render_context_set_parameter`). Type: `mpv_byte_array*`.
pub const MPV_RENDER_PARAM_ICC_PROFILE: mpv_render_param_type = 6;
/// Ambient light in lux (for `mpv_render_context_set_parameter`).
/// Type: `int*`.
#[deprecated(note = "deprecated in the mpv client API")]
pub const MPV_RENDER_PARAM_AMBIENT_LIGHT: mpv_render_param_type = 7;
/// X11 `Display*`, sometimes used for hwdec (for
/// `mpv_render_context_create`); must stay valid for the context's lifetime.
pub const MPV_RENDER_PARAM_X11_DISPLAY: mpv_render_param_type = 8;
/// `struct wl_display*`, sometimes used for hwdec (for
/// `mpv_render_context_create`); must stay valid for the context's lifetime.
pub const MPV_RENDER_PARAM_WL_DISPLAY: mpv_render_param_type = 9;
/// Enable advanced control (for `mpv_render_context_create`). Type: `int*`
/// (`0` disable/default, `1` enable). Enables direct rendering and GPU
/// screenshots, but obligates the caller to follow the threading rules
/// strictly (never wait on the core from the render thread; always call
/// [`mpv_render_context_update`] promptly after update callbacks) — or the
/// core will deadlock.
pub const MPV_RENDER_PARAM_ADVANCED_CONTROL: mpv_render_param_type = 10;
/// Return information about the next frame to render (for
/// `mpv_render_context_get_info`). Type: `mpv_render_frame_info*`.
pub const MPV_RENDER_PARAM_NEXT_FRAME_INFO: mpv_render_param_type = 11;
/// Enable (`1`, default) or disable (`0`) blocking until the target display
/// time in `mpv_render_context_render`. Type: `int*`. If disabled, do your
/// own timing via [`mpv_render_frame_info`] or set `video-timing-offset` to
/// `0`, or A/V sync will be slightly off.
pub const MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME: mpv_render_param_type = 12;
/// Skip rendering the frame (for `mpv_render_context_render`). Type: `int*`
/// (`0` render/default, `1` skip). The frame still counts as rendered.
pub const MPV_RENDER_PARAM_SKIP_RENDERING: mpv_render_param_type = 13;
/// Type: `struct mpv_opengl_drm_params*`.
#[deprecated(note = "not supported; use MPV_RENDER_PARAM_DRM_DISPLAY_V2")]
pub const MPV_RENDER_PARAM_DRM_DISPLAY: mpv_render_param_type = 14;
/// DRM draw surface dimensions (for `mpv_render_context_create`).
/// Type: `struct mpv_opengl_drm_draw_surface_size*`.
pub const MPV_RENDER_PARAM_DRM_DRAW_SURFACE_SIZE: mpv_render_param_type = 15;
/// DRM display handles (for `mpv_render_context_create`).
/// Type: `struct mpv_opengl_drm_params_v2*`.
pub const MPV_RENDER_PARAM_DRM_DISPLAY_V2: mpv_render_param_type = 16;
/// Software renderer only: target surface size `{w, h}`, mandatory (for
/// `mpv_render_context_render`). Type: `int[2]`.
pub const MPV_RENDER_PARAM_SW_SIZE: mpv_render_param_type = 17;
/// Software renderer only: target surface pixel format, mandatory (for
/// `mpv_render_context_render`). Type: `char*`; `"rgb0"`, `"bgr0"`,
/// `"0bgr"`, `"0rgb"` (4 bytes per pixel; the `0` byte is garbage) or the
/// discouraged, slow `"rgb24"`.
pub const MPV_RENDER_PARAM_SW_FORMAT: mpv_render_param_type = 18;
/// Software renderer only: target surface bytes per line, mandatory (for
/// `mpv_render_context_render`). Type: `size_t*`. Must be a multiple of the
/// pixel size; stride and pointer should be 64-byte aligned for performance
/// and must at least match the pixel alignment.
pub const MPV_RENDER_PARAM_SW_STRIDE: mpv_render_param_type = 19;
/// Software renderer only: pointer to the first (top-left) pixel of the
/// target surface, mandatory (for `mpv_render_context_render`). Type:
/// `void*`. Line `y` starts at `pointer + stride * y`; everything up to
/// `pointer + stride * h` must be writable.
pub const MPV_RENDER_PARAM_SW_POINTER: mpv_render_param_type = 20;

/// Backwards-compatible alias for [`MPV_RENDER_PARAM_DRM_DRAW_SURFACE_SIZE`].
pub const MPV_RENDER_PARAM_DRM_OSD_SIZE: mpv_render_param_type =
    MPV_RENDER_PARAM_DRM_DRAW_SURFACE_SIZE;

/// A typed parameter passed to `mpv_render_*` functions. `data` points to a
/// value of the type documented on the `MPV_RENDER_PARAM_*` constant used as
/// `type_`. Parameter arrays are terminated by an entry with
/// `type_ == MPV_RENDER_PARAM_INVALID` (`0`), order is irrelevant, and
/// pointers only need to stay valid for the duration of the call unless
/// documented otherwise.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct mpv_render_param {
    pub type_: mpv_render_param_type,
    pub data: *mut c_void,
}

/// [`MPV_RENDER_PARAM_API_TYPE`] value for the OpenGL backend (desktop GL 2.1+
/// or GLES 2.0+); requires [`MPV_RENDER_PARAM_OPENGL_INIT_PARAMS`].
pub const MPV_RENDER_API_TYPE_OPENGL: &CStr = c"opengl";
/// [`MPV_RENDER_PARAM_API_TYPE`] value for the (slow) software renderer.
pub const MPV_RENDER_API_TYPE_SW: &CStr = c"sw";

/// Bit flags for [`mpv_render_frame_info::flags`].
pub type mpv_render_frame_info_flag = c_int;

/// Set if there is actually a next frame (of any kind, including redraws).
/// If unset, no other flags or fields requiring a queued frame are set.
pub const MPV_RENDER_FRAME_INFO_PRESENT: mpv_render_frame_info_flag = 1 << 0;
/// The frame is a redraw request rather than a new video frame (e.g. an
/// option changed while paused). Implies `PRESENT`.
pub const MPV_RENDER_FRAME_INFO_REDRAW: mpv_render_frame_info_flag = 1 << 1;
/// The frame is supposed to reproduce the previous frame perfectly (used by
/// `display-...` video-sync modes). Implies `PRESENT`.
pub const MPV_RENDER_FRAME_INFO_REPEAT: mpv_render_frame_info_flag = 1 << 2;
/// The player timing code expects the render thread to block on vsync
/// (by delaying the render call or calling
/// [`mpv_render_context_report_swap`] at vsync time). Implies `PRESENT`.
pub const MPV_RENDER_FRAME_INFO_BLOCK_VSYNC: mpv_render_frame_info_flag = 1 << 3;

/// Information about the next frame to be rendered; retrieved with
/// [`MPV_RENDER_PARAM_NEXT_FRAME_INFO`] via [`mpv_render_context_get_info`].
#[repr(C)]
#[derive(Clone, Copy)]
pub struct mpv_render_frame_info {
    /// A bitset of `MPV_RENDER_FRAME_INFO_*` flags.
    pub flags: u64,
    /// Absolute target display time, in the unit and base of
    /// `mpv_get_time_us`. Can be `0` for redraws or vsync-locked timing.
    pub target_time: i64,
}

/// Bit flags in the return value of [`mpv_render_context_update`].
pub type mpv_render_update_flag = c_int;

/// A new video frame must be rendered; call [`mpv_render_context_render`].
pub const MPV_RENDER_UPDATE_FRAME: mpv_render_update_flag = 1 << 0;

/// Alias kept for parity with the C header, which names the enum both
/// `mpv_render_update_flag` and `mpv_render_context_flag`.
pub type mpv_render_context_flag = mpv_render_update_flag;

/// Update callback type for [`mpv_render_context_set_update_callback`].
pub type mpv_render_update_fn = Option<unsafe extern "C" fn(cb_ctx: *mut c_void)>;

extern "C" {
    /// Initialize the renderer. `*res` is always overwritten (with the
    /// context on success, null on failure). `params` is a
    /// [`MPV_RENDER_PARAM_INVALID`]-terminated array which must contain at
    /// least [`MPV_RENDER_PARAM_API_TYPE`] plus any backend-specific init
    /// parameter. Errors include `MPV_ERROR_UNSUPPORTED` (GL version or
    /// missing extensions), `MPV_ERROR_NOT_IMPLEMENTED` (unknown/disabled API
    /// type), and `MPV_ERROR_INVALID_PARAMETER`.
    pub fn mpv_render_context_create(
        res: *mut *mut mpv_render_context,
        mpv: *mut mpv_handle,
        params: *mut mpv_render_param,
    ) -> c_int;

    /// Attempt to change a single parameter; support depends on the backend
    /// and parameter type.
    pub fn mpv_render_context_set_parameter(
        ctx: *mut mpv_render_context,
        param: mpv_render_param,
    ) -> c_int;

    /// Retrieve information from the render context into the variable that
    /// `param.data` points to. Only parameter types that explicitly document
    /// support for this work (e.g. [`MPV_RENDER_PARAM_NEXT_FRAME_INFO`]);
    /// others fail with `MPV_ERROR_NOT_IMPLEMENTED`.
    pub fn mpv_render_context_get_info(
        ctx: *mut mpv_render_context,
        param: mpv_render_param,
    ) -> c_int;

    /// Set the callback notifying that a new frame is available or a redraw
    /// is required. Like the wakeup callback, it may be invoked from foreign
    /// threads and must only notify — no mpv API calls, no GL access, no
    /// unwinding. Callable from any thread except an update callback; setting
    /// it raises an update callback immediately.
    pub fn mpv_render_context_set_update_callback(
        ctx: *mut mpv_render_context,
        callback: mpv_render_update_fn,
        callback_ctx: *mut c_void,
    );

    /// Call on the render thread after the update callback fired (never from
    /// the callback itself). Optional without
    /// [`MPV_RENDER_PARAM_ADVANCED_CONTROL`]; a hard requirement with it.
    /// Returns a bitset of `MPV_RENDER_UPDATE_*` flags; `0` or unknown flags
    /// mean nothing needs to be done.
    pub fn mpv_render_context_update(ctx: *mut mpv_render_context) -> u64;

    /// Render the current video frame to the target described by `params`
    /// (a [`MPV_RENDER_PARAM_INVALID`]-terminated array; e.g.
    /// [`MPV_RENDER_PARAM_OPENGL_FBO`] and possibly
    /// [`MPV_RENDER_PARAM_FLIP_Y`]). Pulls a frame from the internal queue,
    /// or redraws the previous one, and by default blocks until the frame's
    /// display time.
    pub fn mpv_render_context_render(
        ctx: *mut mpv_render_context,
        params: *mut mpv_render_param,
    ) -> c_int;

    /// Tell the renderer a frame was flipped at this time. Optional, improves
    /// timing — but once used, it must be used consistently. Ignored while no
    /// video is initialized.
    pub fn mpv_render_context_report_swap(ctx: *mut mpv_render_context);

    /// Destroy the renderer state (forcefully disabling video if still
    /// active). `ctx` is invalid afterwards; null is allowed and does
    /// nothing. Must happen before the mpv core is destroyed.
    pub fn mpv_render_context_free(ctx: *mut mpv_render_context);
}

// --- render_gl.h ---

/// OpenGL init parameters for [`MPV_RENDER_PARAM_OPENGL_INIT_PARAMS`].
///
/// mpv accesses OpenGL exclusively through the function pointers returned by
/// `get_proc_address` and does not load GL libraries itself. It expects the
/// GL state to be at standard defaults on entry and restores it likewise,
/// except for: viewport, scissor box, `glBlendFuncSeparate`, and clear color
/// state; it may overwrite the `glDebugMessageCallback` and always disables
/// `GL_DITHER` at init.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct mpv_opengl_init_params {
    /// Resolve an OpenGL function by name (like `glXGetProcAddressARB` /
    /// `wglGetProcAddress`). If the platform API doesn't return pointers for
    /// all standard functions, the callback has to compensate by looking them
    /// up itself.
    pub get_proc_address:
        Option<unsafe extern "C" fn(ctx: *mut c_void, name: *const c_char) -> *mut c_void>,
    /// Value passed as `ctx` to `get_proc_address`.
    pub get_proc_address_ctx: *mut c_void,
}

/// A GL framebuffer target for [`MPV_RENDER_PARAM_OPENGL_FBO`].
#[repr(C)]
#[derive(Clone, Copy)]
pub struct mpv_opengl_fbo {
    /// A complete, color-renderable FBO name from `glGenFramebuffers()`, or
    /// `0` for the OpenGL default framebuffer.
    pub fbo: c_int,
    /// Width of the framebuffer. Must always be set.
    pub w: c_int,
    /// Height of the framebuffer. Must always be set.
    pub h: c_int,
    /// Underlying texture internal format (e.g. `GL_RGBA8`), or `0` if
    /// unknown.
    pub internal_format: c_int,
}

/// For the deprecated [`MPV_RENDER_PARAM_DRM_DISPLAY`].
#[deprecated(note = "use mpv_opengl_drm_params_v2 with MPV_RENDER_PARAM_DRM_DISPLAY_V2")]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct mpv_opengl_drm_params {
    pub fd: c_int,
    pub crtc_id: c_int,
    pub connector_id: c_int,
    pub atomic_request_ptr: *mut *mut c_void,
    pub render_fd: c_int,
}

/// DRM draw surface dimensions for
/// [`MPV_RENDER_PARAM_DRM_DRAW_SURFACE_SIZE`].
#[repr(C)]
#[derive(Clone, Copy)]
pub struct mpv_opengl_drm_draw_surface_size {
    /// Size of the draw plane surface in pixels.
    pub width: c_int,
    /// Size of the draw plane surface in pixels.
    pub height: c_int,
}

/// DRM display handles for [`MPV_RENDER_PARAM_DRM_DISPLAY_V2`].
#[repr(C)]
#[derive(Clone, Copy)]
pub struct mpv_opengl_drm_params_v2 {
    /// DRM file descriptor; `-1` if invalid.
    pub fd: c_int,
    /// Currently used CRTC id.
    pub crtc_id: c_int,
    /// Currently used connector id.
    pub connector_id: c_int,
    /// Pointer to the `drmModeAtomicReq*` used for the render loop (usually
    /// changed every iteration). C type: `struct _drmModeAtomicReq **`.
    pub atomic_request_ptr: *mut *mut c_void,
    /// DRM render node, used for VAAPI interop; `-1` if invalid.
    pub render_fd: c_int,
}

/// Backwards-compatible alias for [`mpv_opengl_drm_draw_surface_size`]
/// (the C header spells this `mpv_opengl_drm_osd_size`).
pub type mpv_opengl_drm_osd_size = mpv_opengl_drm_draw_surface_size;
