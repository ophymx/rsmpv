//! Owned representation of `mpv_node` values.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};

use crate::error::{Error, Result};

/// An owned, structured mpv value (the safe counterpart of `mpv_node`).
///
/// Used for structured property access, structured commands, and command
/// results.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Node {
    /// Empty value.
    #[default]
    None,
    /// A boolean flag.
    Flag(bool),
    /// A 64-bit integer.
    Int64(i64),
    /// A double.
    Double(f64),
    /// A string. mpv strings are not guaranteed to be UTF-8 (filenames,
    /// tags); values read from mpv are converted lossily.
    String(String),
    /// An array of values.
    Array(Vec<Node>),
    /// A map. Key order is not meaningful (mpv returns keys in random
    /// order); duplicate keys are not allowed.
    Map(Vec<(String, Node)>),
    /// A raw byte array.
    ByteArray(Vec<u8>),
}

impl Node {
    /// Deep-copy a raw `mpv_node` written by libmpv into an owned `Node`.
    ///
    /// # Safety
    /// `raw` must point to a valid, fully initialized `mpv_node` (typically
    /// one written by libmpv).
    pub(crate) unsafe fn from_raw(raw: *const rsmpv_sys::mpv_node) -> Node {
        let node = &*raw;
        match node.format {
            rsmpv_sys::MPV_FORMAT_FLAG => Node::Flag(node.u.flag != 0),
            rsmpv_sys::MPV_FORMAT_INT64 => Node::Int64(node.u.int64),
            rsmpv_sys::MPV_FORMAT_DOUBLE => Node::Double(node.u.double_),
            rsmpv_sys::MPV_FORMAT_STRING => {
                Node::String(CStr::from_ptr(node.u.string).to_string_lossy().into_owned())
            }
            rsmpv_sys::MPV_FORMAT_NODE_ARRAY => {
                let list = &*node.u.list;
                let mut items = Vec::with_capacity(list.num.max(0) as usize);
                for i in 0..list.num.max(0) as usize {
                    items.push(Node::from_raw(list.values.add(i)));
                }
                Node::Array(items)
            }
            rsmpv_sys::MPV_FORMAT_NODE_MAP => {
                let list = &*node.u.list;
                let mut entries = Vec::with_capacity(list.num.max(0) as usize);
                for i in 0..list.num.max(0) as usize {
                    let key = CStr::from_ptr(*list.keys.add(i))
                        .to_string_lossy()
                        .into_owned();
                    entries.push((key, Node::from_raw(list.values.add(i))));
                }
                Node::Map(entries)
            }
            rsmpv_sys::MPV_FORMAT_BYTE_ARRAY => {
                let ba = &*node.u.ba;
                let bytes = if ba.size == 0 {
                    Vec::new()
                } else {
                    std::slice::from_raw_parts(ba.data as *const u8, ba.size).to_vec()
                };
                Node::ByteArray(bytes)
            }
            // MPV_FORMAT_NONE and anything unknown.
            _ => Node::None,
        }
    }
}

/// Backing storage for a raw `mpv_node` tree built from a [`Node`].
///
/// libmpv never writes to nodes the client passes in, so the raw tree only
/// has to stay valid for the duration of the API call; the storage owns
/// every allocation the raw pointers refer to.
#[derive(Default)]
#[allow(clippy::vec_box)] // the raw tree points into the boxed values, whose
                          // addresses must survive the Vecs reallocating
pub(crate) struct NodeStorage {
    strings: Vec<CString>,
    lists: Vec<Box<rsmpv_sys::mpv_node_list>>,
    node_arrays: Vec<Box<[rsmpv_sys::mpv_node]>>,
    key_arrays: Vec<Box<[*mut c_char]>>,
    byte_arrays: Vec<Box<rsmpv_sys::mpv_byte_array>>,
    bytes: Vec<Box<[u8]>>,
}

impl NodeStorage {
    fn cstring(&mut self, s: &str) -> Result<*mut c_char> {
        let c = CString::new(s).map_err(|_| Error::InteriorNul)?;
        let ptr = c.as_ptr() as *mut c_char;
        self.strings.push(c);
        Ok(ptr)
    }

