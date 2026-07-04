//! `weft run --seed <N> -- <program> [args...]`: launch a target under the
//! determinism shim.
//!
//! The orchestrator's whole job in Phase 1 is environment plumbing: locate
//! `libweft_shim.so`, set `LD_PRELOAD` (prepending to any existing value),
//! `WEFT_SEED`, and optionally `WEFT_TRACE`, then `exec` the target so exit
//! codes and signals pass through untouched.

use std::ffi::OsString;
use std::path::PathBuf;

use weft_abi::Strategy;

const SHIM_SO: &str = "libweft_shim.so";

pub const RUN_USAGE: &str = "\
Usage: weft run --seed <N> [OPTIONS] -- <program> [args...]

Launch <program> with deterministic time, randomness, and thread scheduling.

Options:
  --seed <N>          Run seed, decimal or 0x-hex u64 (required)
  --strategy <S>      Scheduler interleaving strategy: 'random' (default) or
                      'rr' (round-robin with perturbation)
  --no-sched          Disable deterministic thread scheduling (time and
                      randomness stay deterministic; OS schedules threads)
  --net <SPEC>        Simulate the network through a seeded broker. SPEC is
                      comma-separated clauses: latency=fixed:N|uniform:LO-HI|
                      exp:MEAN (ns), loss=P, bw=BYTES_PER_SEC,
                      partition=0+1|2 (an empty SPEC is a reliable network)
  --nodes <N>         With --net: launch N instances of the program, node ids
                      0..N-1 (default 1)
  --trace, --verbose  Log every intercepted call to stderr
  --stats             Print scheduler statistics at exit
  --shim <PATH>       Path to libweft_shim.so (default: WEFT_SHIM env,
                      then next to the weft binary)
  -h, --help          Print this help";

#[derive(Debug, PartialEq, Eq)]
pub struct RunOpts {
    pub seed: u64,
    pub trace: bool,
    pub stats: bool,
    pub no_sched: bool,
    pub strategy: Strategy,
    /// Network-condition spec; `Some` engages the broker (even if empty).
    pub net: Option<String>,
    pub nodes: u32,
    pub shim: Option<PathBuf>,
    pub program: Vec<OsString>,
}

/// Parse the arguments after `run`.
///
/// # Errors
///
/// Returns a human-readable message for missing/invalid `--seed`, an unknown
/// option, or a missing program.
pub fn parse_args<I: IntoIterator<Item = OsString>>(args: I) -> Result<RunOpts, String> {
    let mut args = args.into_iter();
    let mut seed: Option<u64> = None;
    let mut trace = false;
    let mut stats = false;
    let mut no_sched = false;
    let mut strategy = Strategy::default();
    let mut net = None;
    let mut nodes = 1u32;
    let mut shim = None;
    let mut program = Vec::new();

    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--seed") => {
                let v = args
                    .next()
                    .ok_or_else(|| "--seed requires a value".to_string())?;
                let v = v.to_str().ok_or_else(|| "--seed value is not UTF-8".to_string())?;
                seed = Some(weft_abi::parse_seed(v).map_err(|e| format!("--seed {v:?}: {e}"))?);
            }
            Some("--strategy") => {
                let v = args
                    .next()
                    .ok_or_else(|| "--strategy requires a value".to_string())?;
                let v = v.to_str().ok_or_else(|| "--strategy value is not UTF-8".to_string())?;
                strategy = Strategy::parse(v).map_err(|e| format!("--strategy {v:?}: {e}"))?;
            }
            Some("--trace" | "--verbose") => trace = true,
            Some("--stats") => stats = true,
            Some("--no-sched") => no_sched = true,
            Some("--net") => {
                let v = args
                    .next()
                    .ok_or_else(|| "--net requires a spec (may be empty: \"\")".to_string())?;
                let v = v.to_str().ok_or_else(|| "--net spec is not UTF-8".to_string())?;
                // Validate eagerly so a typo fails before anything launches.
                weft_net::config::parse(0, v).map_err(|e| format!("--net: {e}"))?;
                net = Some(v.to_string());
            }
            Some("--nodes") => {
                let v = args
                    .next()
                    .ok_or_else(|| "--nodes requires a count".to_string())?;
                let v = v.to_str().ok_or_else(|| "--nodes value is not UTF-8".to_string())?;
                nodes = v
                    .parse()
                    .map_err(|_| format!("--nodes {v:?} is not a positive integer"))?;
                if nodes == 0 || nodes > 64 {
                    return Err("--nodes must be in 1..=64".to_string());
                }
            }
            Some("--shim") => {
                let v = args
                    .next()
                    .ok_or_else(|| "--shim requires a path".to_string())?;
                shim = Some(PathBuf::from(v));
            }
            Some("-h" | "--help") => return Err(RUN_USAGE.to_string()),
            Some("--") => {
                program.extend(args);
                break;
            }
            _ => {
                return Err(format!(
                    "unknown or misplaced argument {:?} (put the program after `--`)",
                    arg.to_string_lossy()
                ))
            }
        }
    }

    let seed = seed.ok_or_else(|| format!("--seed is required\n\n{RUN_USAGE}"))?;
    if program.is_empty() {
        return Err(format!("no program given after `--`\n\n{RUN_USAGE}"));
    }
    if nodes > 1 && net.is_none() {
        return Err("--nodes requires --net (nodes communicate through the broker)".to_string());
    }
    Ok(RunOpts {
        seed,
        trace,
        stats,
        no_sched,
        strategy,
        net,
        nodes,
        shim,
        program,
    })
}

