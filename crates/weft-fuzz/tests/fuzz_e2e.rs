//! End-to-end: the complete find → shrink → reproduce loop against a real
//! multi-fault scenario (latency variance + loss — the same combination the
//! kvreplica demo from Phase 3 exploits), driven purely through the public
//! config surface, exactly like `weft fuzz --config` does.

use weft_fuzz::{run_fuzz, FuzzConfig, ViolationKey};
use weft_replay::invariant::{NoDuplicateDelivery, PerChannelFifo};
use weft_replay::{replay_log, Log};

fn out_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("weft-fuzz-e2e-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn config(name: &str) -> FuzzConfig {
    let mut cfg = FuzzConfig::from_json(
        r#"{
            "net": "latency=uniform:0-8000,loss=0.02",
            "seed_start": 0,
            "seed_count": 60,
            "jobs": 4,
            "invariants": ["fifo", "dup"],
            "workload": { "nodes": 3, "sends": 30, "payload_len": 4 }
        }"#,
    )
    .unwrap();
    cfg.out_dir = out_dir(name);
    cfg
}

#[test]
fn full_loop_finds_shrinks_and_reproduces() {
    let cfg = config("main");
    let report = run_fuzz(&cfg).unwrap();

    assert_eq!(report.seeds_tested, 60);
    assert!(
        !report.violations.is_empty(),
        "a 30-send burst under uniform 0-8000ns latency must reorder somewhere"
    );
    assert!(report.seeds_failed > 0);

    for f in &report.violations {
        // Every distinct violation carries a shrunk reproducer…
        let stats = f.shrink.expect("shrinking was enabled");
        assert!(
            stats.ops_after < stats.ops_before,
            "{}: no reduction ({} ops)",
            f.key,
            stats.ops_after
        );
        // …whose log replays byte-identically and still fails the same way.
        let path = f.log_path.as_ref().expect("reproducer written");
        let log = Log::read(path).unwrap();
        let out = replay_log(
            &log,
            vec![
                Box::new(PerChannelFifo::new()),
                Box::new(NoDuplicateDelivery::new()),
            ],
            None,
        )
        .unwrap();
        assert!(out.identical, "{}: {:?}", f.key, out.divergence);
        assert!(
            out.violations.iter().any(|v| ViolationKey::of(v) == f.key),
            "{}: reproducer lost its violation",
            f.key
        );
        // Interpretability: the reproducer is drastically smaller but keeps
        // the full story (its sends and recvs on the violating channel).
        assert!(
            stats.ops_after >= 4,
            "cannot violate FIFO with fewer than 2 sends + 2 recvs"
        );
    }

    // The report names everything a human needs.
    let text = report.render(&cfg);
    assert!(text.contains("distinct violation"));
    assert!(text.contains("weft replay"));
    assert!(text.contains("shrunk"));
    assert!(!report.regression_seeds().is_empty());

    let _ = std::fs::remove_dir_all(&cfg.out_dir);
}

#[test]
fn fuzz_results_are_deterministic_across_runs() {
    let cfg1 = config("det1");
    let cfg2 = config("det2");
    let r1 = run_fuzz(&cfg1).unwrap();
    let r2 = run_fuzz(&cfg2).unwrap();

    let keys1: Vec<String> = r1.violations.iter().map(|f| f.key.to_string()).collect();
    let keys2: Vec<String> = r2.violations.iter().map(|f| f.key.to_string()).collect();
    assert_eq!(
        keys1, keys2,
        "distinct violations must not depend on thread timing"
    );
    for (a, b) in r1.violations.iter().zip(&r2.violations) {
        assert_eq!(a.seed, b.seed, "representative seed must be stable");
        assert_eq!(a.seeds_hit, b.seeds_hit);
        assert_eq!(
            a.shrink.unwrap().ops_after,
            b.shrink.unwrap().ops_after,
            "shrunk size must be stable"
        );
    }
    let _ = std::fs::remove_dir_all(&cfg1.out_dir);
    let _ = std::fs::remove_dir_all(&cfg2.out_dir);
}

#[test]
fn regression_seeds_are_checked_even_under_zero_budget_pressure() {
    // A budget too small to sweep anything still tests the regression seeds
    // (they are queued first).
    let mut cfg = config("regress");
    let baseline = run_fuzz(&cfg).unwrap();
    let regressions = baseline.regression_seeds();
    assert!(!regressions.is_empty());

    cfg.regression_seeds = regressions.clone();
    cfg.seed_count = 1_000_000; // absurd sweep that the budget will cut short
    cfg.time_budget_secs = 1;
    let rerun = run_fuzz(&cfg).unwrap();
    assert!(rerun.budget_exhausted || rerun.seeds_tested >= regressions.len() as u64);
    assert!(
        !rerun.violations.is_empty(),
        "regression seeds must re-detect their violations"
    );
    let _ = std::fs::remove_dir_all(&cfg.out_dir);
}
