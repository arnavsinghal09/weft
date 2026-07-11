//! `raft-oracle <fix-level> <recording.weftlog>...`: replay each recorded
//! ElectionSafety violation's restart schedule against the formal model
//! (raft_model.rs) and classify BOTH_CONFIRM / DYNAMIC_ONLY / UNDECIDED —
//! same contract as chord-oracle.
//!
//! `raft-oracle --exhaustive <fix-level> <restarts>`: MODEL_ONLY sweep over
//! all restart placements (any node, any time, up to <restarts> total).

use std::process::ExitCode;

use stateright::{Checker, Model};
use weft_raft::raft_model::{RaftModel, RestartSchedule};
use weft_raft::{check, parse_report};
use weft_replay::log::Event;
use weft_replay::Log;

struct Extracted {
    members: u8,
    restarts: Vec<u8>,
    max_term: u8,
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn extract(log: &Log) -> Option<Extracted> {
    let mut restarts: Vec<(i32, u8)> = Vec::new();
    let mut max_node = 0u8;
    let mut max_term = 0u8;
    for r in &log.records {
        let Event::Send { payload, .. } = &r.e else {
            continue;
        };
        let Some(bytes) = weft_replay::hash::from_hex(payload) else {
            continue;
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        let Some(rep) = parse_report(text) else {
            continue;
        };
        max_node = max_node.max(rep.node as u8);
        max_term = max_term.max(rep.term.clamp(0, 255) as u8);
        if !rep.alive {
            restarts.push((rep.date, rep.node as u8));
        }
    }
    if max_node == 0 {
        return None;
    }
    restarts.sort_unstable();
    Some(Extracted {
        members: max_node + 1,
        restarts: restarts.into_iter().map(|(_, n)| n).collect(),
        max_term,
    })
}

fn num_threads() -> usize {
    std::thread::available_parallelism().map_or(4, std::num::NonZero::get)
}

fn run_one(path: &str, fix: u8) -> Result<String, String> {
    let log = Log::read(std::path::Path::new(&path)).map_err(|e| format!("{path}: {e}"))?;
    let dynamic = check(&log);
    if dynamic.violations.is_empty() {
        return Ok(format!("{path}\tNO_DYNAMIC_VIOLATION\t-"));
    }
    let ex = extract(&log).ok_or_else(|| format!("{path}: no reports"))?;

    let model = RaftModel {
        members: ex.members,
        fix,
        term_bound: ex.max_term.saturating_add(1),
        schedule: RestartSchedule::Fixed(ex.restarts.clone()),
        max_restarts: 0,
    };
    let checker = model
        .checker()
        .threads(num_threads())
        .target_state_count(20_000_000)
        .spawn_bfs()
        .join();
    let exhausted = checker.is_done();
    let found = checker
        .discoveries()
        .contains_key("election-safety-violated");
    let verdict = if found {
        "BOTH_CONFIRM"
    } else if exhausted {
        "DYNAMIC_ONLY"
    } else {
        "UNDECIDED"
    };
    Ok(format!(
        "{path}\tElectionSafety={verdict}\tstates={} restarts={} term_bound={}",
        checker.unique_state_count(),
        ex.restarts.len(),
        ex.max_term.saturating_add(1),
    ))
}

fn run_exhaustive(fix: u8, restarts: u8) {
    let model = RaftModel {
        members: 5,
        fix,
        term_bound: 3,
        schedule: RestartSchedule::Exhaustive,
        max_restarts: restarts,
    };
    let checker = model
        .checker()
        .threads(num_threads())
        .target_state_count(60_000_000)
        .spawn_bfs()
        .join();
    let exhausted = checker.is_done();
    let found = checker
        .discoveries()
        .contains_key("election-safety-violated");
    println!(
        "exhaustive fix={fix} restarts<={restarts} term_bound=3: states={} exhausted={} \
         ElectionSafety-violation: {}",
        checker.unique_state_count(),
        exhausted,
        if found {
            "REACHABLE"
        } else if exhausted {
            "unreachable (exhaustive within bounds)"
        } else {
            "not found (BOUNDED — state budget hit)"
        }
    );
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--exhaustive") {
        let (Some(fix), Some(r)) = (
            args.get(1).and_then(|s| s.parse::<u8>().ok()),
            args.get(2).and_then(|s| s.parse::<u8>().ok()),
        ) else {
            eprintln!("usage: raft-oracle --exhaustive <fix-level> <max-restarts>");
            return ExitCode::from(1);
        };
        run_exhaustive(fix, r);
        return ExitCode::from(0);
    }
    let Some(fix) = args.first().and_then(|s| s.parse::<u8>().ok()) else {
        eprintln!("usage: raft-oracle <fix-level> <recording.weftlog>...");
        eprintln!("       raft-oracle --exhaustive <fix-level> <max-restarts>");
        return ExitCode::from(1);
    };
    let mut failed = false;
    for path in &args[1..] {
        match run_one(path, fix) {
            Ok(line) => println!("{line}"),
            Err(e) => {
                eprintln!("raft-oracle: {e}");
                failed = true;
            }
        }
    }
    ExitCode::from(u8::from(failed))
}
