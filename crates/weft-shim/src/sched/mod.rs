//! The deterministic cooperative scheduler.
//!
//! # Model
//!
//! Real OS threads still exist, but exactly **one logical thread runs at a
//! time**. A thread may only make forward progress while it holds the token
//! (`running == its tid`). At every *yield point* — an interposed
//! synchronization operation (see [`crate::hooks::thread`]) — the running
//! thread hands the token back to the scheduler, which deterministically
//! selects the next thread to run from a seed-derived ChaCha8 stream and
//! wakes it; all other threads park.
//!
//! The scheduler is a *model* of the synchronization state: it tracks mutex
//! ownership, condition-variable waiters, and join dependencies, and only
//! ever schedules a thread that can actually proceed (an *enabled* thread).
//! Because execution is serialized and every hand-off crosses this module's
//! `std::sync` primitives (full barriers), the modeled operations fully
//! *replace* the target's real pthread primitives — we never call the real
//! `pthread_mutex_lock`/`unlock`/`cond_*`, so there is no way for a parked
//! thread to sit blocked in the kernel where the scheduler can't see it.
//!
//! # Why this can't preempt arbitrary instructions
//!
//! This is userspace-only: yield points exist *only* where the target calls
//! an interposed libc function. A pure-compute region between two yield
//! points runs atomically. See `docs/scheduling-model.md` for the precise
//! limitation and what it means for which races are reachable.
//!
//! # Reentrancy
//!
//! The scheduler's own state uses Rust `std::sync` (futex-based, never routed
//! through our interposed C symbols). Any libc call it makes internally
//! (`malloc`, `dlsym`) is fenced by [`guard::Reentrancy`], so hooks entered
//! from within the scheduler take their passthrough path.

// Nearly every method locks the state mutex; the only possible panic anywhere
// is lock poisoning, which cannot happen (no panicking operation runs inside
// any critical section). Documenting that non-condition on every method adds
// noise, not safety — it is stated once here.
#![allow(clippy::missing_panics_doc)]

pub mod guard;

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::{Condvar, Mutex, MutexGuard};

use rand_chacha::ChaCha8Rng;
use rand_core::RngCore;
use weft_abi::Strategy;

pub use guard::{current_tid, is_reentrant, Reentrancy};

/// A trivial FNV-1a hasher for the scheduler's own collections.
///
/// The default `HashMap` seeds its SipHash keys from `getrandom`, which we
/// interpose — so building one inside `init()` would re-enter `shim()` and
/// deadlock the `OnceLock`. A deterministic hasher sidesteps that entirely
/// (and, as a bonus, makes map iteration order reproducible — though every
/// selection here also sorts, so correctness never depends on it).
#[derive(Default)]
struct FnvHasher(u64);

impl Hasher for FnvHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        let mut h = if self.0 == 0 {
            0xcbf2_9ce4_8422_2325
        } else {
            self.0
        };
        for &b in bytes {
            h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.0 = h;
    }
    fn write_u64(&mut self, i: u64) {
        self.write(&i.to_ne_bytes());
    }
    fn write_usize(&mut self, i: usize) {
        self.write(&i.to_ne_bytes());
    }
}

type FnvBuild = BuildHasherDefault<FnvHasher>;
type Map<K, V> = HashMap<K, V, FnvBuild>;
type Set<T> = HashSet<T, FnvBuild>;

/// Logical thread id, assigned by the scheduler (distinct from the OS tid).
pub type Tid = u64;

/// Identity of a pthread object (its address), used to key ownership/wait
/// tables. Two different `pthread_mutex_t` objects have distinct addresses.
pub type Key = usize;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Status {
    /// Able to run; eligible for selection.
    Runnable,
    /// Waiting to acquire a held mutex.
    BlockedMutex(Key),
    /// In `pthread_cond_wait`, not yet signaled.
    BlockedCond(Key),
    /// In `pthread_join`, waiting for the target thread to finish.
    BlockedJoin(Tid),
    /// Returned from its start routine (or called `pthread_exit`).
    Finished,
}

