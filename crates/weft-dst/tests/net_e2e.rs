//! End-to-end tests for network simulation (Phase 3): drive the real
//! `weft run --net` CLI over the example programs and assert on cross-process
//! messaging, reproducibility, and the reordering-bug proof.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const PROGRAMS: &[&str] = &[
    "pingpong",
    "kvreplica",
    "netsched",
    "deadlock_recv",
    "crash_now",
];

/// The seeded network spec used by the kvreplica proof (documented in
/// docs/network-model.md; the trigger/avoid seeds below belong to it).
const KV_NET: &str = "latency=uniform:1000-50000";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn weft_bin() -> &'static str {
    env!("CARGO_BIN_EXE_weft")
}

fn shim_path() -> PathBuf {
    Path::new(weft_bin())
        .parent()
        .unwrap()
        .join("libweft_shim.so")
}

fn built() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let out = repo_root().join("target").join("net-examples");
        std::fs::create_dir_all(&out).unwrap();
        for name in PROGRAMS {
            let src = repo_root().join("examples").join(format!("{name}.c"));
            let status = Command::new("cc")
                .args(["-O2", "-o"])
                .arg(out.join(name))
                .arg(&src)
                .arg("-lpthread")
                .status()
                .expect("cc not found: the net e2e suite needs a C compiler");
            assert!(status.success(), "failed to compile {name}.c");
        }
        out
    })
}

/// Run under `weft run --net`, killing the whole run after 30s so a
/// simulation bug can never wedge the suite. Returns (stdout, exit code).
fn weft_net_run(seed: u64, net: &str, nodes: u32, program: &str) -> (String, i32) {
    weft_net_run_window(seed, net, nodes, 0, program)
}

