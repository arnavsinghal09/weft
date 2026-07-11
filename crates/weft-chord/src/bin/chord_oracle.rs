//! `chord-oracle <fix-level> <m> <recording.weftlog>...`: replay each
//! recorded violation's fault schedule against the formal model
//! (chord_model.rs — synchronous, perfect failure detection) and classify:
//!
//!   BOTH_CONFIRM   the model reaches the same invariant-class violation
//!                  under the same join/fail schedule (some interleaving)
//!   DYNAMIC_ONLY   the model exhaustively cannot reach that class under
//!                  that schedule — the dynamic violation depends on
//!                  something the model excludes (detection latency)
//!   UNDECIDED      the checker hit its state budget before exhausting
//!                  (reported honestly, never folded into either bucket)
//!
//! `chord-oracle --exhaustive <fix-level> <m>`: MODEL_ONLY sweep — explore
//! all schedules of the case-study scenario shape and report which
//! invariant classes are reachable in the model at this fix level.
//!
//! Exit codes: 0 completed (any mix of classes), 1 usage/read error.

use std::process::ExitCode;

use stateright::{Checker, Model};
use weft_chord::chord_model::{ChordModel, SchedEvent, Schedule};
use weft_chord::{parse_report, Snapshot};
use weft_replay::log::Event;
use weft_replay::Log;

/// Idents of the case-study scenario (m=6, 3 base + 3 appendages), derived
/// from each recording rather than hard-coded: base = idents reporting a
/// successor at date 0.
struct Extracted {
    idents: Vec<i8>,
    base: Vec<i8>,
    schedule: Vec<SchedEvent>,
    /// The invariant classes the DYNAMIC checker reports for this log.
    dynamic_classes: Vec<&'static str>,
}

#[allow(clippy::cast_possible_truncation)]
fn extract(log: &Log) -> Option<Extracted> {
    // Chronological report stream (Sends are broker-linearized already).
    let mut reports = Vec::new();
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
        if let Some(st) = parse_report(text) {
            reports.push(st);
        }
    }
    if reports.is_empty() {
        return None;
    }

    let mut idents: Vec<i8> = reports.iter().map(|r| r.ident as i8).collect();
    idents.sort_unstable();
    idents.dedup();

    // Base = members with a successor in their date-0 report.
    let mut base: Vec<i8> = reports
        .iter()
        .filter(|r| r.date == 0 && r.succ >= 0)
        .map(|r| r.ident as i8)
        .collect();
    base.sort_unstable();
    base.dedup();

    // Joins: first date a non-base node reports succ >= 0.
    // Fails: the date of an alive=0 report. Ordered by (date, kind, ident):
    // joins before fails on ties (a node cannot fail before it joined).
    let mut events: Vec<(i32, u8, i8)> = Vec::new();
    for &id in &idents {
        if base.contains(&id) {
            continue;
        }
        if let Some(j) = reports
            .iter()
            .find(|r| r.ident as i8 == id && r.alive && r.succ >= 0)
        {
            events.push((j.date, 0, id));
        }
        if let Some(f) = reports.iter().find(|r| r.ident as i8 == id && !r.alive) {
            events.push((f.date, 1, id));
        }
    }
    events.sort_unstable();
    let schedule = events
        .iter()
        .map(|&(_, kind, id)| {
            if kind == 0 {
                SchedEvent::Join(id)
            } else {
                SchedEvent::Fail(id)
            }
        })
        .collect();

    Some(Extracted {
        idents,
        base,
        schedule,
        dynamic_classes: Vec::new(),
    })
}

fn dynamic_classes(log: &Log, m: u32) -> Vec<&'static str> {
    let snap = Snapshot::from_log(log);
    let mut classes: Vec<&'static str> = snap
        .check(m)
        .into_iter()
        .map(|v| v.invariant.name())
        .collect();
    classes.sort_unstable();
    classes.dedup();
    classes
}

