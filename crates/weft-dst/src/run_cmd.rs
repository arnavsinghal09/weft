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
  --window <NS>       With --net: run the windowed multi-host sequencer with a
                      window width of NS nanoseconds, making cross-process
                      delivery order a pure function of the seed (default: off,
                      single-host arrival-routed broker)
  --record <LOG>      With --net: record every broker operation to <LOG> for
                      later `weft replay`; a .gz path is gzip-compressed
                      (see docs/recording-format.md)
  --watchdog <SECS>   With --net: abort and discard the run if the broker
                      makes no progress for SECS seconds (a deadlock or a
                      guest wedged in uninstrumented compute); 0 = off
  --window-ops <N>    With --window: discard the run if one node buffers
                      more than N sends inside a single window (backpressure
                      against a send-spamming guest); 0 = unbounded
  --listen <IP:PORT>  With --net: host the broker on TCP instead of a Unix
                      socket so nodes on other hosts can join (--broker there)
  --broker <IP:PORT>  Join a broker another `weft run --listen` is hosting,
                      instead of hosting one (the remote half of a multi-host
                      run; --record stays on the hosting side)
  --spawn <LO-HI>     Node ids to launch locally, inclusive (default 0-N-1);
                      with --listen/--broker each host launches its share and
                      no window seals until all --nodes ids have joined
  --host-id <N>       This host's id in a multi-host run (default 0): the
                      second tier of the windowed ordering key, keeping
                      hosts totally ordered even if node numbering overlaps
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
    /// Multi-host clock-protocol window width in ns (0 = single-host,
    /// arrival-routed broker). Requires `--net`. See
    /// docs/MULTI_HOST_CLOCK_PROTOCOL.md.
    pub window: u64,
    /// Record broker operations to this weft-log file (requires `--net`).
    pub record: Option<PathBuf>,
    /// Real-time no-progress watchdog in seconds (0 = off). Requires `--net`.
    /// If the broker makes no progress for this long the run is aborted and
    /// marked invalid (discard) — the design's F3/F6 handling. Only ever
    /// discards; a completed run is never altered.
    pub watchdog: u64,
    /// F7 backpressure bound (0 = unbounded): the most sends one node may
    /// buffer inside a single window before the run is discarded. Requires
    /// `--window`.
    pub window_ops: usize,
    /// Host the broker on this TCP address (`IP:PORT`) instead of a Unix
    /// socket, so nodes on other hosts can join (`--broker` on their side).
    pub listen: Option<String>,
    /// Join the broker at this TCP address instead of hosting one — the
    /// remote half of a multi-host run. Excludes `--listen`/`--record`.
    pub broker: Option<String>,
    /// Node ids to spawn locally, inclusive (multi-host: each host spawns its
    /// share; the join barrier still waits for all `--nodes`). Defaults to
    /// `0..nodes-1`.
    pub spawn: Option<(u32, u32)>,
    /// This host's id in a multi-host run (default 0) — the second tier of
    /// the windowed sort key. Requires `--net`.
    pub host_id: u32,
    pub shim: Option<PathBuf>,
    pub program: Vec<OsString>,
}

