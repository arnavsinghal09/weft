//! In-process scheduler tests, designed to run under AddressSanitizer and
//! ThreadSanitizer in CI. They drive the [`Scheduler`] with real OS threads
//! that replicate exactly what `hooks::thread`'s trampoline does — register a
//! child, park until scheduled, perform modeled sync operations, finish — so
//! the sanitizers see the same concurrency the shim exercises in production.

#![cfg(target_os = "linux")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use weft_abi::Strategy;
use weft_shim::sched::{Scheduler, Tid};

/// Run `body` on each of `workers` scheduler-managed threads, on a dedicated
/// "main" OS thread so the per-thread scheduler id starts clean. Returns
/// whatever the shared `state` accumulates.
fn scenario<S, F>(seed: u64, strategy: Strategy, workers: usize, state: S, body: F) -> S
where
    S: Send + Sync + 'static,
    F: Fn(&Scheduler, Tid, &S) + Send + Sync + 'static,
{
    let sched = Arc::new(Scheduler::new(seed, strategy, false));
    let state = Arc::new(state);
    let body = Arc::new(body);

    let (sched_outer, state_outer, body_outer) =
        (Arc::clone(&sched), Arc::clone(&state), Arc::clone(&body));
    std::thread::spawn(move || {
        sched_outer.ensure_main_registered();
        let mut handles = Vec::new();
        for _ in 0..workers {
            let tid = sched_outer.register_child();
            let (sc, st, bd) = (
                Arc::clone(&sched_outer),
                Arc::clone(&state_outer),
                Arc::clone(&body_outer),
            );
            let h = std::thread::spawn(move || {
                sc.child_started(tid);
                // A panicking body must still hand the token back: a worker
                // that dies without `thread_finished` leaves the scheduler
                // waiting on it forever, turning an assertion failure into a
                // silent hang of the whole test binary.
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    bd(&sc, tid, &st);
                }));
                sc.thread_finished(tid);
                if let Err(p) = r {
                    std::panic::resume_unwind(p);
                }
            });
            handles.push((h, tid));
        }
        for (_, tid) in &handles {
            sched_outer.join(*tid);
        }
        for (h, _) in handles {
            h.join().unwrap();
        }
    })
    .join()
    .unwrap();

    Arc::try_unwrap(state).unwrap_or_else(|_| panic!("state still shared"))
}

const APP_MUTEX: usize = 0xA11CE;
const APP_COND: usize = 0x00C0_FFEE;

/// A worker that repeatedly enters a modeled critical section and records its
/// tid, producing an interleaving order log.
fn logging_body(
    iters: usize,
) -> impl Fn(&Scheduler, Tid, &Mutex<Vec<Tid>>) + Send + Sync + 'static {
    move |sched, tid, log| {
        for _ in 0..iters {
            sched.mutex_lock(APP_MUTEX);
            log.lock().unwrap().push(tid);
            sched.mutex_unlock(APP_MUTEX);
        }
    }
}

fn order_log(seed: u64, strategy: Strategy, workers: usize, iters: usize) -> Vec<Tid> {
    let log = scenario(
        seed,
        strategy,
        workers,
        Mutex::new(Vec::new()),
        logging_body(iters),
    );
    log.into_inner().unwrap()
}

#[test]
fn same_seed_reproduces_the_interleaving() {
    for strategy in [Strategy::Random, Strategy::RoundRobin] {
        let a = order_log(123, strategy, 4, 10);
        let b = order_log(123, strategy, 4, 10);
        assert_eq!(a, b, "{strategy:?} interleaving not reproducible");
        assert_eq!(a.len(), 40, "some critical sections were lost");
    }
}

#[test]
fn different_seeds_diverge() {
    let logs: Vec<Vec<Tid>> = (0..8)
        .map(|s| order_log(s, Strategy::Random, 4, 10))
        .collect();
    let distinct = logs.iter().collect::<std::collections::HashSet<_>>().len();
    assert!(distinct > 1, "seed had no effect on the interleaving");
}

#[test]
fn stress_many_threads_and_iterations() {
    // Heavy contention on one modeled mutex: must complete (no hang, no lost
    // section) and stay reproducible.
    let a = order_log(7, Strategy::Random, 16, 50);
    assert_eq!(a.len(), 16 * 50);
    assert_eq!(a, order_log(7, Strategy::Random, 16, 50));
}

