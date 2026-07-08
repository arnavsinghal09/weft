//! End-to-end tests for the deterministic scheduler (Phase 2): compile the
//! concurrency example programs, run them through the real `weft run` CLI, and
//! assert on reproducibility, seed sensitivity, the race proof, and deadlock
//! detection.
//!
//! Linux-only: scheduling relies on `LD_PRELOAD` interposition of pthread.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

const PROGRAMS: &[&str] = &["race_bank", "prodcons", "thread_churn", "deadlock"];

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
        let out = repo_root().join("target").join("sched-examples");
        std::fs::create_dir_all(&out).unwrap();
        for name in PROGRAMS {
            let src = repo_root().join("examples").join(format!("{name}.c"));
            let dst = out.join(name);
            let status = Command::new("cc")
                .args(["-O2", "-o"])
                .arg(&dst)
                .arg(&src)
                .arg("-lpthread")
                .status()
                .expect("cc not found: the scheduler e2e suite needs a C compiler");
            assert!(status.success(), "failed to compile {name}.c");
        }
        out
    })
}

/// Run `weft run --seed <seed> [extra] -- <program> [args]`, returning the
/// full `Output` (does not assert on exit status — some tests expect failure).
fn weft_run(seed: u64, extra: &[&str], program: &str, args: &[&str]) -> Output {
    let prog = built().join(program);
    Command::new(weft_bin())
        .arg("run")
        .args(["--seed", &seed.to_string()])
        .args(["--shim"])
        .arg(shim_path())
        .args(extra)
        .arg("--")
        .arg(&prog)
        .args(args)
        .output()
        .expect("failed to spawn weft")
}

fn stdout_of(seed: u64, extra: &[&str], program: &str, args: &[&str]) -> String {
    let out = weft_run(seed, extra, program, args);
    assert!(
        out.status.success(),
        "{program} seed {seed} failed: {:?}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// Capture stdout without asserting on exit status. `race_bank` deliberately
/// returns non-zero when it observes a lost update, so tests that expect (or
/// tolerate) the race use this instead of [`stdout_of`].
fn stdout_lenient(seed: u64, extra: &[&str], program: &str, args: &[&str]) -> String {
    String::from_utf8(weft_run(seed, extra, program, args).stdout).unwrap()
}

#[test]
fn shim_library_exists() {
    assert!(shim_path().is_file(), "build the workspace first");
}

/// The scheduler is deterministic: the same seed reproduces a run exactly,
/// across every concurrency example, over many repeats.
#[test]
fn same_seed_is_byte_identical() {
    let cases: &[(&str, &[&str])] = &[
        ("race_bank", &["4", "25"]),
        ("prodcons", &["2", "2", "20"]),
        ("thread_churn", &["12"]),
    ];
    for (prog, args) in cases {
        let first = stdout_lenient(9, &[], prog, args);
        for _ in 0..12 {
            assert_eq!(
                first,
                stdout_lenient(9, &[], prog, args),
                "{prog} not reproducible"
            );
        }
    }
}

/// Different seeds explore different interleavings. `thread_churn` reports an
/// interleaving-sensitive statistic (`trylock_wins`), so its output must vary
/// across at least some seeds.
#[test]
fn different_seeds_explore_different_interleavings() {
    let outputs: Vec<String> = (0..12)
        .map(|s| stdout_of(s, &[], "thread_churn", &["16"]))
        .collect();
    let distinct = outputs
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len();
    assert!(
        distinct > 1,
        "seed had no effect on interleaving: {outputs:?}"
    );
}

/// The race proof: a specific seed reliably triggers the lost-update race, and
/// a specific seed reliably avoids it — each identical across 20 runs. (Seeds
/// found by the scan documented in docs/scheduling-model.md.)
#[test]
fn race_is_triggered_and_avoided_deterministically() {
    // 2 threads x 2 iters is the delicate configuration where the outcome is
    // a genuine coin-flip on the interleaving.
    let trigger = "threads=2 iters=2 expected=4 balance=2 lost=2\n";
    let avoid = "threads=2 iters=2 expected=4 balance=4 lost=0\n";
    for _ in 0..20 {
        assert_eq!(
            stdout_lenient(3, &[], "race_bank", &["2", "2"]),
            trigger,
            "seed 3 must trigger"
        );
        assert_eq!(
            stdout_of(2, &[], "race_bank", &["2", "2"]),
            avoid,
            "seed 2 must avoid"
        );
    }
    // The buggy program returns non-zero when it loses an update.
    assert!(!weft_run(3, &[], "race_bank", &["2", "2"]).status.success());
    assert!(weft_run(2, &[], "race_bank", &["2", "2"]).status.success());
}

/// At scale the race is pervasive: every seed loses updates, deterministically.
#[test]
fn race_is_pervasive_at_scale() {
    for seed in [1, 7, 42] {
        let first = stdout_lenient(seed, &[], "race_bank", &["4", "25"]);
        assert!(first.contains("balance=") && !first.contains("lost=0\n"));
        assert_eq!(first, stdout_lenient(seed, &[], "race_bank", &["4", "25"]));
    }
}

/// Producer/consumer over condition variables runs correctly (every produced
/// item is consumed) and deterministically under the scheduler.
#[test]
fn condvar_producer_consumer_is_correct_and_deterministic() {
    let out = stdout_of(5, &[], "prodcons", &["3", "2", "15"]);
    assert!(
        out.contains("match=1"),
        "producer/consumer lost items: {out}"
    );
    assert_eq!(out, stdout_of(5, &[], "prodcons", &["3", "2", "15"]));
}

/// Both scheduler strategies are deterministic.
#[test]
fn both_strategies_are_deterministic() {
    for strat in ["random", "rr"] {
        let a = stdout_of(4, &["--strategy", strat], "thread_churn", &["12"]);
        let b = stdout_of(4, &["--strategy", strat], "thread_churn", &["12"]);
        assert_eq!(a, b, "strategy {strat} not deterministic");
    }
}

/// Deadlock detection: an ABBA lock-ordering deadlock is caught (not hung) for
/// a triggering seed, and a non-triggering seed completes cleanly — both
/// reproducibly.
#[test]
fn deadlock_is_detected_deterministically() {
    // Seeds from the documented scan: 1 deadlocks, 0 completes.
    let dl = weft_run(1, &[], "deadlock", &[]);
    assert!(
        !dl.status.success(),
        "seed 1 should deadlock (non-zero exit)"
    );
    assert!(
        String::from_utf8_lossy(&dl.stderr).contains("DEADLOCK"),
        "expected a DEADLOCK diagnostic on stderr, got:\n{}",
        String::from_utf8_lossy(&dl.stderr)
    );

    let ok = weft_run(0, &[], "deadlock", &[]);
    assert!(ok.status.success(), "seed 0 should complete cleanly");
    assert!(String::from_utf8_lossy(&ok.stdout).contains("completed"));
}

/// The `--stats` flag reports scheduler coverage (thread count, decisions,
/// distinct yield-point sites).
#[test]
fn stats_report_yield_point_coverage() {
    let out = weft_run(1, &["--stats"], "thread_churn", &["8"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("scheduler:") && stderr.contains("yield-point site"),
        "missing scheduler stats:\n{stderr}"
    );
}