struct State {
    /// Whose turn it is, or `None` once every thread has finished.
    running: Option<Tid>,
    status: Map<Tid, Status>,
    mutex_owner: Map<Key, Tid>,
    /// OS `pthread_t` (as `usize`) → our [`Tid`], for `pthread_join`.
    handles: Map<usize, Tid>,
    rng: ChaCha8Rng,
    strategy: Strategy,
    rr_cursor: Tid,
    next_tid: Tid,
    // coverage / stats
    decisions: u64,
    sites: Set<&'static str>,
    max_threads: usize,
}

/// The process-global deterministic scheduler.
pub struct Scheduler {
    state: Mutex<State>,
    turn: Condvar,
    stats: bool,
}

impl Scheduler {
    #[must_use]
    pub fn new(seed: u64, strategy: Strategy, stats: bool) -> Self {
        Self {
            state: Mutex::new(State {
                running: None,
                status: Map::default(),
                mutex_owner: Map::default(),
                handles: Map::default(),
                rng: crate::rng::Domains::scheduler_stream(seed),
                strategy,
                rr_cursor: 0,
                next_tid: 0,
                decisions: 0,
                sites: Set::default(),
                max_threads: 0,
            }),
            turn: Condvar::new(),
            stats,
        }
    }

    fn fresh_tid(st: &mut State) -> Tid {
        let t = st.next_tid;
        st.next_tid += 1;
        st.max_threads = st.max_threads.max(st.status.len() + 1);
        t
    }

    /// Register the calling thread (the main thread) on the first
    /// `pthread_create`, making it the initial token holder. Idempotent.
    pub fn ensure_main_registered(&self) -> Tid {
        if let Some(t) = current_tid() {
            return t;
        }
        let _g = Reentrancy::enter();
        let mut st = self.state.lock().unwrap();
        let tid = Self::fresh_tid(&mut st);
        st.status.insert(tid, Status::Runnable);
        if st.running.is_none() {
            st.running = Some(tid);
        }
        drop(st);
        guard::set_current_tid(tid);
        tid
    }

    /// Reserve a [`Tid`] for a child before it starts. The child adopts it in
    /// [`Self::child_started`]; the parent maps its `pthread_t` handle to it.
    pub fn register_child(&self) -> Tid {
        let _g = Reentrancy::enter();
        let mut st = self.state.lock().unwrap();
        let tid = Self::fresh_tid(&mut st);
        st.status.insert(tid, Status::Runnable);
        tid
    }

    /// Retire a child tid that was reserved but never started (its
    /// `pthread_create` failed): mark it finished so it can't block a join.
    pub fn abandon_child(&self, tid: Tid) {
        let _g = Reentrancy::enter();
        self.state
            .lock()
            .unwrap()
            .status
            .insert(tid, Status::Finished);
    }

    /// Record the OS handle (`pthread_t`) for a reserved child tid so
    /// `pthread_join` can find it.
    pub fn record_handle(&self, handle: usize, tid: Tid) {
        let _g = Reentrancy::enter();
        self.state.lock().unwrap().handles.insert(handle, tid);
    }

    /// Resolve an OS handle back to a logical tid.
    #[must_use]
    pub fn tid_for_handle(&self, handle: usize) -> Option<Tid> {
        let _g = Reentrancy::enter();
        self.state.lock().unwrap().handles.get(&handle).copied()
    }

    /// A freshly-started child adopts its reserved tid and parks until the
    /// scheduler grants it the token.
    pub fn child_started(&self, tid: Tid) {
        guard::set_current_tid(tid);
        let _g = Reentrancy::enter();
        let st = self.state.lock().unwrap();
        let _st = self.wait_turn(tid, st);
    }

    /// Mark the current thread finished, wake any joiners, and hand the token
    /// on. Does not park — the OS thread returns and exits.
    pub fn thread_finished(&self, tid: Tid) {
        let _g = Reentrancy::enter();
        let mut st = self.state.lock().unwrap();
        st.status.insert(tid, Status::Finished);
        for s in st.status.values_mut() {
            if *s == Status::BlockedJoin(tid) {
                *s = Status::Runnable;
            }
        }
        self.pick_next("thread_finished", &mut st);
        drop(st);
        // From here on this thread must not be treated as schedulable.
        guard::clear_current_tid();
    }

