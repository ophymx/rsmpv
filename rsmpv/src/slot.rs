//! A shared callback cell whose address is handed to libmpv, used for
//! both the client wakeup callback and the render update callback.

use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use crate::lock_ignore_poison;

pub(crate) type Callback = Arc<dyn Fn() + Send + Sync>;
type Slot = Mutex<Option<Callback>>;

/// Owner of a callback slot whose address is registered with libmpv.
///
/// # Sharing and lifetime of the slot
/// The slot is an `Arc<Mutex<..>>`; the context pointer given to libmpv
/// points into that shared allocation, backed by one deliberately leaked
/// `Arc` reference (created on first registration). The leak buys two
/// guarantees for the price of one small allocation per registered
/// handle/render context:
///
/// - Moves of the owning struct are trivially fine: nothing here is a
///   `Box` whose moves would assert unique ownership of memory libmpv
///   threads are concurrently reading.
/// - The slot outlives everything, so even a callback dispatch that races
///   handle/render-context destruction (the vendored headers promise no
///   synchronization between destruction and an in-flight dispatch) finds
///   a valid slot — empty, if the owner cleared it — and no-ops. No
///   teardown-ordering argument is needed for memory safety.
///
/// Dropping the `CallbackSlot` only releases the owner's reference. The
/// wrapper `Drop` impls empty the slot first (via their
/// [`teardown`](CallbackSlot::teardown) paths); a closure left behind
/// would merely leak with the slot.
///
/// # Locking and lifetime of closures
/// [`trampoline`](CallbackSlot::trampoline) clones the stored `Arc` under
/// the slot lock and invokes the closure *outside* it. Consequences:
///
/// - Invocations may run concurrently (hence the `Sync` bound on stored
///   closures) and never hold a lock user code can observe.
/// - [`set`](CallbackSlot::set) / [`clear`](CallbackSlot::clear) never
///   wait for a running closure: a replaced or removed closure is simply
///   released, and its memory is freed when the last in-flight invocation
///   drops its clone.
/// - `set` and `clear` serialize against each other (slot update plus the
///   libmpv registration call) through a separate registration lock the
///   trampoline never takes, so the registered state and the stored
///   closure cannot diverge under concurrent set/clear.
pub(crate) struct CallbackSlot {
    slot: Arc<Slot>,
    /// Serializes `set`/`clear` against each other across their FFI
    /// registration call; never taken by the trampoline, so holding it
    /// across the FFI call cannot deadlock with a (possibly synchronous)
    /// callback dispatch. (A user callback calling set/clear from *inside*
    /// a dispatch would deadlock here — but that call is doc-forbidden and
    /// already deadlocks on libmpv's own dispatch lock, so this adds no
    /// new hazard.) The `bool` records whether the libmpv-side leaked
    /// reference exists yet.
    registration: Mutex<bool>,
}

impl CallbackSlot {
    pub(crate) fn new() -> CallbackSlot {
        CallbackSlot {
            slot: Arc::new(Mutex::new(None)),
            registration: Mutex::new(false),
        }
    }

    /// Store `callback` and register the trampoline with libmpv via
    /// `register`, which is called with the context pointer to pass
    /// alongside [`trampoline`](CallbackSlot::trampoline).
    ///
    /// Ordering: store first, then register, both under the registration
    /// lock — libmpv's registration functions may invoke the new callback
    /// synchronously on the calling thread, and that invocation must
    /// already see the new closure. The replaced closure (arbitrary user
    /// `Drop` code) is released after all locks are dropped.
    pub(crate) fn set(
        &self,
        callback: impl Fn() + Send + Sync + 'static,
        register: impl FnOnce(*mut c_void),
    ) {
        let previous;
        {
            let mut leaked = lock_ignore_poison(&self.registration);
            if !*leaked {
                // libmpv's reference: leaked so the slot allocation stays
                // valid forever (see the struct docs).
                std::mem::forget(Arc::clone(&self.slot));
                *leaked = true;
            }
            previous = lock_ignore_poison(&self.slot).replace(Arc::new(callback));
            register(Arc::as_ptr(&self.slot) as *mut c_void);
        }
        drop(previous);
    }

    /// Unregister with libmpv via `unregister`, then remove the stored
    /// closure, both under the registration lock (mirroring
    /// [`set`](CallbackSlot::set)'s order: no new dispatches can start
    /// once `unregister` returns, and dispatches before it still find the
    /// closure they were promised). An in-flight invocation keeps the
    /// closure alive until it returns, so this never waits for user code.
    ///
    /// The removed closure is handed back rather than dropped; see
    /// [`teardown`](CallbackSlot::teardown) for why the caller controls
    /// its release.
    #[must_use = "dropping the closure runs user Drop code; the caller chooses when"]
    pub(crate) fn clear(&self, unregister: impl FnOnce()) -> Option<Callback> {
        let _reg = lock_ignore_poison(&self.registration);
        unregister();
        lock_ignore_poison(&self.slot).take()
    }

    /// Owner teardown: [`clear`](CallbackSlot::clear) via `unregister`,
    /// run `destroy` (the owner's mpv destroy/terminate/free call — or a
    /// no-op when only clearing the callback), and release the removed
    /// closure last.
    ///
    /// This ordering is the crate's panic-safety story in one place:
    /// releasing the closure runs arbitrary user `Drop` code, and a panic
    /// there must not skip `destroy` — destruction of the mpv object is
    /// what makes it sound for the owner's remaining fields (e.g. `Mpv`'s
    /// protocol registry, `RenderInner`'s get_proc_address box) to drop
    /// after it, unwinding or not. Nothing here waits for an in-flight
    /// callback; `destroy` is what synchronizes with those.
    pub(crate) fn teardown(&self, unregister: impl FnOnce(), destroy: impl FnOnce()) {
        let callback = self.clear(unregister);
        destroy();
        drop(callback);
    }

    /// The C callback registered with libmpv; `ctx` is the pointer that
    /// [`set`](CallbackSlot::set) passed to its `register` closure. Clones
    /// the stored closure under the slot lock and invokes it outside;
    /// panics are caught and ignored (the libmpv callback contracts forbid
    /// unwinding out).
    pub(crate) unsafe extern "C" fn trampoline(ctx: *mut c_void) {
        // The pointee is kept alive forever by the leaked Arc reference,
        // so this is valid no matter how late libmpv calls.
        let slot = &*(ctx as *const Slot);
        let callback = lock_ignore_poison(slot).clone();
        if let Some(callback) = callback {
            // The clone must be consumed *inside* the guard: if a
            // concurrent set/clear released the slot's reference, this
            // clone is the last one, and dropping it runs the closure's
            // destructor (arbitrary user code) right here on mpv's
            // dispatch thread — a panic from that Drop must not unwind
            // across the FFI boundary any more than one from the call.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || callback()));
        }
    }
}