/// Which model property corresponds to a dynamic invariant class.
fn property_for(class: &str) -> &'static str {
    match class {
        "AtLeastOneRing" => "alor-after-schedule",
        "ConnectedAppendages" => "connapp-after-schedule",
        // OrderedRing / AtMostOneRing share one search property.
        _ => "ordered-or-split-after-schedule",
    }
}

fn run_one(path: &str, fix: u8, m: u32) -> Result<String, String> {
    let log = Log::read(std::path::Path::new(path)).map_err(|e| format!("{path}: {e}"))?;
    let mut ex = extract(&log).ok_or_else(|| format!("{path}: no reports in recording"))?;
    ex.dynamic_classes = dynamic_classes(&log, m);
    if ex.dynamic_classes.is_empty() {
        return Ok(format!("{path}\tNO_DYNAMIC_VIOLATION\t-"));
    }

    let model = ChordModel {
        m,
        idents: ex.idents.clone(),
        base: ex.base.clone(),
        fix,
        schedule: Schedule::Fixed(ex.schedule.clone()),
        max_fails: 3,
    };
    let checker = model
        .checker()
        .threads(num_threads())
        .target_state_count(20_000_000)
        .spawn_bfs()
        .join();
    let exhausted = checker.is_done();
    let discoveries = checker.discoveries();

    let mut verdicts = Vec::new();
    for class in &ex.dynamic_classes {
        let prop = property_for(class);
        let verdict = if discoveries.contains_key(prop) {
            "BOTH_CONFIRM"
        } else if exhausted {
            "DYNAMIC_ONLY"
        } else {
            "UNDECIDED"
        };
        verdicts.push(format!("{class}={verdict}"));
    }
    Ok(format!(
        "{path}\t{}\tstates={} sched={} events",
        verdicts.join(","),
        checker.unique_state_count(),
        ex.schedule.len(),
    ))
}

fn run_exhaustive(fix: u8, m: u32) {
    // The case-study universe (matches every campaign recording).
    let model = ChordModel {
        m,
        idents: vec![1, 4, 22, 25, 43, 46],
        base: vec![1, 22, 43],
        fix,
        schedule: Schedule::Exhaustive,
        max_fails: 3,
    };
    let checker = model
        .checker()
        .threads(num_threads())
        .target_state_count(60_000_000)
        .spawn_bfs()
        .join();
    let exhausted = checker.is_done();
    let discoveries = checker.discoveries();
    println!(
        "exhaustive fix={fix}: states={} exhausted={}",
        checker.unique_state_count(),
        exhausted
    );
    for prop in [
        "alor-after-schedule",
        "connapp-after-schedule",
        "ordered-or-split-after-schedule",
    ] {
        let reach = if discoveries.contains_key(prop) {
            "REACHABLE"
        } else if exhausted {
            "unreachable (exhaustive)"
        } else {
            "not found (BOUNDED — state budget hit, not exhaustive)"
        };
        println!("  {prop}: {reach}");
    }
}

fn num_threads() -> usize {
    std::thread::available_parallelism().map_or(4, std::num::NonZero::get)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--exhaustive") {
        let (Some(fix), Some(m)) = (
            args.get(1).and_then(|s| s.parse::<u8>().ok()),
            args.get(2).and_then(|s| s.parse::<u32>().ok()),
        ) else {
            eprintln!("usage: chord-oracle --exhaustive <fix-level> <m>");
            return ExitCode::from(1);
        };
        run_exhaustive(fix, m);
        return ExitCode::from(0);
    }

    let (Some(fix), Some(m)) = (
        args.first().and_then(|s| s.parse::<u8>().ok()),
        args.get(1).and_then(|s| s.parse::<u32>().ok()),
    ) else {
        eprintln!("usage: chord-oracle <fix-level> <m> <recording.weftlog>...");
        eprintln!("       chord-oracle --exhaustive <fix-level> <m>");
        return ExitCode::from(1);
    };
    let mut failed = false;
    for path in &args[2..] {
        match run_one(path, fix, m) {
            Ok(line) => println!("{line}"),
            Err(e) => {
                eprintln!("chord-oracle: {e}");
                failed = true;
            }
        }
    }
    ExitCode::from(u8::from(failed))
}
