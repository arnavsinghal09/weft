//! `weft` CLI entry point.
//!
//! Phase 0 skeleton: only `--version`/`-V` and `--help`/`-h` exist. Real
//! subcommands (`weft run`, `weft replay`, `weft fuzz`, ...) arrive with the
//! phases that implement them; argument parsing stays hand-rolled until a
//! second subcommand justifies pulling in a parser dependency.

use std::process::ExitCode;

const USAGE: &str = "\
weft - deterministic simulation testing for unmodified Linux binaries

Usage: weft [OPTIONS]

Options:
  -h, --help     Print this help
  -V, --version  Print version

Subcommands land in later phases; see PROJECT_NOTES.md for the roadmap.";

fn main() -> ExitCode {
    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        Some("-V" | "--version") => {
            println!("weft {}", weft_dst::version());
            ExitCode::SUCCESS
        }
        Some("-h" | "--help") | None => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("weft: unknown argument '{other}'\n\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}
