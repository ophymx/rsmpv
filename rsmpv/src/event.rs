//! Owned representation of libmpv events.

use std::ffi::CStr;

use crate::error::{check, Error, Result};
use crate::node::Node;

/// The value carried by a property event, decoded according to the format it
/// was requested/observed with.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum PropertyData {
    /// No data (the property is unavailable, reading it failed, or it was
    /// observed with [`Format::None`](crate::Format::None)).
    None,
    /// Raw string value (lossily converted; see [`Node::String`]).
    String(String),
    /// OSD string value.
    OsdString(String),
    /// Boolean flag.
    Flag(bool),
    /// 64-bit integer.
    Int64(i64),
    /// Double.
    Double(f64),
    /// Structured value.
    Node(Node),
}

/// Why playback of a file ended (see [`Event::EndFile`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EndFileReason {
    /// The end of the file was reached (also possible for broken files or
    /// interrupted network streams, or a restricted playback range).
    Eof,
    /// Playback was stopped by an external action (e.g. playlist controls).
    Stop,
    /// Playback was stopped by the `quit` command or player shutdown.
    Quit,
    /// An error aborted playback; see the `error` field of
    /// [`Event::EndFile`].
    Error,
    /// The file was a playlist (or similar) whose entries replaced it.
    Redirect,
    /// A reason this crate doesn't know about.
    Unknown(i32),
}

impl EndFileReason {
    fn from_raw(raw: rsmpv_sys::mpv_end_file_reason) -> EndFileReason {
        match raw {
            rsmpv_sys::MPV_END_FILE_REASON_EOF => EndFileReason::Eof,
            rsmpv_sys::MPV_END_FILE_REASON_STOP => EndFileReason::Stop,
            rsmpv_sys::MPV_END_FILE_REASON_QUIT => EndFileReason::Quit,
            rsmpv_sys::MPV_END_FILE_REASON_ERROR => EndFileReason::Error,
            rsmpv_sys::MPV_END_FILE_REASON_REDIRECT => EndFileReason::Redirect,
            other => EndFileReason::Unknown(other),
        }
    }
}

/// A log message (see [`Event::LogMessage`] and
/// [`Handle::request_log_messages`](crate::Handle::request_log_messages)).
#[derive(Debug, Clone, PartialEq)]
pub struct LogMessage {
    /// Module prefix identifying the sender (the special value `"overflow"`
    /// indicates the message buffer overflowed).
    pub prefix: String,
    /// The log level as a string (never `"no"`).
    pub level: String,
    /// One line of text, terminated with a newline character.
    pub text: String,
    /// The numeric log level (a `MPV_LOG_LEVEL_*` value).
    pub log_level: i32,
}

/// An owned libmpv event, as returned by
/// [`Mpv::wait_event`](crate::Mpv::wait_event).
///
/// All data is deep-copied out of libmpv's event storage, so events can be
/// kept and sent across threads freely.
#[derive(Debug, Clone)]
#[non_exhaustive]
#[allow(missing_docs)] // self-describing fields (userdata, name, ids)
pub enum Event {
    /// The player is quitting; drop the handle (or the client) soon.
    Shutdown,
    /// A log message.
    LogMessage(LogMessage),
    /// Reply to [`Handle::get_property_async`](crate::Handle::get_property_async).
    GetPropertyReply {
        userdata: u64,
        /// The property name and decoded value on success.
        result: Result<(String, PropertyData)>,
    },
    /// Reply to [`Handle::set_property_async`](crate::Handle::set_property_async).
    SetPropertyReply { userdata: u64, result: Result<()> },
    /// Reply to [`Handle::command_async`](crate::Handle::command_async).
    CommandReply {
        userdata: u64,
        /// Command-specific return data on success ([`Node::None`] for most
        /// commands).
        result: Result<Node>,
    },
    /// Playback of a file is starting.
    StartFile { playlist_entry_id: i64 },
    /// Playback of a file ended.
    ///
    /// The playlist fields are `0` when the linked libmpv predates client
    /// API 1.108 (mpv 0.33), which lacks them.
    EndFile {
        reason: EndFileReason,
        /// Set when `reason` is [`EndFileReason::Error`].
        error: Option<Error>,
        playlist_entry_id: i64,
        playlist_insert_id: i64,
        playlist_insert_num_entries: i32,
    },
    /// The file has been loaded and decoding starts.
    FileLoaded,
    /// A `script-message` directed at this client.
    ClientMessage(Vec<String>),
    /// Video was reconfigured.
    VideoReconfig,
    /// Audio was reconfigured.
    AudioReconfig,
    /// A seek started.
    Seek,
    /// Playback was reinitialized after a discontinuity.
    PlaybackRestart,
    /// An observed property (may have) changed.
    PropertyChange {
        userdata: u64,
        name: String,
        data: PropertyData,
    },
    /// The event queue overflowed and at least one event was dropped.
    QueueOverflow,
    /// A hook was invoked; it must be continued with
    /// [`Handle::hook_continue`](crate::Handle::hook_continue), passing `id`.
    Hook {
        userdata: u64,
        name: String,
        id: u64,
    },
    /// An event this crate doesn't know about (including the events
    /// deprecated upstream, `MPV_EVENT_IDLE` = 11 and `MPV_EVENT_TICK` = 14).
    Unknown(i32),
}

