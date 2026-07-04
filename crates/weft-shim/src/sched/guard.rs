//! Per-thread scheduler bookkeeping: the logical thread id and a reentrancy
//! guard.
//!
//! The reentrancy guard is what makes it safe to interpose `pthread_mutex_*`
//! at all. The scheduler's own internals use Rust `std::sync` primitives
//! (futex-based, so they never route through our interposed C symbols), but
//! any *libc* call the scheduler makes — `malloc`, `dlsym` inside
//! [`crate::real`] — could still reach an interposed `pthread_mutex_lock`.
//! While the guard is held, every hook takes its passthrough path, so
//! scheduler-internal work can never recurse back into scheduling.

use std::cell::Cell;

use super::Tid;

thread_local! {
    /// This thread's logical id, once it has registered with the scheduler.
    static CURRENT_TID: Cell<Option<Tid>> = const { Cell::new(None) };
    /// Depth of scheduler/real-resolution reentrancy on this thread.
    static IN_WEFT: Cell<u32> = const { Cell::new(0) };
}

/// This thread's logical id, or `None` if it has never registered (e.g. the
/// main thread before the first `pthread_create`, or a thread created outside
/// our interception).
pub fn current_tid() -> Option<Tid> {
    CURRENT_TID.with(Cell::get)
}

/// Record this thread's logical id (called once, at registration).
pub fn set_current_tid(tid: Tid) {
    CURRENT_TID.with(|c| c.set(Some(tid)));
}

/// Forget this thread's logical id once it has finished, so any later
/// teardown (TLS destructors, etc.) takes the passthrough path instead of
/// asking the scheduler to run an already-finished thread.
pub fn clear_current_tid() {
    CURRENT_TID.with(|c| c.set(None));
}

/// True while this thread is inside scheduler machinery; hooks must take their
/// passthrough path to avoid recursion.
#[must_use]
pub fn is_reentrant() -> bool {
    IN_WEFT.with(|c| c.get() > 0)
}

/// RAII marker: sets the reentrancy flag for its lifetime. Nestable.
pub struct Reentrancy(());

impl Reentrancy {
    #[must_use]
    pub fn enter() -> Self {
        IN_WEFT.with(|c| c.set(c.get() + 1));
        Self(())
    }
}

impl Drop for Reentrancy {
    fn drop(&mut self) {
        IN_WEFT.with(|c| c.set(c.get() - 1));
    }
}
