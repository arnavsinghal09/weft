//! Delta-debugging shrinker: reduce an op-input sequence to a (locally)
//! minimal one that still triggers the *same* failure.
//!
//! "Same failure" is semantic, not textual: removing a send shifts the
//! channel sequence numbers of every later send on that channel, so their
//! fates — and any violation message quoting them — legitimately change. A
//! candidate is accepted iff it still raises a violation with the same
//! [`ViolationKey`]: the invariant's name plus the channel (src→dst) of the
//! violating event. That keeps the reproducer *interpretable*: the shrunk
//! log fails the same invariant on the same channel as the original, under
//! the same seed and net spec (which are never varied — changing them would
//! reproduce a different run, not a smaller version of this one).
//!
//! Passes, in order:
//! 1. **Truncate** everything after the first target violation.
//! 2. **ddmin** chunk removal over removable ops (everything except
//!    `Connect`, which is cheap, structural, and kept for readability).
//! 3. **1-minimal sweep**: single-op removals to a fixpoint.
//! 4. **Payload pass**: shrink each surviving send's payload to one byte
//!    (length only matters to fates under a bandwidth cap; the check decides).
//! 5. **Connect GC**: drop `Connect`s whose conn no longer appears.
//!
//! ddmin guarantees 1-minimality w.r.t. these moves, not a global minimum;
//! the ground-truth tests (tests/shrink_ground_truth.rs) pin down that it
//! reaches the known exact minimum on isolated triggers.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use weft_replay::invariant::{Invariant, ViolationRecord};
use weft_replay::log::{Event, RecvOutcome};

use crate::input::{execute, OpInput};

/// The identity of a failure for dedup and shrink acceptance.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ViolationKey {
    pub invariant: String,
    /// "src → dst" of the violating event's channel, or empty when the event
    /// carries no channel (e.g. an end-of-run style invariant).
    pub subject: String,
}

impl ViolationKey {
    #[must_use]
    pub fn of(v: &ViolationRecord) -> Self {
        let subject = match &v.event {
            Event::Recv {
                outcome: RecvOutcome::Delivered { src, dst, .. },
                ..
            } => {
                format!("{src} → {dst}")
            }
            Event::Send { src, dst, .. } => format!("{src} → {dst}"),
            _ => String::new(),
        };
        Self {
            invariant: v.invariant.clone(),
            subject,
        }
    }
}

impl std::fmt::Display for ViolationKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.subject.is_empty() {
            write!(f, "{}", self.invariant)
        } else {
            write!(f, "{} on {}", self.invariant, self.subject)
        }
    }
}

/// Fresh invariant instances for every candidate execution (invariants are
/// stateful, so one set can never be reused across runs).
pub type InvariantFactory<'a> = dyn Fn() -> Vec<Box<dyn Invariant>> + Sync + 'a;

/// What the shrinker did, for the report.
#[derive(Clone, Copy, Debug, Default)]
pub struct ShrinkStats {
    pub ops_before: usize,
    pub ops_after: usize,
    pub executions: usize,
    /// True when the execution cap stopped the search early (result is still
    /// a valid reproducer, just possibly not 1-minimal).
    pub budget_exhausted: bool,
}

/// Upper bound on candidate executions per shrink, so a pathological case
/// cannot stall the fuzz loop. Each execution is a pure in-process run of the
/// core (no I/O), so thousands are cheap.
const MAX_EXECUTIONS: usize = 4000;

struct Checker<'a> {
    seed: u64,
    net: &'a str,
    invariants: &'a InvariantFactory<'a>,
    target: &'a ViolationKey,
    executions: AtomicUsize,
}

impl Checker<'_> {
    /// Does this candidate still raise the target violation?
    fn fails(&self, ops: &[OpInput]) -> bool {
        self.executions.fetch_add(1, Ordering::Relaxed);
        let Ok(out) = execute(self.seed, self.net, ops, (self.invariants)()) else {
            return false;
        };
        out.violations
            .iter()
            .any(|v| ViolationKey::of(v) == *self.target)
    }

    /// First input index whose event completes the target violation, if any.
    fn first_failure_op(&self, ops: &[OpInput]) -> Option<usize> {
        self.executions.fetch_add(1, Ordering::Relaxed);
        let out = execute(self.seed, self.net, ops, (self.invariants)()).ok()?;
        out.violations
            .iter()
            .find(|v| ViolationKey::of(v) == *self.target)
            .map(|v| usize::try_from(v.op).expect("op fits usize"))
    }

    fn spent(&self) -> usize {
        self.executions.load(Ordering::Relaxed)
    }
    fn exhausted(&self) -> bool {
        self.spent() >= MAX_EXECUTIONS
    }
}

fn removable(op: &OpInput) -> bool {
    !matches!(op, OpInput::Connect { .. })
}