/// Locate the shim: `--shim` flag, then `WEFT_SHIM` env, then next to the
/// `weft` binary itself (where cargo puts both build products).
///
/// # Errors
///
/// Returns a message describing every location tried.
pub fn find_shim(explicit: Option<PathBuf>) -> Result<PathBuf, String> {
    let mut tried = Vec::new();
    let mut candidates = Vec::new();
    if let Some(p) = explicit {
        candidates.push(p);
    }
    if let Some(env) = std::env::var_os(weft_abi::ENV_SHIM) {
        candidates.push(PathBuf::from(env));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(SHIM_SO));
        }
    }
    for c in candidates {
        if c.is_file() {
            return Ok(c);
        }
        tried.push(c.display().to_string());
    }
    Err(format!(
        "cannot find {SHIM_SO}; tried: {}. Build it with `cargo build -p weft-shim` \
         or point --shim / {} at it",
        tried.join(", "),
        weft_abi::ENV_SHIM
    ))
}

/// Build the target `Command` with the full shim environment applied.
#[cfg(target_os = "linux")]
fn target_command(opts: &RunOpts, shim_path: &std::path::Path) -> std::process::Command {
    let mut preload = shim_path.to_path_buf().into_os_string();
    if let Some(existing) = std::env::var_os("LD_PRELOAD") {
        if !existing.is_empty() {
            preload.push(":");
            preload.push(existing);
        }
    }
    let mut cmd = std::process::Command::new(&opts.program[0]);
    cmd.args(&opts.program[1..])
        .env("LD_PRELOAD", preload)
        .env(weft_abi::ENV_SEED, opts.seed.to_string())
        .env(weft_abi::ENV_STRATEGY, opts.strategy.name());
    if opts.trace {
        cmd.env(weft_abi::ENV_TRACE, "1");
    }
    if opts.stats {
        cmd.env(weft_abi::ENV_SCHED_STATS, "1");
    }
    if opts.no_sched {
        cmd.env(weft_abi::ENV_SCHED, "0");
    }
    cmd
}

/// Exec the target under the shim (no network simulation). Only returns on
/// failure to exec.
#[must_use]
#[cfg(target_os = "linux")]
pub fn exec(opts: &RunOpts) -> String {
    use std::os::unix::process::CommandExt;

    let shim_path = match find_shim(opts.shim.clone()) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let err = target_command(opts, &shim_path).exec(); // replaces this process on success
    format!("failed to exec {:?}: {err}", opts.program[0].to_string_lossy())
}