/// Whether the linked libmpv has the fields added in client API 1.108
/// (mpv 0.33): the `mpv_event_end_file` playlist ids and non-null
/// `MPV_EVENT_START_FILE` data. Cached — the linked library's version
/// can't change at runtime.
fn has_api_1_108() -> bool {
    use std::sync::OnceLock;
    static HAS: OnceLock<bool> = OnceLock::new();
    *HAS.get_or_init(|| {
        let v = unsafe { rsmpv_sys::mpv_client_api_version() };
        v >= rsmpv_sys::MPV_MAKE_VERSION(1, 108)
    })
}

unsafe fn lossy(ptr: *const std::os::raw::c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
}

unsafe fn property_data(prop: &rsmpv_sys::mpv_event_property) -> PropertyData {
    if prop.data.is_null() {
        return PropertyData::None;
    }
    match prop.format {
        rsmpv_sys::MPV_FORMAT_STRING => {
            PropertyData::String(lossy(*(prop.data as *const *const std::os::raw::c_char)))
        }
        rsmpv_sys::MPV_FORMAT_OSD_STRING => {
            PropertyData::OsdString(lossy(*(prop.data as *const *const std::os::raw::c_char)))
        }
        rsmpv_sys::MPV_FORMAT_FLAG => {
            PropertyData::Flag(*(prop.data as *const std::os::raw::c_int) != 0)
        }
        rsmpv_sys::MPV_FORMAT_INT64 => PropertyData::Int64(*(prop.data as *const i64)),
        rsmpv_sys::MPV_FORMAT_DOUBLE => PropertyData::Double(*(prop.data as *const f64)),
        rsmpv_sys::MPV_FORMAT_NODE => {
            PropertyData::Node(Node::from_raw(prop.data as *const rsmpv_sys::mpv_node))
        }
        _ => PropertyData::None,
    }
}

