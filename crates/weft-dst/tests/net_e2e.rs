//! End-to-end tests for network simulation (Phase 3): drive the real
//! `weft run --net` CLI over the example programs and assert on cross-process
//! messaging, reproducibility, and the reordering-bug proof.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const PROGRAMS: &[&str] = &["pingpong", "kvreplica", "netsched"];

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
    let mut child = Command::new(weft_bin())
        .arg("run")
        .args(["--seed", &seed.to_string()])
        .args(["--net", net])
        .args(["--nodes", &nodes.to_string()])
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
