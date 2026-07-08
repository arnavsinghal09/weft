//! `raft-check <recording.weftlog>`: scan a recording of raft_node
//! processes for ElectionSafety violations (two leaders in one term).
//!
//! Exit codes (matching chord-check):
//!   0  safe — every term had at most one leader
//!   2  VIOLATION — some term elected two distinct leaders
//!   3  DISCARD — no leader was ever elected (seed exercised nothing)
//!   1  the recording could not be read

use std::process::ExitCode;

use weft_replay::Log;

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: raft-check <recording.weftlog>");
        return ExitCode::from(1);
    };

    let log = match Log::read(std::path::Path::new(&path)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("raft-check: {path}: {e}");
            return ExitCode::from(1);
        }
    };

    let v = weft_raft::check(&log);

    println!("=============== RAFT ELECTION-SAFETY CHECK ===============");
    println!(
        "recording : {path}  (seed {}, net {})",
        log.header.seed, log.header.net
    );
    println!(
        "reports   : {} state reports, {} crash-restarts",
        v.reports, v.restarts
    );
    for (term, leaders) in &v.leaders_by_term {
        println!("term {term:>4}  leaders: {leaders:?}");
    }

    if v.uninformative() {
        println!("\nverdict   : DISCARD — no leader elected; seed uninformative");
        println!("==========================================================");
        return ExitCode::from(3);
    }
    if v.violations.is_empty() {
        println!("\nverdict   : OK — at most one leader per term");
        println!("==========================================================");
        ExitCode::from(0)
    } else {
        println!("\nverdict   : VIOLATION — ElectionSafety broken");
        for viol in &v.violations {
            println!(
                "  ✗ term {}: leaders {:?} (second leader at op {})",
                viol.term, viol.leaders, viol.at_op
            );
        }
        println!("==========================================================");
        ExitCode::from(2)
    }
}
