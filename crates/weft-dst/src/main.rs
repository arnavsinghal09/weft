//! `weft` CLI entry point.
//!
//! Argument parsing stays hand-rolled: one subcommand plus four flags does
//! not yet justify a parser dependency (revisit when `weft replay` lands).

use std::process::ExitCode;

use weft_dst::run;

const USAGE: &str = "\
weft - deterministic simulation testing for unmodified Linux binaries

Usage: weft <COMMAND> [OPTIONS]

Commands:
  run --seed <N> [OPTIONS] -- <program> [args...]
                 Run a program deterministically: time, randomness, thread
                 scheduling (--strategy random|rr), and, with --net <SPEC>
                 [--nodes N], a fully simulated network (latency, loss,
                 reordering, partitions). `weft run -h` lists all options.

Options:
  -h, --help     Print this help
  -V, --version  Print version

Later phases add: replay, fuzz. See PROJECT_NOTES.md for the roadmap.";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    match args.next().as_deref().and_then(std::ffi::OsStr::to_str) {
        Some("run") => match run::parse_args(args) {
            Ok(opts) if opts.net.is_some() => match run::run_cluster(&opts) {
                // Clamp to u8 range; anything non-zero must stay non-zero.
                Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1).max(u8::from(code != 0))),
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
