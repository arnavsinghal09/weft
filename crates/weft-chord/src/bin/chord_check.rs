//! `chord-check <recording.weftlog> [m]`: reconstruct the final Chord
//! configuration from a recording and evaluate Zave's correctness invariants.
//!
//! Exit codes (CI-friendly, matching the rest of the toolchain):
//!   0  every invariant holds in the final quiescent state
//!   2  at least one correctness invariant is violated
//!   3  DISCARD — the scenario broke the papers' failure precondition
//!      (a failure stranded some node with no live successor), so any
//!      violation in the run would be the harness's, not Chord's
//!   1  the recording could not be read

use std::process::ExitCode;

use weft_chord::Snapshot;
use weft_replay::Log;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: chord-check <recording.weftlog> [m-bits]");
        return ExitCode::from(1);
    };
    let m: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(6);

    let log = match Log::read(std::path::Path::new(&path)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("chord-check: {path}: {e}");
            return ExitCode::from(1);
        }
    };

    let snap = Snapshot::from_log(&log);
    let violations = snap.check(m);
    let assumption_ok = Snapshot::failure_assumption_held(&log);

    println!("=============== CHORD FINAL-STATE CHECK ===============");
    println!(
        "recording : {path}  (seed {}, net {})",
        log.header.seed, log.header.net
    );
    print!("{}", snap.render());
    println!(
        "assumption: papers' failure precondition {}",
        if assumption_ok {
            "HELD at every failure"
        } else {
            "VIOLATED (seed invalid)"
        }
    );

    if !assumption_ok {
        // The run broke the model's own precondition; any violation in it is
        // the harness's artifact, not Chord's. Exit 3 so campaigns discard it.
        println!("\nverdict   : DISCARD — failure assumption violated by the scenario");
        println!("=======================================================");
        return ExitCode::from(3);
    }
    if violations.is_empty() {
        println!("\nverdict   : OK — all correctness invariants hold");
        println!("=======================================================");
        ExitCode::from(0)
    } else {
        // State what the run observed (still violated after the quiescent
        // repair tail); unrepairability is Zave's theorem about these four
        // invariants, not an observation of this run.
        println!(
            "\nverdict   : VIOLATION — still violated after the quiescent repair tail \
             (unrepairable per Zave for these invariants)"
        );
        for v in &violations {
            println!("  ✗ {} : {}", v.invariant.name(), v.detail);
        }
        println!("=======================================================");
        ExitCode::from(2)
    }
}