/// Run the target under network simulation: host the seeded broker in this
/// process and spawn `opts.nodes` instances of the program, each with its own
/// node id. Returns the combined exit code (0 iff every node exited 0).
///
/// # Errors
///
/// Returns a message if the shim cannot be found, the net spec is invalid, the
/// broker cannot bind, or a child fails to spawn.
#[cfg(target_os = "linux")]
pub fn run_cluster(opts: &RunOpts) -> Result<i32, String> {
    let shim_path = find_shim(opts.shim.clone())?;
    let spec = opts.net.as_deref().unwrap_or("");
    let model = weft_net::config::parse(opts.seed, spec)?;

    // A per-run socket path (pid disambiguates concurrent weft runs).
    let sock_path = std::env::temp_dir().join(format!("weft-broker-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&sock_path);
    let broker =
        weft_net::Broker::bind(&sock_path, model).map_err(|e| format!("broker bind: {e}"))?;
    let broker = std::sync::Arc::new(broker);
    {
        let broker = std::sync::Arc::clone(&broker);
        std::thread::spawn(move || broker.run());
    }

    let mut children = Vec::new();
    for node in 0..opts.nodes {
        let mut cmd = target_command(opts, &shim_path);
        cmd.env(weft_abi::ENV_BROKER, &sock_path)
            .env(weft_abi::ENV_NODE_ID, node.to_string())
            .env(weft_abi::ENV_NET, spec);
        let child = cmd
            .spawn()
            .map_err(|e| format!("failed to spawn node {node}: {e}"))?;
        children.push((node, child));
    }

    let mut code = 0;
    for (node, mut child) in children {
        match child.wait() {
            Ok(status) => {
                let c = status.code().unwrap_or(128);
                if c != 0 {
                    eprintln!("weft: node {node} exited with {c}");
                    code = c;
                }
            }
            Err(e) => {
                eprintln!("weft: node {node} wait failed: {e}");
                code = 1;
            }
        }
    }
    if opts.stats {
        let (sent, dropped) = broker.stats();
        eprintln!("[weft] network: {sent} datagram(s) sent, {dropped} dropped");
    }
    let _ = std::fs::remove_file(&sock_path);
    Ok(code)
}

/// Non-Linux stub for [`run_cluster`].
///
/// # Errors
///
/// Always: network simulation requires the Linux shim.
#[cfg(not(target_os = "linux"))]
pub fn run_cluster(_opts: &RunOpts) -> Result<i32, String> {
    Err("weft run --net requires Linux (see scripts/linux-test.sh)".to_string())
}

/// Non-Linux stub: `weft run` needs `LD_PRELOAD` semantics.
#[must_use]
#[cfg(not(target_os = "linux"))]
pub fn exec(_opts: &RunOpts) -> String {
    "weft run requires Linux (the shim is injected via LD_PRELOAD). \
     On macOS, develop the orchestrator here and run targets in a Linux \
     container: see scripts/linux-test.sh"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_full_command_line() {
        let opts =
            parse_args(os(&["--seed", "0x2a", "--trace", "--", "prog", "-x", "1"])).unwrap();
        assert_eq!(opts.seed, 42);
        assert!(opts.trace);
        assert_eq!(opts.program, os(&["prog", "-x", "1"]));
    }

    #[test]
    fn rejects_missing_seed_and_missing_program() {
        assert!(parse_args(os(&["--", "prog"])).unwrap_err().contains("--seed is required"));
        assert!(parse_args(os(&["--seed", "1"])).unwrap_err().contains("no program"));
        assert!(parse_args(os(&["--seed", "zzz", "--", "p"])).is_err());
    }

    #[test]
    fn parses_scheduler_flags() {
        let opts = parse_args(os(&[
            "--seed", "1", "--strategy", "rr", "--no-sched", "--stats", "--", "p",
        ]))
        .unwrap();
        assert_eq!(opts.strategy, Strategy::RoundRobin);
        assert!(opts.no_sched);
        assert!(opts.stats);
        assert!(parse_args(os(&["--seed", "1", "--strategy", "bogus", "--", "p"])).is_err());
        // Strategy defaults to Random when unspecified.
        assert_eq!(
            parse_args(os(&["--seed", "1", "--", "p"])).unwrap().strategy,
            Strategy::Random
        );
    }

    #[test]
    fn program_args_after_separator_are_untouched() {
        let opts = parse_args(os(&["--seed", "7", "--", "prog", "--seed", "--trace"])).unwrap();
        assert_eq!(opts.program, os(&["prog", "--seed", "--trace"]));
        assert!(!opts.trace);
    }
}