/// [`weft_net_run`] with a windowed multi-host sequencer of width `window` ns
/// (0 = single-host).
fn weft_net_run_window(
    seed: u64,
    net: &str,
    nodes: u32,
    window: u64,
    program: &str,
) -> (String, i32) {
    let mut cmd = Command::new(weft_bin());
    cmd.arg("run")
        .args(["--seed", &seed.to_string()])
        .args(["--net", net])
        .args(["--nodes", &nodes.to_string()]);
    if window > 0 {
        cmd.args(["--window", &window.to_string()]);
    }
    let mut child = cmd
        .arg("--shim")
        .arg(shim_path())
        .arg("--")
        .arg(built().join(program))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn weft");
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(st) = child.try_wait().expect("wait failed") {
            break st;
        }
        assert!(
            Instant::now() < deadline,
            "weft run --net timed out (seed {seed}, {program}); killing"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut out = String::new();
    std::io::Read::read_to_string(child.stdout.as_mut().unwrap(), &mut out).unwrap();
    (out, status.code().unwrap_or(-1))
}

#[test]
fn two_processes_exchange_messages_deterministically() {
    // The two nodes share stdout, and *which node's line lands first* is OS
    // process scheduling — exactly the cross-process interleaving Phase 3
    // does not control (see docs/network-model.md). Message *content* is
    // fully seed-determined, so compare the line multiset, not the order.
    let sorted = |s: &str| {
        let mut v: Vec<&str> = s.lines().collect();
        v.sort_unstable();
        v.join("\n")
    };
    let (first, code) = weft_net_run(42, "", 2, "pingpong");
    assert_eq!(code, 0, "pingpong failed:\n{first}");
    assert!(
        first.contains("PING:") && first.contains("PONG:"),
        "bad output: {first}"
    );
    // The payload is seed-deterministic across repeated runs...
    for _ in 0..5 {
        let (again, code) = weft_net_run(42, "", 2, "pingpong");
        assert_eq!(code, 0);
        assert_eq!(
            sorted(&first),
            sorted(&again),
            "same seed changed the payload"
        );
    }
    // ...and different for a different seed.
    let (other, _) = weft_net_run(7, "", 2, "pingpong");
    assert_ne!(sorted(&first), sorted(&other));
}

/// The windowed multi-host sequencer keeps a request/reply exchange live and
/// deterministic. With lookahead (minimum latency) == window width, the reply
/// is admissible after the request's window seals; the seed-derived payload is
/// identical across runs and differs by seed. (A window wider than the
/// lookahead would risk the L=0 deadlock — see the run_cmd warning.)
#[test]
fn windowed_multihost_pingpong_is_live_and_deterministic() {
    let sorted = |s: &str| {
        let mut v: Vec<&str> = s.lines().collect();
        v.sort_unstable();
        v.join("\n")
    };
    let net = "latency=fixed:1000000";
    let (first, code) = weft_net_run_window(42, net, 2, 1_000_000, "pingpong");
    assert_eq!(code, 0, "windowed pingpong did not complete:\n{first}");
    assert!(
        first.contains("PING:") && first.contains("PONG:"),
        "bad output: {first}"
    );
    for _ in 0..5 {
        let (again, code) = weft_net_run_window(42, net, 2, 1_000_000, "pingpong");
        assert_eq!(code, 0);
        assert_eq!(
            sorted(&first),
            sorted(&again),
            "windowed run not deterministic"
        );
    }
    let (other, _) = weft_net_run_window(7, net, 2, 1_000_000, "pingpong");
    assert_ne!(
        sorted(&first),
        sorted(&other),
        "different seed should differ"
    );
}

/// Same seed, same windowed run ⇒ the *recorded send sequence* (the sealed
/// linearization, the input every delivery is derived from) is identical.
/// Whole-log byte identity is NOT claimed: setup ops (hello/bind) and recv
/// events are written in lock-arrival order, which is real time — see
/// LIMITATIONS.md. Sends, though, are recorded at seal time in virtual-time
/// order, so their sequence must never differ.
#[test]
fn windowed_recording_send_order_is_identical_across_runs() {
    let record_run = |path: &std::path::Path| {
        let mut child = Command::new(weft_bin())
            .arg("run")
            .args(["--seed", "42"])
            .args(["--net", "latency=fixed:1000000"])
            .args(["--nodes", "2"])
            .args(["--window", "1000000"])
            .arg("--record")
            .arg(path)
            .arg("--shim")
            .arg(shim_path())
            .arg("--")
            .arg(built().join("pingpong"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn weft");
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(st) = child.try_wait().expect("wait failed") {
                assert!(st.success(), "recorded windowed run failed");
                break;
            }
            assert!(Instant::now() < deadline, "recorded windowed run timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    // The volatile fields (`op` counter, integrity `chain`) depend on how many
    // arrival-ordered entries interleave, and broker `conn`/`to_conn` ids are
    // accept-order aliases (real time) — a node's stable identity is its
    // address, which every send carries as src/dst. Compare each send's event
    // body with the alias fields stripped, in order.
    let strip_key = |s: &str, key: &str| -> String {
        let mut out = s.to_string();
        let pat = format!("\"{key}\":");
        while let Some(i) = out.find(&pat) {
            let rest = &out[i + pat.len()..];
            let n = rest.chars().take_while(char::is_ascii_digit).count();
            let comma = usize::from(rest[n..].starts_with(','));
            out.replace_range(i..i + pat.len() + n + comma, "");
        }
        out
    };
    let send_seq = |path: &std::path::Path| -> Vec<String> {
        let text = std::fs::read_to_string(path).unwrap();
        text.lines()
            .filter(|l| l.contains("\"k\":\"send\""))
            .map(|l| {
                let start = l.find("\"e\":").expect("entry has no event field");
                let end = l.rfind(",\"chain\"").expect("entry has no chain field");
                strip_key(&strip_key(&l[start..end], "to_conn"), "conn")
            })
            .collect()
    };
    let dir = std::env::temp_dir();
    let (a, b) = (
        dir.join(format!("weft-recdet-a-{}.log", std::process::id())),
        dir.join(format!("weft-recdet-b-{}.log", std::process::id())),
    );
    record_run(&a);
    record_run(&b);
    let (sa, sb) = (send_seq(&a), send_seq(&b));
    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
    assert!(!sa.is_empty(), "recording contains no sends");
    assert_eq!(
        sa, sb,
        "same-seed windowed runs recorded different send sequences"
    );
}

/// Two `weft run` processes, one hosting the broker on TCP (`--listen`,
/// spawning node 0) and one joining it (`--broker`, spawning node 1) — the
/// exact shape of a two-host run, minus the second machine. Both halves must
/// complete, and the exchange must be deterministic across repeats and
/// windowed-sealed exactly like the single-orchestrator run.
#[test]
#[allow(clippy::zombie_processes)] // reaped via try_wait on success; on an assert failure the runs finish and exit on their own
fn split_orchestration_over_tcp_is_live_and_deterministic() {
    let addr = "127.0.0.1:17641";
    let spawn_half = |spawn: &str, host: bool| {
        let mut cmd = Command::new(weft_bin());
        cmd.arg("run")
            .args(["--seed", "42"])
            .args(["--net", "latency=fixed:1000000"])
            .args(["--nodes", "2"])
            .args(["--window", "1000000"])
            .args(["--spawn", spawn]);
        if host {
            cmd.args(["--listen", addr]);
        } else {
            cmd.args(["--broker", addr]);
        }
        cmd.arg("--shim")
            .arg(shim_path())
            .arg("--")
            .arg(built().join("pingpong"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn weft")
    };
    let run_split = || {
        let mut a = spawn_half("0-0", true);
        // The joining side's shim connects immediately; wait for the listener.
        let start = Instant::now();
        while std::net::TcpStream::connect(addr).is_err() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "--listen broker never came up"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut b = spawn_half("1-1", false);
        let deadline = Instant::now() + Duration::from_secs(30);
        let (mut sa, mut sb) = (None, None);
        while sa.is_none() || sb.is_none() {
            assert!(Instant::now() < deadline, "split run timed out");
            if sa.is_none() {
                sa = a.try_wait().expect("wait failed");
            }
            if sb.is_none() {
                sb = b.try_wait().expect("wait failed");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut out = String::new();
        std::io::Read::read_to_string(a.stdout.as_mut().unwrap(), &mut out).unwrap();
        let mut out_b = String::new();
        std::io::Read::read_to_string(b.stdout.as_mut().unwrap(), &mut out_b).unwrap();
        out.push_str(&out_b);
        assert_eq!(sa.unwrap().code(), Some(0), "hosting side failed: {out}");
        assert_eq!(sb.unwrap().code(), Some(0), "joining side failed: {out}");
        let mut lines: Vec<&str> = out.lines().collect();
        lines.sort_unstable();
        lines.join("\n")
    };
    let first = run_split();
    assert!(
        first.contains("PING:") && first.contains("PONG:"),
        "bad output: {first}"
    );
    for _ in 0..2 {
        assert_eq!(first, run_split(), "split TCP run not deterministic");
    }
}

/// A windowed cluster that cannot make progress — here a lone node blocked in
/// `recvfrom` with no peer — must be detected as a terminal deadlock and
/// discarded (exit 3, the deterministic F6 quiescence report,
/// docs/MULTI_HOST_CLOCK_PROTOCOL.md §8) rather than hang. Without the check
/// the run never terminates and the harness's 30s guard would fire the test.
#[test]
fn windowed_deadlock_is_detected_and_discarded() {
    let (_out, code) = weft_net_run_window(0, "latency=fixed:100", 1, 100, "deadlock_recv");
    assert_eq!(
        code, 3,
        "windowed deadlock must discard (exit 3), got {code}"
    );
}

/// A node killed by a signal mid-run is a real crash (F1): the windowed run is
/// invalid (the ordering survivors see depends on when, in real time, the
/// crash landed) and must be discarded, not reported as pass/fail.
#[test]
fn windowed_crash_by_signal_is_discarded() {
    let (_out, code) = weft_net_run_window(0, "latency=fixed:100", 1, 100, "crash_now");
    assert_eq!(code, 3, "signal crash must discard (exit 3), got {code}");
    // Non-windowed keeps the historical signal-exit mapping (128), so crashes
    // stay visible-but-ordinary where arrival order was never deterministic.
    let (_out, code) = weft_net_run(0, "", 1, "crash_now");
    assert_eq!(code, 128, "non-windowed signal exit must stay 128");
}

/// The Phase 3 bug proof: under seeded latency variance the replica's missing
/// version check yields a stale read for one seed and a correct read for
/// another — each reliable across 20 runs. The stale read still *requires*
/// latency variance (see [`reliable_network_never_reorders`]); these two
/// seeds just demonstrate both outcomes under the deterministic scheduler.
#[test]
fn reordering_bug_is_triggered_and_avoided_deterministically() {
    for _ in 0..20 {
        let (out, code) = weft_net_run(1, KV_NET, 1, "kvreplica");
        assert_eq!(out, "final=2 expected=8 stale=1\n", "seed 1 must reorder");
        assert_ne!(code, 0, "stale read must fail the run");

        let (out, code) = weft_net_run(6, KV_NET, 1, "kvreplica");
        assert_eq!(
            out, "final=8 expected=8 stale=0\n",
            "seed 6 must stay in order"
        );
        assert_eq!(code, 0);
    }
}

/// A reliable network never triggers the bug, whatever the seed: the defect
/// needs reordering, and a zero-variance network can't reorder.
#[test]
fn reliable_network_never_reorders() {
    for seed in [0, 1, 9, 42] {
        let (out, code) = weft_net_run(seed, "", 1, "kvreplica");
        assert_eq!(out, "final=8 expected=8 stale=0\n");
        assert_eq!(code, 0);
    }
}

/// Exponential latency (heavy tail) also reproduces per seed — the second
/// distribution demanded by the phase, exercised end-to-end.
#[test]
fn exponential_latency_is_reproducible_per_seed() {
    for seed in 0..6 {
        let (first, _) = weft_net_run(seed, "latency=exp:20000", 1, "kvreplica");
        let (again, _) = weft_net_run(seed, "latency=exp:20000", 1, "kvreplica");
        assert_eq!(
            first, again,
            "seed {seed} not reproducible under exp latency"
        );
    }
}

/// Like [`weft_net_run`] but injects extra environment variables into the
/// child (used to feed `netsched` its real-jitter knob) and a per-run
/// `NETSCHED_READY` handshake file so node 1 never sends before node 0 binds.
/// Same 30s watchdog.
fn weft_net_run_env(seed: u64, program: &str, envs: &[(&str, &str)]) -> (String, i32) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let ready = std::env::temp_dir().join(format!(
        "weft_netsched_ready_{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&ready);
    let mut cmd = Command::new(weft_bin());
    cmd.arg("run")
        .args(["--seed", &seed.to_string()])
        .args(["--net", ""])
        .args(["--nodes", "2"])
        .arg("--shim")
        .arg(shim_path())
        .arg("--")
        .arg(built().join(program))
        .env("NETSCHED_READY", &ready)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("failed to spawn weft");
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(st) = child.try_wait().expect("wait failed") {
            break st;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = std::fs::remove_file(&ready);
            panic!("weft run --net timed out (seed {seed}, {program}); killed");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut out = String::new();
    std::io::Read::read_to_string(child.stdout.as_mut().unwrap(), &mut out).unwrap();
    let _ = std::fs::remove_file(&ready);
    (out, status.code().unwrap_or(-1))
}

/// OQ-5 regression (the entropy-free-network-waiting guarantee): waiting on
/// the network must consume no scheduler entropy, so REAL timing jitter on
/// the peer — a busy-spin inserted between its sends — must not shift the
/// receiver's thread interleaving. The receiver's `order=` line is a pure
/// function of its scheduler decisions; if a poll ever drew RNG per real
/// poll (the old `yield_now` behavior) the spin would change it.
#[test]
fn net_wait_consumes_no_scheduler_entropy() {
    for seed in [0u64, 7, 42] {
        let (calm, code) = weft_net_run_env(seed, "netsched", &[("NETSCHED_SPIN", "0")]);
        assert_eq!(code, 0, "netsched (calm) failed:\n{calm}");
        assert!(calm.contains("order="), "bad output: {calm}");

        let (jittered, code) =
            weft_net_run_env(seed, "netsched", &[("NETSCHED_SPIN", "100000000")]);
        assert_eq!(code, 0, "netsched (jittered) failed:\n{jittered}");
        assert_eq!(
            calm, jittered,
            "seed {seed}: real peer timing changed the receiver's schedule"
        );

        let (again, _) = weft_net_run_env(seed, "netsched", &[("NETSCHED_SPIN", "0")]);
        assert_eq!(calm, again, "seed {seed}: same seed, different schedule");
    }
}
