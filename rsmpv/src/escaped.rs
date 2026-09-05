//! An owned heap allocation whose address has escaped to libmpv.

/// A `Box<T>` held as its raw pointer because the address was handed to
/// libmpv, which may call through it concurrently with moves of the owning
/// struct. A live `Box` field would assert unique ownership of the pointee
/// on every such move (aliasing UB under the Box `noalias` rules); the raw
/// pointer carries no such assertion.
///
/// Dropping this frees the allocation, so the owner must drop it only once
/// libmpv can no longer use the pointer (the owners order it after
/// destroying the mpv object the pointer was registered with). To hand the
/// allocation to libmpv permanently instead, `std::mem::forget` it.
pub(crate) struct EscapedBox<T: ?Sized> {
    ptr: *mut T,
}

// SAFETY: semantically this owns the pointee exactly like the Box it was
// made from, minus the noalias assertion, so it inherits Box's Send/Sync
// conditions. The pointer is never dereferenced through this type; it is
// only produced (as_ptr) and freed (Drop).
unsafe impl<T: ?Sized + Send> Send for EscapedBox<T> {}
unsafe impl<T: ?Sized + Sync> Sync for EscapedBox<T> {}

impl<T: ?Sized> EscapedBox<T> {
    pub(crate) fn new(value: Box<T>) -> EscapedBox<T> {
        EscapedBox {
            ptr: Box::into_raw(value),
        }
    }

    /// The escaped pointer, valid until `self` drops.
    pub(crate) fn as_ptr(&self) -> *mut T {
        self.ptr
    }
}

impl<T: ?Sized> Drop for EscapedBox<T> {
    fn drop(&mut self) {
        // SAFETY: ptr came from Box::into_raw and is freed exactly once,
        // here; the owner orders this after libmpv's last possible use.
        unsafe { drop(Box::from_raw(self.ptr)) }
    }
}