    /// Yield point for `pthread_mutex_lock`. Returns once the caller owns the
    /// mutex in the model and holds the token.
    pub fn mutex_lock(&self, key: Key) {
        let me = current_tid().expect("caller registered");
        let _g = Reentrancy::enter();
        let mut st = self.state.lock().unwrap();
        loop {
            if let Entry::Vacant(e) = st.mutex_owner.entry(key) {
                e.insert(me);
                self.pick_next("mutex_lock", &mut st);
                let _held = self.wait_turn(me, st);
                return;
            }
            st.status.insert(me, Status::BlockedMutex(key));
            self.pick_next("mutex_lock_contended", &mut st);
            st = self.wait_turn(me, st);
            // Woken because the lock was released; loop to re-check (another
            // woken waiter may have taken it first).
        }
    }

    /// Yield point for `pthread_mutex_trylock`. Returns `true` if acquired.
    pub fn mutex_trylock(&self, key: Key) -> bool {
        let me = current_tid().expect("caller registered");
        let _g = Reentrancy::enter();
        let mut st = self.state.lock().unwrap();
        let acquired = if let Entry::Vacant(e) = st.mutex_owner.entry(key) {
            e.insert(me);
            true
        } else {
            false
        };
        self.pick_next("mutex_trylock", &mut st);
        let _st = self.wait_turn(me, st);
        acquired
    }

    /// Yield point for `pthread_mutex_unlock`.
    pub fn mutex_unlock(&self, key: Key) {
        let me = current_tid().expect("caller registered");
        let _g = Reentrancy::enter();
        let mut st = self.state.lock().unwrap();
        st.mutex_owner.remove(&key);
        for s in st.status.values_mut() {
            if *s == Status::BlockedMutex(key) {
                *s = Status::Runnable;
            }
        }
        self.pick_next("mutex_unlock", &mut st);
        let _st = self.wait_turn(me, st);
    }

    /// Yield point for `pthread_cond_wait`: atomically release `mutex`, block
    /// until signaled, then reacquire `mutex` before returning.
    pub fn cond_wait(&self, cond: Key, mutex: Key) {
        let me = current_tid().expect("caller registered");
        let _g = Reentrancy::enter();
        let mut st = self.state.lock().unwrap();
        // Release the associated mutex and wake its waiters.
        st.mutex_owner.remove(&mutex);
        for s in st.status.values_mut() {
            if *s == Status::BlockedMutex(mutex) {
                *s = Status::Runnable;
            }
        }
        // Block on the condition variable until a signal makes us Runnable.
        st.status.insert(me, Status::BlockedCond(cond));
        self.pick_next("cond_wait", &mut st);
        st = self.wait_turn(me, st);
        // Reacquire the mutex.
        loop {
            if let Entry::Vacant(e) = st.mutex_owner.entry(mutex) {
                e.insert(me);
                self.pick_next("cond_reacquire", &mut st);
                let _held = self.wait_turn(me, st);
                return;
            }
            st.status.insert(me, Status::BlockedMutex(mutex));
            self.pick_next("cond_reacquire_contended", &mut st);
            st = self.wait_turn(me, st);
        }
    }

    /// Yield point for `pthread_cond_signal`: wake one waiter (chosen
    /// deterministically), then hand off.
    pub fn cond_signal(&self, cond: Key) {
        let me = current_tid().expect("caller registered");
        let _g = Reentrancy::enter();
        let mut st = self.state.lock().unwrap();
        let mut waiters: Vec<Tid> = st
            .status
            .iter()
            .filter(|(_, s)| **s == Status::BlockedCond(cond))
            .map(|(t, _)| *t)
            .collect();
        if !waiters.is_empty() {
            waiters.sort_unstable();
            #[allow(clippy::cast_possible_truncation)] // index into a Vec
            let idx = (st.rng.next_u64() % waiters.len() as u64) as usize;
            let chosen = waiters[idx];
            st.status.insert(chosen, Status::Runnable);
        }
        self.pick_next("cond_signal", &mut st);
        let _st = self.wait_turn(me, st);
    }