/// Parse the arguments after `run`.
///
/// # Errors
///
/// Returns a human-readable message for missing/invalid `--seed`, an unknown
/// option, or a missing program.
#[allow(clippy::too_many_lines)] // one match arm per flag; splitting obscures it
pub fn parse_args<I: IntoIterator<Item = OsString>>(args: I) -> Result<RunOpts, String> {
    let mut args = args.into_iter();
    let mut seed: Option<u64> = None;
    let mut trace = false;
    let mut stats = false;
    let mut no_sched = false;
    let mut strategy = Strategy::default();
    let mut net = None;
    let mut nodes = 1u32;
    let mut window = 0u64;
    let mut watchdog = 0u64;
    let mut window_ops = 0usize;
    let mut record = None;
    let mut listen = None;
    let mut broker = None;
    let mut spawn = None;
    let mut host_id = 0u32;
    let mut shim = None;
    let mut program = Vec::new();

    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--seed") => {
                let v = args
                    .next()
                    .ok_or_else(|| "--seed requires a value".to_string())?;
                let v = v
                    .to_str()
                    .ok_or_else(|| "--seed value is not UTF-8".to_string())?;
                seed = Some(weft_abi::parse_seed(v).map_err(|e| format!("--seed {v:?}: {e}"))?);
            }
            Some("--strategy") => {
                let v = args
                    .next()
                    .ok_or_else(|| "--strategy requires a value".to_string())?;
                let v = v
                    .to_str()
                    .ok_or_else(|| "--strategy value is not UTF-8".to_string())?;
                strategy = Strategy::parse(v).map_err(|e| format!("--strategy {v:?}: {e}"))?;
            }
            Some("--trace" | "--verbose") => trace = true,
            Some("--stats") => stats = true,
            Some("--no-sched") => no_sched = true,
            Some("--net") => {
                let v = args
                    .next()
                    .ok_or_else(|| "--net requires a spec (may be empty: \"\")".to_string())?;
                let v = v
                    .to_str()
                    .ok_or_else(|| "--net spec is not UTF-8".to_string())?;
                // Validate eagerly so a typo fails before anything launches.
                weft_net::config::parse(0, v).map_err(|e| format!("--net: {e}"))?;
                net = Some(v.to_string());
            }
            Some("--nodes") => {
                let v = args
                    .next()
                    .ok_or_else(|| "--nodes requires a count".to_string())?;
                let v = v
                    .to_str()
                    .ok_or_else(|| "--nodes value is not UTF-8".to_string())?;
                nodes = v
                    .parse()
                    .map_err(|_| format!("--nodes {v:?} is not a positive integer"))?;
                if nodes == 0 || nodes > 64 {
                    return Err("--nodes must be in 1..=64".to_string());
                }
            }
            Some("--window") => {
                let v = args
                    .next()
                    .ok_or_else(|| "--window requires a width in ns".to_string())?;
                let v = v
                    .to_str()
                    .ok_or_else(|| "--window value is not UTF-8".to_string())?;
                window = v
                    .parse()
                    .map_err(|_| format!("--window {v:?} is not a non-negative integer (ns)"))?;
                if window == 0 {
                    return Err("--window width must be non-zero (ns)".to_string());
                }
            }
            Some("--watchdog") => {
                let v = args
                    .next()
                    .ok_or_else(|| "--watchdog requires a value in seconds".to_string())?;
                let v = v
                    .to_str()
                    .ok_or_else(|| "--watchdog value is not UTF-8".to_string())?;
                watchdog = v.parse().map_err(|_| {
                    format!("--watchdog {v:?} is not a non-negative integer (secs)")
                })?;
            }
            Some("--window-ops") => {
                let v = args
                    .next()
                    .ok_or_else(|| "--window-ops requires a count".to_string())?;
                let v = v
                    .to_str()
                    .ok_or_else(|| "--window-ops value is not UTF-8".to_string())?;
                window_ops = v
                    .parse()
                    .map_err(|_| format!("--window-ops {v:?} is not a non-negative integer"))?;
            }
            Some("--record") => {
                let v = args
                    .next()
                    .ok_or_else(|| "--record requires a log path".to_string())?;
                record = Some(PathBuf::from(v));
            }
            Some("--listen") => {
                let v = args
                    .next()
                    .ok_or_else(|| "--listen requires an IP:PORT address".to_string())?;
                let v = v
                    .to_str()
                    .ok_or_else(|| "--listen address is not UTF-8".to_string())?;
                listen = Some(v.to_string());
            }
            Some("--broker") => {
                let v = args
                    .next()
                    .ok_or_else(|| "--broker requires an IP:PORT address".to_string())?;
                let v = v
                    .to_str()
                    .ok_or_else(|| "--broker address is not UTF-8".to_string())?;
                broker = Some(v.to_string());
            }
            Some("--host-id") => {
                let v = args
                    .next()
                    .ok_or_else(|| "--host-id requires a value".to_string())?;
                let v = v
                    .to_str()
                    .ok_or_else(|| "--host-id value is not UTF-8".to_string())?;
                host_id = v
                    .parse()
                    .map_err(|_| format!("--host-id {v:?} is not a non-negative integer"))?;
            }
            Some("--spawn") => {
                let v = args
                    .next()
                    .ok_or_else(|| "--spawn requires a LO-HI node id range".to_string())?;
                let v = v
                    .to_str()
                    .ok_or_else(|| "--spawn range is not UTF-8".to_string())?;
                let (lo, hi) = v
                    .split_once('-')
                    .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
                    .ok_or_else(|| format!("--spawn {v:?} is not LO-HI (e.g. 0-2)"))?;
                if lo > hi {
                    return Err(format!("--spawn {v:?}: LO must be <= HI"));
                }
                spawn = Some((lo, hi));
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
    if record.is_some() && net.is_none() {
        return Err("--record requires --net (recording captures broker operations)".to_string());
    }
    if window > 0 && net.is_none() {
        return Err(
            "--window requires --net (the windowed sequencer orders broker ops)".to_string(),
        );
    }
    if watchdog > 0 && net.is_none() {
        return Err("--watchdog requires --net (progress is measured at the broker)".to_string());
    }
    if (listen.is_some() || broker.is_some() || spawn.is_some() || host_id > 0) && net.is_none() {
        return Err("--listen/--broker/--spawn/--host-id require --net".to_string());
    }
    if window_ops > 0 && window == 0 {
        return Err("--window-ops requires --window (it bounds a window's buffer)".to_string());
    }
    if listen.is_some() && broker.is_some() {
        return Err(
            "--listen and --broker are mutually exclusive (host or join, not both)".to_string(),
        );
    }
    if broker.is_some() && record.is_some() {
        return Err(
            "--record needs the hosting side (--listen); the joining side has no broker"
                .to_string(),
        );
    }
    if broker.is_some() && watchdog > 0 {
        return Err(
            "--watchdog needs the hosting side (--listen); progress is measured at the broker"
                .to_string(),
        );
    }
    if let Some((_, hi)) = spawn {
        if hi >= nodes {
            return Err(format!(
                "--spawn id {hi} is out of range for --nodes {nodes}"
            ));
        }
    }
    Ok(RunOpts {
        seed,
        trace,
        stats,
        no_sched,
        strategy,
        net,
        nodes,
        window,
        record,
        watchdog,
        window_ops,
        listen,
        broker,
        spawn,
        host_id,
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
    format!(
        "failed to exec {:?}: {err}",
        opts.program[0].to_string_lossy()
    )
}

/// Exit code for a run aborted as invalid (crash, deadlock, protocol
/// violation, or watchdog), matching the campaign-discard convention
/// (`chord-check` exit 3).
#[cfg(target_os = "linux")]
const DISCARD: i32 = 3;

/// Run the target under network simulation: host the seeded broker in this
/// process and spawn `opts.nodes` instances of the program, each with its own
/// node id. Returns the combined exit code (0 iff every node exited 0).
///
/// # Errors
///
/// Returns a message if the shim cannot be found, the net spec is invalid, the
/// broker cannot bind, or a child fails to spawn.
#[cfg(target_os = "linux")]
#[allow(clippy::too_many_lines)] // one linear orchestration flow; splitting obscures it
pub fn run_cluster(opts: &RunOpts) -> Result<i32, String> {
    let shim_path = find_shim(opts.shim.clone())?;
    let spec = opts.net.as_deref().unwrap_or("");
    let model = weft_net::config::parse(opts.seed, spec).map_err(|e| e.to_string())?;

    // Windowed mode needs lookahead (minimum latency) >= the window width, or a
    // blocking receiver's reactivation bound stalls its own delivery (the L=0
    // deadlock — docs/MULTI_HOST_CLOCK_PROTOCOL.md §5). Warn rather than abort:
    // pure-sink workloads with no request/reply can still make progress.
    if opts.window > 0 && model.min_delay_ns() < opts.window {
        eprintln!(
            "[weft] warning: --window {} ns exceeds the network's minimum latency {} ns; \
             windowed request/reply can deadlock (need lookahead >= window)",
            opts.window,
            model.min_delay_ns()
        );
    }

    // A per-run socket path (pid disambiguates concurrent weft runs).
    let sock_path = std::env::temp_dir().join(format!("weft-broker-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&sock_path);

    // With --record, every broker operation is captured in linearization
    // order for later `weft replay` (see docs/recording-format.md).
    let recorder = match &opts.record {
        Some(log_path) => {
            let header = weft_replay::Header {
                format: weft_replay::log::FORMAT.into(),
                version: weft_replay::log::VERSION,
                seed: opts.seed,
                net: spec.to_string(),
                // The windowed multi-host path records the real window width
                // so replay reconstructs the same sealed order; single-host
                // (window 0) keeps latency-only delivery.
                window_ns: opts.window,
                meta: weft_replay::log::Meta {
                    weft_version: Some(crate::version().to_string()),
                    ..weft_replay::log::Meta::default()
                },
            };
            Some(
                weft_replay::Recorder::create(log_path, &header, Vec::new())
                    .map_err(|e| format!("--record {}: {e}", log_path.display()))?,
            )
        }
        None => None,
    };
    let observer = recorder.as_ref().map(weft_replay::Recorder::observer);
    // Three broker arrangements: join a remote one (--broker: no broker here,
    // the hosting side owns recording and failure detection), host on TCP
    // (--listen: other hosts can join), or host on the Unix socket (default).
    // window 0 selects the single-host arrival-routed broker; a non-zero width
    // engages the windowed sequencer (docs/MULTI_HOST_CLOCK_PROTOCOL.md).
    let (broker, endpoint) = if let Some(addr) = &opts.broker {
        (None, std::ffi::OsString::from(addr.as_str()))
    } else {
        let broker = if let Some(addr) = &opts.listen {
            weft_net::Broker::bind_tcp_window(addr.as_str(), model, observer, opts.window)
                .map_err(|e| format!("broker bind {addr}: {e}"))?
        } else {
            weft_net::Broker::bind_with_window(&sock_path, model, observer, opts.window)
                .map_err(|e| format!("broker bind: {e}"))?
        };
        // Windowed sealing must wait for the whole cluster to say Hello, or
        // node startup order (OS scheduling) races the horizon past a late
        // joiner — on multi-host runs, past a whole slow host.
        if opts.window > 0 {
            broker.expect_nodes(opts.nodes);
            if opts.window_ops > 0 {
                broker.limit_window_ops(opts.window_ops);
            }
        }
        let broker = std::sync::Arc::new(broker);
        {
            let broker = std::sync::Arc::clone(&broker);
            std::thread::spawn(move || broker.run());
        }
        let endpoint = opts
            .listen
            .as_ref()
            .map_or_else(|| sock_path.clone().into_os_string(), Into::into);
        (Some(broker), endpoint)
    };

    let (spawn_lo, spawn_hi) = opts.spawn.unwrap_or((0, opts.nodes - 1));
    let mut children = Vec::new();
    for node in spawn_lo..=spawn_hi {
        let mut cmd = target_command(opts, &shim_path);
        cmd.env(weft_abi::ENV_BROKER, &endpoint)
            .env(weft_abi::ENV_NODE_ID, node.to_string())
            .env(weft_abi::ENV_NET, spec);
        if opts.window > 0 {
            cmd.env(weft_abi::ENV_WINDOW_NS, opts.window.to_string());
        }
        if opts.host_id > 0 {
            cmd.env(weft_abi::ENV_HOST_ID, opts.host_id.to_string());
        }
        let child = cmd
            .spawn()
            .map_err(|e| format!("failed to spawn node {node}: {e}"))?;
        children.push((node, child));
    }

    // A blocking join per child hangs forever if the windowed cluster
    // deadlocks, so poll instead and act on two signals: the deterministic F6
    // quiescence check (fires the instant no connected guest can make progress)
    // and, when `--watchdog` is set, a real-time no-progress guard for a guest
    // wedged in uninstrumented compute (F3). Both only ever abort-and-discard;
    // a run that completes is untouched, and the poll only reads broker atomics
    // so it never perturbs the recorded bytes.
    let watchdog = (opts.watchdog > 0).then(|| std::time::Duration::from_secs(opts.watchdog));
    let mut code = 0;
    let mut running = children;
    let mut last_sent = broker.as_ref().map_or(0, |b| b.stats().0);
    let mut last_progress = std::time::Instant::now();
    // F2 observability: sample per-node frontier lag while the run is live
    // (at exit every connection has closed, so a final snapshot is empty)
    // and keep the maxima for the --stats report. Real-time sampling, so the
    // numbers are indicative, not exact — fine for naming the laggard.
    let mut max_lags: std::collections::HashMap<(u32, u32), u64> = std::collections::HashMap::new();
    loop {
        let mut i = 0;
        while i < running.len() {
            match running[i].1.try_wait() {
                Ok(Some(status)) => {
                    let (node, _) = running.remove(i);
                    match status.code() {
                        Some(0) => {}
                        Some(c) => {
                            eprintln!("weft: node {node} exited with {c}");
                            code = c;
                        }
                        // Killed by a signal. In a windowed run that is a real
                        // crash mid-window (F1): the ordering the survivors see
                        // depends on when (real time) the crash landed, so the
                        // run is invalid. Non-windowed keeps the historical 128.
                        None if opts.window > 0 => {
                            eprintln!("weft: node {node} crashed (signal); run discarded");
                            code = DISCARD;
                        }
                        None => {
                            eprintln!("weft: node {node} exited with 128");
                            code = 128;
                        }
                    }
                }
                Ok(None) => i += 1,
                Err(e) => {
                    let (node, _) = running.remove(i);
                    eprintln!("weft: node {node} wait failed: {e}");
                    code = 1;
                }
            }
        }
        if running.is_empty() {
            // A --listen host serves remote nodes too: hold the broker open
            // until every expected node has joined and finished (or a failure
            // check below fires), not just until the local children exit.
            let remote_pending = opts.listen.is_some()
                && code != DISCARD
                && broker
                    .as_ref()
                    .is_some_and(|b| !b.cluster_drained(opts.nodes));
            if !remote_pending {
                break;
            }
        }
        // Failure detection lives with the broker; the joining side of a
        // multi-host run (--broker) defers to the hosting side's exit code.
        if let Some(b) = &broker {
            // F4/F5: a rejected op means the linearization is already corrupt.
            if b.violation().is_some() {
                break;
            }
            // F6: deterministic terminal-deadlock report (windowed mode only).
            if b.deadlock_check() {
                eprintln!(
                    "[weft] deadlock: every connected node is blocked with nothing \
                     in flight (windowed quiescence); run discarded"
                );
                code = DISCARD;
                break;
            }
            // F3: real-time no-progress watchdog. Its firing is nondeterministic
            // (a merely slow guest can trip it), so it only ever discards.
            if let Some(timeout) = watchdog {
                let sent = b.stats().0;
                if sent == last_sent {
                    if last_progress.elapsed() >= timeout {
                        eprintln!(
                            "[weft] watchdog: no broker progress for {}s (deadlock or \
                             wedged guest); run discarded",
                            opts.watchdog
                        );
                        code = DISCARD;
                        break;
                    }
                } else {
                    last_sent = sent;
                    last_progress = std::time::Instant::now();
                }
            }
            if opts.stats && opts.window > 0 {
                for (host, node, lag) in b.frontier_lags() {
                    let m = max_lags.entry((host, node)).or_insert(0);
                    *m = (*m).max(lag);
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // F4/F5: checked after the loop too — a violated run whose nodes then
    // exit 0 must still be discarded, not reported as a clean pass.
    if let Some(v) = broker.as_ref().and_then(|b| b.violation()) {
        eprintln!("[weft] protocol violation: {v}; run discarded");
        code = DISCARD;
    }
    // Reap any child still alive after an abort so it does not linger.
    for (_, mut child) in running {
        let _ = child.kill();
        let _ = child.wait();
    }
    if opts.stats {
        if let Some(b) = &broker {
            let (sent, dropped) = b.stats();
            eprintln!("[weft] network: {sent} datagram(s) sent, {dropped} dropped");
            if opts.window > 0 {
                // F2 observability: the largest |node local clock - broker
                // logical clock| seen across all ops that carried a clock.
                eprintln!("[weft] max clock skew: {} ns", b.max_skew_ns());
                // F2: which node's frontier trailed the pack the most —
                // the connection everyone else was waiting on (sampled at
                // 50ms during the run, so indicative rather than exact).
                let mut lags: Vec<_> = max_lags.iter().collect();
                lags.sort_unstable();
                for ((host, node), lag) in lags {
                    eprintln!("[weft] frontier lag host {host} node {node}: max {lag} ns");
                }
            }
        } else {
            eprintln!("[weft] network stats live on the hosting side (--listen)");
        }
    }
    if let Some(rec) = recorder {
        match rec.finish() {
            Ok(_) => {
                if let Some(p) = &opts.record {
                    eprintln!(
                        "[weft] recorded to {} (verify: weft replay {})",
                        p.display(),
                        p.display()
                    );
                }
            }
            Err(e) => eprintln!("weft: recording incomplete: {e}"),
        }
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
        let opts = parse_args(os(&["--seed", "0x2a", "--trace", "--", "prog", "-x", "1"])).unwrap();
        assert_eq!(opts.seed, 42);
        assert!(opts.trace);
        assert_eq!(opts.program, os(&["prog", "-x", "1"]));
    }

    #[test]
    fn rejects_missing_seed_and_missing_program() {
        assert!(parse_args(os(&["--", "prog"]))
            .unwrap_err()
            .contains("--seed is required"));
        assert!(parse_args(os(&["--seed", "1"]))
            .unwrap_err()
            .contains("no program"));
        assert!(parse_args(os(&["--seed", "zzz", "--", "p"])).is_err());
    }

    #[test]
    fn parses_scheduler_flags() {
        let opts = parse_args(os(&[
            "--seed",
            "1",
            "--strategy",
            "rr",
            "--no-sched",
            "--stats",
            "--",
            "p",
        ]))
        .unwrap();
        assert_eq!(opts.strategy, Strategy::RoundRobin);
        assert!(opts.no_sched);
        assert!(opts.stats);
        assert!(parse_args(os(&["--seed", "1", "--strategy", "bogus", "--", "p"])).is_err());
        // Strategy defaults to Random when unspecified.
        assert_eq!(
            parse_args(os(&["--seed", "1", "--", "p"]))
                .unwrap()
                .strategy,
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