/// One ddmin round: try removing each of `n` chunks of the removable ops (in
/// parallel — candidates are independent pure executions); adopt the
/// lowest-index success so the result stays deterministic regardless of
/// thread timing.
fn ddmin_round(ops: &[OpInput], n: usize, checker: &Checker<'_>) -> Option<Vec<OpInput>> {
    let removable_idx: Vec<usize> = (0..ops.len()).filter(|&i| removable(&ops[i])).collect();
    if removable_idx.len() < 2 {
        return None;
    }
    let chunks: Vec<&[usize]> = removable_idx
        .chunks(removable_idx.len().div_ceil(n))
        .collect();

    let winner: Mutex<Option<(usize, Vec<OpInput>)>> = Mutex::new(None);
    std::thread::scope(|s| {
        for (ci, chunk) in chunks.iter().enumerate() {
            let winner = &winner;
            let chunk: Vec<usize> = chunk.to_vec();
            s.spawn(move || {
                if checker.exhausted() {
                    return;
                }
                let candidate: Vec<OpInput> = ops
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !chunk.contains(i))
                    .map(|(_, op)| op.clone())
                    .collect();
                if checker.fails(&candidate) {
                    let mut w = winner.lock().unwrap();
                    if w.as_ref().is_none_or(|(best, _)| ci < *best) {
                        *w = Some((ci, candidate));
                    }
                }
            });
        }
    });
    winner.into_inner().unwrap().map(|(_, c)| c)
}

/// Shrink `ops` to a 1-minimal sequence still failing with `target` under
/// `(seed, net)`. Never reorders ops, never renumbers conns or addresses,
/// never touches seed or net — the reproducer stays a recognizable
/// subsequence of the original run.
#[must_use]
pub fn shrink(
    seed: u64,
    net: &str,
    ops: &[OpInput],
    invariants: &InvariantFactory<'_>,
    target: &ViolationKey,
) -> (Vec<OpInput>, ShrinkStats) {
    let checker = Checker {
        seed,
        net,
        invariants,
        target,
        executions: AtomicUsize::new(0),
    };
    let mut stats = ShrinkStats {
        ops_before: ops.len(),
        ..ShrinkStats::default()
    };

    // Pass 1: truncate after the first target violation.
    let mut cur: Vec<OpInput> = if let Some(k) = checker.first_failure_op(ops) {
        ops[..=k].to_vec()
    } else {
        // The caller's violation does not reproduce at all — return the
        // input untouched rather than "shrink" to something unrelated.
        stats.ops_after = ops.len();
        stats.executions = checker.spent();
        return (ops.to_vec(), stats);
    };

    // Pass 2: ddmin with granularity doubling.
    let mut n = 2usize;
    loop {
        let removable_now = cur.iter().filter(|o| removable(o)).count();
        if removable_now < 2 || n > removable_now || checker.exhausted() {
            break;
        }
        if let Some(smaller) = ddmin_round(&cur, n, &checker) {
            cur = smaller;
            n = n.saturating_sub(1).max(2);
        } else {
            if n >= removable_now {
                break;
            }
            n = (n * 2).min(removable_now);
        }
    }

    // Pass 3: single-op removal to a fixpoint (1-minimality).
    let mut changed = true;
    while changed && !checker.exhausted() {
        changed = false;
        let mut i = 0;
        while i < cur.len() {
            if removable(&cur[i]) {
                let mut candidate = cur.clone();
                candidate.remove(i);
                if checker.fails(&candidate) {
                    cur = candidate;
                    changed = true;
                    continue; // same index now holds the next op
                }
            }
            i += 1;
        }
    }

    // Pass 4: payload simplification (keep one byte per send).
    for i in 0..cur.len() {
        if checker.exhausted() {
            break;
        }
        if let OpInput::Send { payload, .. } = &cur[i] {
            if payload.len() > 1 {
                let mut candidate = cur.clone();
                if let OpInput::Send { payload, .. } = &mut candidate[i] {
                    payload.truncate(1);
                }
                if checker.fails(&candidate) {
                    cur = candidate;
                }
            }
        }
    }

    // Pass 5: drop Connects whose conn no longer appears in any other op.
    let used: Vec<u64> = cur
        .iter()
        .filter(|o| !matches!(o, OpInput::Connect { .. }))
        .map(OpInput::conn)
        .collect();
    cur.retain(|o| match o {
        OpInput::Connect { conn } => used.contains(conn),
        _ => true,
    });
    // GC must not break the reproducer; if it somehow did, undo by keeping
    // the pre-GC sequence (cheap final guard).
    if !checker.fails(&cur) {
        // Should be unreachable: connects carry no channel state. Guard anyway.
        stats.budget_exhausted = checker.exhausted();
        stats.executions = checker.spent();
        let mut pre_gc = cur;
        for op in ops {
            if let OpInput::Connect { .. } = op {
                if !pre_gc.contains(op) {
                    pre_gc.insert(0, op.clone());
                }
            }
        }
        stats.ops_after = pre_gc.len();
        return (pre_gc, stats);
    }

    stats.ops_after = cur.len();
    stats.executions = checker.spent();
    stats.budget_exhausted = checker.exhausted();
    (cur, stats)
}
