//! `chord-trace <recording.weftlog> [m]`: walk the recorded state-report
//! stream in linearization order and show how each node's successor pointers
//! evolve, when nodes die, and the exact operation at which the ring
//! (AtLeastOneRing) breaks — distinguishing a transient break that heals from
//! the permanent break that is the bug.
//!
//! This is the instrument for the root-cause trace: it turns a 3000-line
//! recording into the handful of load-bearing state transitions.

use std::collections::HashMap;
use std::process::ExitCode;

use weft_chord::{parse_report, NodeState, Snapshot};
use weft_replay::log::Event;
use weft_replay::Log;

fn report_of(e: &Event) -> Option<NodeState> {
    let Event::Send { payload, .. } = e else {
        return None;
    };
    let bytes = weft_replay::hash::from_hex(payload)?;
    parse_report(std::str::from_utf8(&bytes).ok()?)
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: chord-trace <recording.weftlog> [m-bits]");
        return ExitCode::from(1);
    };
    // m-bits is accepted for CLI symmetry with chord-check; the trace itself
    // is identifier-space-agnostic (ring membership comes from Snapshot).
    let _m: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(6);

    let log = match Log::read(std::path::Path::new(&path)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("chord-trace: {path}: {e}");
            return ExitCode::from(1);
        }
    };

    println!(
        "=== CHORD STATE TRACE: {path} (seed {}) ===",
        log.header.seed
    );

    let mut snap = Snapshot::default();
    let mut last: HashMap<i32, NodeState> = HashMap::new();
    let mut prev_ring_empty = false;
    let mut first_permanent_break: Option<u64> = None;

    for r in &log.records {
        let Some(st) = report_of(&r.e) else { continue };
        // Only print genuine state changes (dedup the per-tick RPT chatter).
        let changed = last.get(&st.ident).is_none_or(|p| {
            p.succ != st.succ || p.succ2 != st.succ2 || p.prdc != st.prdc || p.alive != st.alive
        });
        snap.observe(st);
        last.insert(st.ident, st);

        if !changed {
            continue;
        }

        let ring = snap.ring_members();
        let ring_empty = ring.is_empty();
        // Track the last transition into a permanently-empty ring.
        if ring_empty && !prev_ring_empty {
            first_permanent_break = Some(r.op);
        }
        prev_ring_empty = ring_empty;

        if st.alive {
            println!(
                "op {:>5} node {:>3}  succ={:>3} succ2={:>3} prdc={:>3}  ring={:?}",
                r.op, st.ident, st.succ, st.succ2, st.prdc, ring
            );
        } else {
            println!(
                "op {:>5} node {:>3}  *** FAILED ***  ring={:?}",
                r.op, st.ident, ring
            );
        }
    }

    println!("--- final ---");
    print!("{}", snap.render());
    let final_ring = snap.ring_members();
    if final_ring.is_empty() {
        if let Some(op) = first_permanent_break {
            println!(
                "AtLeastOneRing broke permanently at op {op} and never recovered \
                 through the quiescent tail."
            );
        }
    }
    ExitCode::SUCCESS
}