    /// Yield point for `pthread_cond_broadcast`: wake all waiters.
    pub fn cond_broadcast(&self, cond: Key) {
        let me = current_tid().expect("caller registered");
        let _g = Reentrancy::enter();
        let mut st = self.state.lock().unwrap();
        for s in st.status.values_mut() {
            if *s == Status::BlockedCond(cond) {
                *s = Status::Runnable;
            }
        }
        self.pick_next("cond_broadcast", &mut st);
        let _st = self.wait_turn(me, st);
    }

    /// Yield point for `pthread_join`: block until `target` finishes.
    pub fn join(&self, target: Tid) {
        let me = current_tid().expect("caller registered");
        let _g = Reentrancy::enter();
        let mut st = self.state.lock().unwrap();
        if matches!(st.status.get(&target), Some(s) if *s != Status::Finished) {
            st.status.insert(me, Status::BlockedJoin(target));
            self.pick_next("join", &mut st);
            st = self.wait_turn(me, st);
        }
        drop(st);
    }

    /// Voluntary yield point (`sched_yield`, and the tail of `pthread_create`).
    pub fn yield_now(&self, site: &'static str) {
        let me = current_tid().expect("caller registered");
        let _g = Reentrancy::enter();
        let mut st = self.state.lock().unwrap();
        self.pick_next(site, &mut st);
        let _st = self.wait_turn(me, st);
    }

    /// Choose the next runnable thread and grant it the token. Detects and
    /// reports deadlock (all threads blocked, none can proceed).
    fn pick_next(&self, site: &'static str, st: &mut State) {
        st.decisions += 1;
        st.sites.insert(site);

        let mut runnable: Vec<Tid> = st
            .status
            .iter()
            .filter(|(_, s)| **s == Status::Runnable)
            .map(|(t, _)| *t)
            .collect();

        if runnable.is_empty() {
            if st.status.values().any(|s| *s != Status::Finished) {
                let blocked = st
                    .status
                    .values()
                    .filter(|s| **s != Status::Finished)
                    .count();
                crate::trace::raw_stderr(&format!(
                    "[weft] DEADLOCK: {blocked} thread(s) blocked, none can make progress \
                     (at {site})\n"
                ));
                // Deterministic hard failure. Other threads are parked; the
                // SIGABRT tears down the whole process.
                // SAFETY: abort is always sound to call.
                unsafe { libc::abort() };
            }
            st.running = None;
            self.turn.notify_all();
            return;
        }

        // Sort so selection is independent of HashMap iteration order.
        runnable.sort_unstable();
        let next = self.select(st.strategy, &runnable, st);
        st.rr_cursor = next;
        st.running = Some(next);
        self.turn.notify_all();
    }

    #[allow(clippy::unused_self)]
    fn select(&self, strategy: Strategy, runnable: &[Tid], st: &mut State) -> Tid {
        #[allow(clippy::cast_possible_truncation)] // index into a slice
        let random =
            |rng: &mut ChaCha8Rng| runnable[(rng.next_u64() % runnable.len() as u64) as usize];
        match strategy {
            Strategy::Random => random(&mut st.rng),
            Strategy::RoundRobin => {
                // Mostly rotate to the next tid above the cursor; occasionally
                // perturb so a single seed still explores alternatives.
                if st.rng.next_u64() % 100 < 20 {
                    random(&mut st.rng)
                } else {
                    runnable
                        .iter()
                        .copied()
                        .find(|&t| t > st.rr_cursor)
                        .unwrap_or(runnable[0])
                }
            }
        }
    }

    fn wait_turn<'g>(&self, me: Tid, mut st: MutexGuard<'g, State>) -> MutexGuard<'g, State> {
        while st.running != Some(me) {
            st = self.turn.wait(st).unwrap();
        }
        st
    }

    /// Print scheduler statistics to stderr, if enabled. Called at exit.
    pub fn print_stats(&self) {
        if !self.stats {
            return;
        }
        let _g = Reentrancy::enter();
        let st = self.state.lock().unwrap();
        crate::trace::raw_stderr(&format!(
            "[weft] scheduler: {} logical thread(s), {} decision(s), \
             {} distinct yield-point site(s): {:?}\n",
            st.max_threads,
            st.decisions,
            st.sites.len(),
            {
                let mut sites: Vec<&str> = st.sites.iter().copied().collect();
                sites.sort_unstable();
                sites
            },
        ));
    }
}
