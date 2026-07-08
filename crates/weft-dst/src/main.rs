//! `weft` CLI entry point.
//!
//! Argument parsing stays hand-rolled: three subcommands plus a handful of
//! flags does not yet justify a parser dependency.

use std::process::ExitCode;

use weft_dst::{fuzz_cmd, replay_cmd, run};

const USAGE: &str = "\
weft - deterministic simulation testing for unmodified Linux binaries

Usage: weft <COMMAND> [OPTIONS]

Commands:
  run --seed <N> [OPTIONS] -- <program> [args...]
                 Run a program deterministically: time, randomness, thread
                 scheduling (--strategy random|rr), and, with --net <SPEC>
                 [--nodes N], a fully simulated network (latency, loss,
                 reordering, partitions). --record <LOG> captures the run.
                 `weft run -h` lists all options.
  replay <LOG> [--until N] [--check LIST]
                 Re-execute a recorded run and verify it is byte-identical;
                 optionally check invariants. Works on every platform.
                 `weft replay -h` lists all options.
  fuzz --config <FILE> [OPTIONS]
                 Sweep fault seeds against a workload and its invariants,
                 shrink every distinct violation to a minimal reproducer,
                 and report. CI-friendly exit codes (0 clean, 2 violations).
                 `weft fuzz -h` lists all options; see docs/fuzzing.md.

Options:
  -h, --help     Print this help
  -V, --version  Print version";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    match args.next().as_deref().and_then(std::ffi::OsStr::to_str) {
        Some("run") => match run::parse_args(args) {
            Ok(opts) if opts.net.is_some() => match run::run_cluster(&opts) {
                // Clamp to u8 range; anything non-zero must stay non-zero.
                Ok(code) => {
                    ExitCode::from(u8::try_from(code).unwrap_or(1).max(u8::from(code != 0)))
                }
                Err(msg) => {
                    eprintln!("weft run: {msg}");
                    ExitCode::FAILURE
                }
            },
            Ok(opts) => {
                // Only returns on failure (on Linux, success replaces the
                // process via exec so the target's exit status is ours).
                eprintln!("weft run: {}", run::exec(&opts));
                ExitCode::FAILURE
            }
            Err(msg) => {
                eprintln!("{msg}");
                ExitCode::FAILURE
            }
        },
        Some("replay") => match replay_cmd::parse_args(args) {
            Ok(opts) => {
                let code = replay_cmd::execute(&opts);
                ExitCode::from(u8::try_from(code).unwrap_or(1))
            }
            Err(msg) => {
                eprintln!("{msg}");
                ExitCode::FAILURE
            }
        },
        Some("fuzz") => match fuzz_cmd::parse_args(args) {
            Ok(opts) => {
                let code = fuzz_cmd::execute(&opts);
                ExitCode::from(u8::try_from(code).unwrap_or(1))
            }
            Err(msg) => {
                eprintln!("{msg}");
                ExitCode::FAILURE
            }
        },
        Some("-V" | "--version") => {
            println!("weft {}", weft_dst::version());
            ExitCode::SUCCESS
        }
        Some("-h" | "--help") | None => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("weft: unknown command '{other}'\n\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}
