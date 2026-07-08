//! `weft fuzz --config <FILE>`: sweep seeds, shrink failures, report.
//!
//! Pure computation over the broker core — works on every platform. Exit
//! codes are CI-first: 0 = swept clean, 2 = violations found (reproducers
//! written), 1 = configuration or setup error.

use std::ffi::OsString;
use std::path::PathBuf;

use weft_fuzz::{run_fuzz, FuzzConfig};

pub const FUZZ_USAGE: &str = "\
Usage: weft fuzz --config <FILE> [OPTIONS]

Sweep fault seeds against a deterministic workload, check invariants, shrink
every distinct violation to a minimal reproducer, and write a report. See
docs/fuzzing.md for the config file format.

Options:
  --config <FILE>      JSON config (required; flags below override it)
  --seeds <START:N>    Sweep N seeds starting at START (e.g. 0:1000)
  --time-budget <SEC>  Stop sweeping after SEC seconds
  --jobs <N>           Worker threads
  --out <DIR>          Output directory for reproducers and the report
  --no-shrink          Keep full-size reproducers (skip shrinking)
  --regressions <FILE> JSON array of seeds to test before the sweep; on
                       failure the file is refreshed with all failing seeds
  -h, --help           Print this help

Exit codes: 0 no violations · 2 violations found · 1 error";

#[derive(Debug)]
pub struct FuzzOpts {
    pub config: PathBuf,
    pub seeds: Option<(u64, u64)>,
    pub time_budget: Option<u64>,
    pub jobs: Option<usize>,
    pub out: Option<PathBuf>,
    pub no_shrink: bool,
    pub regressions: Option<PathBuf>,
}

/// Parse the arguments after `fuzz`.
///
/// # Errors
/// A human-readable message for a missing config or malformed option.
pub fn parse_args<I: IntoIterator<Item = OsString>>(args: I) -> Result<FuzzOpts, String> {
    let mut args = args.into_iter();
    let mut config = None;
    let mut seeds = None;
    let mut time_budget = None;
    let mut jobs = None;
    let mut out = None;
    let mut no_shrink = false;
    let mut regressions = None;

    let value = |args: &mut dyn Iterator<Item = OsString>, flag: &str| -> Result<String, String> {
        args.next()
            .ok_or_else(|| format!("{flag} requires a value"))?
            .into_string()
            .map_err(|_| format!("{flag} value is not UTF-8"))
    };

    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--config") => config = Some(PathBuf::from(value(&mut args, "--config")?)),
            Some("--seeds") => {
                let v = value(&mut args, "--seeds")?;
                let (start, count) = v
                    .split_once(':')
                    .ok_or_else(|| format!("--seeds {v:?}: expected START:COUNT"))?;
                seeds = Some((
                    start
                        .parse()
                        .map_err(|_| format!("--seeds start {start:?}: not a u64"))?,
                    count
                        .parse()
                        .map_err(|_| format!("--seeds count {count:?}: not a u64"))?,
                ));
            }
            Some("--time-budget") => {
                let v = value(&mut args, "--time-budget")?;
                time_budget = Some(
                    v.parse()
                        .map_err(|_| format!("--time-budget {v:?}: not seconds"))?,
                );
            }
            Some("--jobs") => {
                let v = value(&mut args, "--jobs")?;
                jobs = Some(
                    v.parse()
                        .map_err(|_| format!("--jobs {v:?}: not a count"))?,
                );
            }
            Some("--out") => out = Some(PathBuf::from(value(&mut args, "--out")?)),
            Some("--no-shrink") => no_shrink = true,
            Some("--regressions") => {
                regressions = Some(PathBuf::from(value(&mut args, "--regressions")?));
            }
            Some("-h" | "--help") => return Err(FUZZ_USAGE.to_string()),
            _ => {
                return Err(format!(
                    "unexpected argument {:?}\n\n{FUZZ_USAGE}",
                    arg.to_string_lossy()
                ))
            }
        }
    }
    let config = config.ok_or_else(|| format!("--config is required\n\n{FUZZ_USAGE}"))?;
    Ok(FuzzOpts {
        config,
        seeds,
        time_budget,
        jobs,
        out,
        no_shrink,
        regressions,
    })
}

fn load_regressions(path: &std::path::Path) -> Result<Vec<u64>, String> {
    if !path.exists() {
        return Ok(Vec::new()); // first run: the file is born on first failure
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Execute `weft fuzz`. Returns the process exit code.
#[must_use]
pub fn execute(opts: &FuzzOpts) -> i32 {
    let mut cfg = match FuzzConfig::load(&opts.config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("weft fuzz: {e}");
            return 1;
        }
    };
    if let Some((start, count)) = opts.seeds {
        cfg.seed_start = start;
        cfg.seed_count = count;
    }
    if let Some(t) = opts.time_budget {
        cfg.time_budget_secs = t;
    }
    if let Some(j) = opts.jobs {
        cfg.jobs = j;
    }
    if let Some(o) = &opts.out {
        cfg.out_dir.clone_from(o);
    }
    if opts.no_shrink {
        cfg.shrink = false;
    }
    if let Some(path) = &opts.regressions {
        match load_regressions(path) {
            Ok(mut seeds) => cfg.regression_seeds.append(&mut seeds),
            Err(e) => {
                eprintln!("weft fuzz: --regressions {e}");
                return 1;
            }
        }
    }

    let report = match run_fuzz(&cfg) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("weft fuzz: {e}");
            return 1;
        }
    };

    let text = report.render(&cfg);
    print!("{text}");
    let report_path = cfg.out_dir.join("report.txt");
    if let Err(e) = std::fs::write(&report_path, &text) {
        eprintln!("weft fuzz: could not write {}: {e}", report_path.display());
    }

    if report.violations.is_empty() {
        return 0;
    }
    // Refresh the regression file with every failing seed we now know about.
    if let Some(path) = &opts.regressions {
        let mut seeds = report.regression_seeds();
        seeds.extend(cfg.regression_seeds.iter().copied());
        seeds.sort_unstable();
        seeds.dedup();
        match serde_json::to_string_pretty(&seeds) {
            Ok(body) => {
                if let Err(e) = std::fs::write(path, body) {
                    eprintln!("weft fuzz: could not write {}: {e}", path.display());
                } else {
                    println!("regression seeds written to {}", path.display());
                }
            }
            Err(e) => eprintln!("weft fuzz: {e}"),
        }
    }
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_config_and_overrides() {
        let o = parse_args(os(&[
            "--config",
            "f.json",
            "--seeds",
            "5:100",
            "--jobs",
            "8",
            "--no-shrink",
            "--time-budget",
            "30",
            "--out",
            "outdir",
            "--regressions",
            "r.json",
        ]))
        .unwrap();
        assert_eq!(o.config, PathBuf::from("f.json"));
        assert_eq!(o.seeds, Some((5, 100)));
        assert_eq!(o.jobs, Some(8));
        assert!(o.no_shrink);
        assert_eq!(o.time_budget, Some(30));
        assert_eq!(o.regressions, Some(PathBuf::from("r.json")));
    }

    #[test]
    fn config_is_required_and_seeds_validated() {
        assert!(parse_args(os(&[]))
            .unwrap_err()
            .contains("--config is required"));
        assert!(parse_args(os(&["--config", "f", "--seeds", "banana"]))
            .unwrap_err()
            .contains("START:COUNT"));
    }
}