    /// Build the raw representation of `node`, borrowing storage from `self`.
    pub(crate) fn build(&mut self, node: &Node) -> Result<rsmpv_sys::mpv_node> {
        let raw = match node {
            Node::None => rsmpv_sys::mpv_node {
                u: rsmpv_sys::mpv_node_u { int64: 0 },
                format: rsmpv_sys::MPV_FORMAT_NONE,
            },
            Node::Flag(v) => rsmpv_sys::mpv_node {
                u: rsmpv_sys::mpv_node_u { flag: *v as c_int },
                format: rsmpv_sys::MPV_FORMAT_FLAG,
            },
            Node::Int64(v) => rsmpv_sys::mpv_node {
                u: rsmpv_sys::mpv_node_u { int64: *v },
                format: rsmpv_sys::MPV_FORMAT_INT64,
            },
            Node::Double(v) => rsmpv_sys::mpv_node {
                u: rsmpv_sys::mpv_node_u { double_: *v },
                format: rsmpv_sys::MPV_FORMAT_DOUBLE,
            },
            Node::String(s) => rsmpv_sys::mpv_node {
                u: rsmpv_sys::mpv_node_u {
                    string: self.cstring(s)?,
                },
                format: rsmpv_sys::MPV_FORMAT_STRING,
            },
            Node::Array(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.build(item)?);
                }
                let values: Box<[rsmpv_sys::mpv_node]> = values.into_boxed_slice();
                let values_ptr = values.as_ptr() as *mut rsmpv_sys::mpv_node;
                self.node_arrays.push(values);
                let list = Box::new(rsmpv_sys::mpv_node_list {
                    num: items.len() as c_int,
                    values: values_ptr,
                    keys: std::ptr::null_mut(),
                });
                let list_ptr = &*list as *const _ as *mut rsmpv_sys::mpv_node_list;
                self.lists.push(list);
                rsmpv_sys::mpv_node {
                    u: rsmpv_sys::mpv_node_u { list: list_ptr },
                    format: rsmpv_sys::MPV_FORMAT_NODE_ARRAY,
                }
            }
            Node::Map(entries) => {
                let mut values = Vec::with_capacity(entries.len());
                let mut keys = Vec::with_capacity(entries.len());
                for (key, value) in entries {
                    keys.push(self.cstring(key)?);
                    values.push(self.build(value)?);
                }
                let values: Box<[rsmpv_sys::mpv_node]> = values.into_boxed_slice();
                let values_ptr = values.as_ptr() as *mut rsmpv_sys::mpv_node;
                self.node_arrays.push(values);
                let keys: Box<[*mut c_char]> = keys.into_boxed_slice();
                let keys_ptr = keys.as_ptr() as *mut *mut c_char;
                self.key_arrays.push(keys);
                let list = Box::new(rsmpv_sys::mpv_node_list {
                    num: entries.len() as c_int,
                    values: values_ptr,
                    keys: keys_ptr,
                });
                let list_ptr = &*list as *const _ as *mut rsmpv_sys::mpv_node_list;
                self.lists.push(list);
                rsmpv_sys::mpv_node {
                    u: rsmpv_sys::mpv_node_u { list: list_ptr },
                    format: rsmpv_sys::MPV_FORMAT_NODE_MAP,
                }
            }
            Node::ByteArray(data) => {
                let data: Box<[u8]> = data.clone().into_boxed_slice();
                let data_ptr = data.as_ptr() as *mut std::os::raw::c_void;
                let size = data.len();
                self.bytes.push(data);
                let ba = Box::new(rsmpv_sys::mpv_byte_array {
                    data: data_ptr,
                    size,
                });
                let ba_ptr = &*ba as *const _ as *mut rsmpv_sys::mpv_byte_array;
                self.byte_arrays.push(ba);
                rsmpv_sys::mpv_node {
                    u: rsmpv_sys::mpv_node_u { ba: ba_ptr },
                    format: rsmpv_sys::MPV_FORMAT_BYTE_ARRAY,
                }
            }
        };
        Ok(raw)
    }
}

impl From<bool> for Node {
    fn from(v: bool) -> Node {
        Node::Flag(v)
    }
}
impl From<i64> for Node {
    fn from(v: i64) -> Node {
        Node::Int64(v)
    }
}
impl From<f64> for Node {
    fn from(v: f64) -> Node {
        Node::Double(v)
    }
}
impl From<&str> for Node {
    fn from(v: &str) -> Node {
        Node::String(v.to_owned())
    }
}
impl From<String> for Node {
    fn from(v: String) -> Node {
        Node::String(v)
    }
}
impl<T: Into<Node>> From<Vec<T>> for Node {
    fn from(v: Vec<T>) -> Node {
        Node::Array(v.into_iter().map(Into::into).collect())
    }
}