impl Event {
    /// Deep-copy a raw event into an owned [`Event`]. Returns `None` for
    /// `MPV_EVENT_NONE`.
    ///
    /// # Safety
    /// `raw` must point to a valid event as returned by `mpv_wait_event`,
    /// and must not be used concurrently with another `mpv_wait_event` call
    /// on the same handle.
    pub(crate) unsafe fn from_raw(raw: *const rsmpv_sys::mpv_event) -> Option<Event> {
        let ev = &*raw;
        Some(match ev.event_id {
            rsmpv_sys::MPV_EVENT_NONE => return None,
            rsmpv_sys::MPV_EVENT_SHUTDOWN => Event::Shutdown,
            rsmpv_sys::MPV_EVENT_LOG_MESSAGE => {
                let msg = &*(ev.data as *const rsmpv_sys::mpv_event_log_message);
                Event::LogMessage(LogMessage {
                    prefix: lossy(msg.prefix),
                    level: lossy(msg.level),
                    text: lossy(msg.text),
                    log_level: msg.log_level,
                })
            }
            rsmpv_sys::MPV_EVENT_GET_PROPERTY_REPLY => Event::GetPropertyReply {
                userdata: ev.reply_userdata,
                result: check(ev.error).map(|_| {
                    let prop = &*(ev.data as *const rsmpv_sys::mpv_event_property);
                    (lossy(prop.name), property_data(prop))
                }),
            },
            rsmpv_sys::MPV_EVENT_SET_PROPERTY_REPLY => Event::SetPropertyReply {
                userdata: ev.reply_userdata,
                result: check(ev.error).map(|_| ()),
            },
            rsmpv_sys::MPV_EVENT_COMMAND_REPLY => Event::CommandReply {
                userdata: ev.reply_userdata,
                result: check(ev.error).map(|_| {
                    let cmd = &*(ev.data as *const rsmpv_sys::mpv_event_command);
                    Node::from_raw(&cmd.result)
                }),
            },
            rsmpv_sys::MPV_EVENT_START_FILE => Event::StartFile {
                // Null before libmpv client API 1.108.
                playlist_entry_id: (ev.data as *const rsmpv_sys::mpv_event_start_file)
                    .as_ref()
                    .map_or(0, |sf| sf.playlist_entry_id),
            },
            rsmpv_sys::MPV_EVENT_END_FILE => {
                // Before client API 1.108 `mpv_event_end_file` ends at
                // `error`: read the playlist fields only when the linked
                // libmpv has them, via raw pointer field access so no
                // reference to the (possibly smaller) full struct is ever
                // formed.
                let ef = ev.data as *const rsmpv_sys::mpv_event_end_file;
                let v108 = has_api_1_108();
                Event::EndFile {
                    reason: EndFileReason::from_raw((*ef).reason),
                    error: ((*ef).error < 0).then(|| Error::from_raw((*ef).error)),
                    playlist_entry_id: if v108 { (*ef).playlist_entry_id } else { 0 },
                    playlist_insert_id: if v108 { (*ef).playlist_insert_id } else { 0 },
                    playlist_insert_num_entries: if v108 {
                        (*ef).playlist_insert_num_entries
                    } else {
                        0
                    },
                }
            }
            rsmpv_sys::MPV_EVENT_FILE_LOADED => Event::FileLoaded,
            rsmpv_sys::MPV_EVENT_CLIENT_MESSAGE => {
                let cm = &*(ev.data as *const rsmpv_sys::mpv_event_client_message);
                let mut args = Vec::with_capacity(cm.num_args.max(0) as usize);
                for i in 0..cm.num_args.max(0) as usize {
                    args.push(lossy(*cm.args.add(i)));
                }
                Event::ClientMessage(args)
            }
            rsmpv_sys::MPV_EVENT_VIDEO_RECONFIG => Event::VideoReconfig,
            rsmpv_sys::MPV_EVENT_AUDIO_RECONFIG => Event::AudioReconfig,
            rsmpv_sys::MPV_EVENT_SEEK => Event::Seek,
            rsmpv_sys::MPV_EVENT_PLAYBACK_RESTART => Event::PlaybackRestart,
            rsmpv_sys::MPV_EVENT_PROPERTY_CHANGE => {
                let prop = &*(ev.data as *const rsmpv_sys::mpv_event_property);
                Event::PropertyChange {
                    userdata: ev.reply_userdata,
                    name: lossy(prop.name),
                    data: property_data(prop),
                }
            }
            rsmpv_sys::MPV_EVENT_QUEUE_OVERFLOW => Event::QueueOverflow,
            rsmpv_sys::MPV_EVENT_HOOK => {
                let hook = &*(ev.data as *const rsmpv_sys::mpv_event_hook);
                Event::Hook {
                    userdata: ev.reply_userdata,
                    name: lossy(hook.name),
                    id: hook.id,
                }
            }
            other => Event::Unknown(other),
        })
    }
}
