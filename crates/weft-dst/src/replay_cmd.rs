//! `weft replay <LOG> [--until N] [--check LIST]`: verify a recorded run by
//! deterministic re-execution.
//!
//! Replay is pure computation (no clock, no threads, no entropy, no Linux
//! shim), so unlike `weft run` it works on every platform: a log recorded on
//! one machine replays bit-identically on another.

use std::ffi::OsString;
use std::path::PathBuf;

use weft_replay::invariant::{Invariant, NoDuplicateDelivery, PerChannelFifo};
use weft_replay::{replay_log, report, Log};

pub const REPLAY_USAGE: &str = "\
Usage: weft replay <LOG> [OPTIONS]

Re-execute a recorded run (a weft-log file, plain or gzip-compressed —
detected by content) and verify the result is byte-for-byte identical to
the recording. See docs/recording-format.md.

Options:
  --until <OP>    Stop after replaying op <OP> (inclusive): halt right after
                  a violating operation and report state up to that point
  --check <LIST>  Comma-separated invariants to check during replay:
                  fifo (per-channel-fifo), dup (no-duplicate-delivery),
                  or 'all' (default: none — pure verification)
  -h, --help      Print this help";

#[derive(Debug, PartialEq, Eq)]
pub struct ReplayOpts {
    pub log: PathBuf,
    pub until: Option<u64>,
    /// Invariant names as accepted by `--check`.
    pub checks: Vec<String>,
}

/// Parse the arguments after `replay`.
///
/// # Errors
/// Returns a human-readable message for a missing log path or bad option.
pub fn parse_args<I: IntoIterator<Item = OsString>>(args: I) -> Result<ReplayOpts, String> {
    let mut args = args.into_iter();
    let mut log: Option<PathBuf> = None;
    let mut until = None;
    let mut checks = Vec::new();

    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--until") => {
                let v = args
                    .next()
                    .ok_or_else(|| "--until requires an op index".to_string())?;
                let v = v
                    .to_str()
                    .ok_or_else(|| "--until value is not UTF-8".to_string())?;
                until = Some(v.parse().map_err(|_| format!("--until {v:?}: not a u64"))?);
            }
            Some("--check") => {
                let v = args
                    .next()
                    .ok_or_else(|| "--check requires a list".to_string())?;
                let v = v
                    .to_str()
                    .ok_or_else(|| "--check value is not UTF-8".to_string())?;
                for name in v.split(',').filter(|s| !s.is_empty()) {
                    match name {
                        "fifo" | "per-channel-fifo" | "dup" | "no-duplicate-delivery" => {
                            checks.push(name.to_string());
                        }
                        "all" => {
                            checks.push("fifo".to_string());
                            checks.push("dup".to_string());
                        }
                        other => return Err(format!("--check: unknown invariant {other:?}")),
                    }
                }
            }
            Some("-h" | "--help") => return Err(REPLAY_USAGE.to_string()),
            _ if log.is_none() => log = Some(PathBuf::from(arg)),
            _ => {
                return Err(format!(
                    "unexpected argument {:?}\n\n{REPLAY_USAGE}",
                    arg.to_string_lossy()
                ))
            }
        }
    }

    let log = log.ok_or_else(|| format!("no log file given\n\n{REPLAY_USAGE}"))?;
    Ok(ReplayOpts { log, until, checks })
}

fn build_invariants(checks: &[String]) -> Vec<Box<dyn Invariant>> {
    let mut out: Vec<Box<dyn Invariant>> = Vec::new();
    for c in checks {
        match c.as_str() {
            "fifo" | "per-channel-fifo" => out.push(Box::new(PerChannelFifo::new())),
            "dup" | "no-duplicate-delivery" => out.push(Box::new(NoDuplicateDelivery::new())),
            _ => unreachable!("validated in parse_args"),
        }
    }
    out
}

/// Execute `weft replay`. Returns the process exit code: 0 for an identical
/// replay with no violations, 2 for violations, 1 for divergence or errors.
#[must_use]
pub fn execute(opts: &ReplayOpts) -> i32 {
    let log = match Log::read(&opts.log) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("weft replay: {}: {e}", opts.log.display());
            return 1;
        }
    };
    let outcome = match replay_log(&log, build_invariants(&opts.checks), opts.until) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("weft replay: {e}");
            return 1;
        }
    };

    if let Some(d) = &outcome.divergence {
        eprintln!("weft replay: DIVERGED at op {}", d.op);
        eprintln!("  recorded: {}", d.recorded);
        eprintln!("  replayed: {}", d.replayed);
        eprintln!(
            "  (the log, seed, and code no longer agree — was the log produced \
             by a different weft version?)"
        );
        return 1;
    }

    println!(
        "replay identical: {} op(s), stream digest {:016x}{}",
        outcome.ops_replayed,
        outcome.stream_digest,
        if opts.until.is_some() {
            " (partial: --until)"
        } else {
            ""
        },
    );

    if outcome.violations.is_empty() {
        return 0;
    }
    for v in &outcome.violations {
        print!("{}", report::render(v, &log, &opts.log));
    }
    println!("{} invariant violation(s)", outcome.violations.len());
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_log_until_and_checks() {
        let o = parse_args(os(&["r.weftlog", "--until", "9", "--check", "fifo,dup"])).unwrap();
        assert_eq!(o.log, PathBuf::from("r.weftlog"));
        assert_eq!(o.until, Some(9));
        assert_eq!(o.checks, vec!["fifo", "dup"]);
    }

    #[test]
    fn check_all_expands_and_unknown_is_rejected() {
        let o = parse_args(os(&["l", "--check", "all"])).unwrap();
        assert_eq!(o.checks, vec!["fifo", "dup"]);
        assert!(parse_args(os(&["l", "--check", "bogus"])).is_err());
    }

    #[test]
    fn missing_log_is_an_error() {
        assert!(parse_args(os(&[])).unwrap_err().contains("no log file"));
    }
}