#[test]
fn threads_that_exit_at_different_times_all_join() {
    // Worker i runs i+1 critical sections, so they finish at very different
    // points; the harness's joins must all complete.
    let counts = Arc::new((0..8).map(|_| AtomicBool::new(false)).collect::<Vec<_>>());
    let seen = scenario(11, Strategy::Random, 8, counts, |sched, tid, done| {
        // The harness main registers first as tid 0, so workers are 1..=8.
        let idx = usize::try_from(tid).unwrap() - 1;
        for _ in 0..=idx {
            sched.mutex_lock(APP_MUTEX);
            sched.mutex_unlock(APP_MUTEX);
        }
        done[idx].store(true, Ordering::Relaxed);
    });
    assert!(
        seen.iter().all(|b| b.load(Ordering::Relaxed)),
        "a thread never ran"
    );
}

#[test]
fn nested_lock_acquisition() {
    // Each worker holds two modeled mutexes in a consistent nested order; no
    // deadlock, fully reproducible.
    const OUTER: usize = 1;
    const INNER: usize = 2;
    let body = |sched: &Scheduler, tid: Tid, log: &Mutex<Vec<Tid>>| {
        for _ in 0..8 {
            sched.mutex_lock(OUTER);
            sched.mutex_lock(INNER);
            log.lock().unwrap().push(tid);
            sched.mutex_unlock(INNER);
            sched.mutex_unlock(OUTER);
        }
    };
    let a = scenario(5, Strategy::Random, 6, Mutex::new(Vec::new()), body)
        .into_inner()
        .unwrap();
    let b = scenario(5, Strategy::Random, 6, Mutex::new(Vec::new()), body)
        .into_inner()
        .unwrap();
    assert_eq!(a, b);
    assert_eq!(a.len(), 48);
}

#[test]
fn condvar_rendezvous() {
    // Workers wait on a condition until a flag is set; the main thread sets it
    // and broadcasts. Every worker must wake and record itself.
    struct Shared {
        ready: AtomicBool,
        log: Mutex<Vec<Tid>>,
    }
    let shared = Shared {
        ready: AtomicBool::new(false),
        log: Mutex::new(Vec::new()),
    };
    // The "release" is performed by worker 0 acting as coordinator once it is
    // scheduled; all others wait for it. This keeps everything inside managed
    // worker threads.
    let result = scenario(3, Strategy::Random, 5, shared, |sched, tid, sh| {
        sched.mutex_lock(APP_MUTEX);
        if tid == 1 {
            // First registered worker releases the rest.
            sh.ready.store(true, Ordering::Relaxed);
            sched.cond_broadcast(APP_COND);
        } else {
            while !sh.ready.load(Ordering::Relaxed) {
                sched.cond_wait(APP_COND, APP_MUTEX);
            }
        }
        sh.log.lock().unwrap().push(tid);
        sched.mutex_unlock(APP_MUTEX);
    });
    let mut order = result.log.into_inner().unwrap();
    order.sort_unstable();
    assert_eq!(
        order,
        vec![1, 2, 3, 4, 5],
        "not every worker woke and recorded"
    );
}

#[test]
fn net_block_promotes_the_sole_blocked_thread() {
    // A managed worker that blocks on the network with no runnable sibling
    // must be promoted back to run (it becomes the process's poller) rather
    // than deadlock — and the promotion draws no RNG. Test completion proves
    // `net_block` returns; the flag proves control ran past the park.
    const KEY: usize = 0x5000;
    let ran = scenario(
        1,
        Strategy::Random,
        1,
        AtomicBool::new(false),
        |sched, _tid, flag| {
            sched.net_block(KEY);
            flag.store(true, Ordering::Relaxed);
        },
    );
    assert!(
        ran.load(Ordering::Relaxed),
        "net_block never returned for the sole thread"
    );
}

#[test]
fn net_block_rotates_between_two_idle_waiters() {
    // Two managed workers both block on the network with nothing else
    // runnable. Round-robin promotion must hand each of them a turn (rather
    // than promoting one forever), so both return from `net_block` and
    // finish. A worker that never resumed would leave the harness join
    // hanging; completion plus both flags proves neither starved.
    let seen = scenario(
        2,
        Strategy::Random,
        2,
        (AtomicBool::new(false), AtomicBool::new(false)),
        |sched, tid, flags| {
            sched.net_block(0x6000 + usize::try_from(tid).unwrap());
            if tid == 1 {
                flags.0.store(true, Ordering::Relaxed);
            } else {
                flags.1.store(true, Ordering::Relaxed);
            }
        },
    );
    assert!(
        seen.0.load(Ordering::Relaxed) && seen.1.load(Ordering::Relaxed),
        "a net-blocked waiter was never promoted"
    );
}
