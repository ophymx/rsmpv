//! Struct layout and linkage checks.
//!
//! The expected sizes/offsets were produced by compiling the equivalent
//! `sizeof`/`offsetof` program against the real mpv headers on a 64-bit
//! target, so these tests pin the hand-written translation to the C ABI.

#![cfg(target_pointer_width = "64")]

use core::mem::{offset_of, size_of};
use rsmpv_sys::*;

#[test]
fn client_struct_layouts() {
    assert_eq!(size_of::<mpv_node>(), 16);
    assert_eq!(offset_of!(mpv_node, u), 0);
    assert_eq!(offset_of!(mpv_node, format), 8);

    assert_eq!(size_of::<mpv_node_list>(), 24);
    assert_eq!(offset_of!(mpv_node_list, num), 0);
    assert_eq!(offset_of!(mpv_node_list, values), 8);
    assert_eq!(offset_of!(mpv_node_list, keys), 16);

    assert_eq!(size_of::<mpv_byte_array>(), 16);

    assert_eq!(size_of::<mpv_event_property>(), 24);
    assert_eq!(offset_of!(mpv_event_property, format), 8);
    assert_eq!(offset_of!(mpv_event_property, data), 16);

    assert_eq!(size_of::<mpv_event_log_message>(), 32);
    assert_eq!(offset_of!(mpv_event_log_message, log_level), 24);

    assert_eq!(size_of::<mpv_event_start_file>(), 8);

    assert_eq!(size_of::<mpv_event_end_file>(), 32);
    assert_eq!(offset_of!(mpv_event_end_file, error), 4);
    assert_eq!(offset_of!(mpv_event_end_file, playlist_entry_id), 8);
    assert_eq!(offset_of!(mpv_event_end_file, playlist_insert_id), 16);
    assert_eq!(
        offset_of!(mpv_event_end_file, playlist_insert_num_entries),
        24
    );

    assert_eq!(size_of::<mpv_event_client_message>(), 16);
    assert_eq!(offset_of!(mpv_event_client_message, args), 8);

    assert_eq!(size_of::<mpv_event_hook>(), 16);
    assert_eq!(offset_of!(mpv_event_hook, id), 8);

    assert_eq!(size_of::<mpv_event_command>(), 16);

    assert_eq!(size_of::<mpv_event>(), 24);
    assert_eq!(offset_of!(mpv_event, error), 4);
    assert_eq!(offset_of!(mpv_event, reply_userdata), 8);
    assert_eq!(offset_of!(mpv_event, data), 16);
}

#[cfg(feature = "render")]
#[test]
fn render_struct_layouts() {
    assert_eq!(size_of::<mpv_render_param>(), 16);
    assert_eq!(offset_of!(mpv_render_param, data), 8);

    assert_eq!(size_of::<mpv_render_frame_info>(), 16);
    assert_eq!(offset_of!(mpv_render_frame_info, target_time), 8);

    assert_eq!(size_of::<mpv_opengl_init_params>(), 16);
    assert_eq!(offset_of!(mpv_opengl_init_params, get_proc_address_ctx), 8);

    assert_eq!(size_of::<mpv_opengl_fbo>(), 16);
    assert_eq!(offset_of!(mpv_opengl_fbo, internal_format), 12);

    #[allow(deprecated)]
    {
        assert_eq!(size_of::<mpv_opengl_drm_params>(), 32);
        assert_eq!(offset_of!(mpv_opengl_drm_params, atomic_request_ptr), 16);
        assert_eq!(offset_of!(mpv_opengl_drm_params, render_fd), 24);
    }

    assert_eq!(size_of::<mpv_opengl_drm_draw_surface_size>(), 8);

    assert_eq!(size_of::<mpv_opengl_drm_params_v2>(), 32);
    assert_eq!(offset_of!(mpv_opengl_drm_params_v2, atomic_request_ptr), 16);
    assert_eq!(offset_of!(mpv_opengl_drm_params_v2, render_fd), 24);
}

#[cfg(feature = "stream-cb")]
#[test]
fn stream_cb_struct_layouts() {
    assert_eq!(size_of::<mpv_stream_cb_info>(), 48);
    assert_eq!(offset_of!(mpv_stream_cb_info, read_fn), 8);
    assert_eq!(offset_of!(mpv_stream_cb_info, close_fn), 32);
    assert_eq!(offset_of!(mpv_stream_cb_info, cancel_fn), 40);
}

#[test]
fn links_and_reports_a_sane_api_version() {
    let version = unsafe { mpv_client_api_version() };
    // Major version 1 or 2; these bindings target 2.x but link against 1.x too.
    assert!(
        (1..=2).contains(&(version >> 16)),
        "unexpected api version {version:#x}"
    );
}

#[test]
fn error_strings_are_static() {
    let s = unsafe { mpv_error_string(MPV_ERROR_NOMEM) };
    assert!(!s.is_null());
}
