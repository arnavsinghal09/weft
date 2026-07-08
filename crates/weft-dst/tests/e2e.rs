//! End-to-end determinism tests: compile the real C target programs in
//! `examples/`, launch them through the actual `weft run` CLI (so the whole
//! LD_PRELOAD + env plumbing is what's under test), and assert on bytes.
//!
//! Linux-only: interception requires LD_PRELOAD semantics.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

const EXAMPLES: &[&str] = &["chrono", "montecarlo", "entropy"];

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

/// Compile the C examples once per test-binary run.
fn built_examples() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let out = repo_root().join("target").join("e2e-examples");
        std::fs::create_dir_all(&out).unwrap();
        for name in EXAMPLES {
            let src = repo_root().join("examples").join(format!("{name}.c"));
            let dst = out.join(name);
            let status = Command::new("cc")
                .arg("-O2")
                .arg("-o")
                .arg(&dst)
                .arg(&src)
                .arg("-lpthread")
                .status()
                .expect("cc not found: the e2e suite needs a C compiler");
            assert!(status.success(), "failed to compile {name}.c");
        }
        out
    })
}

fn shim_path() -> PathBuf {
    // The shim cdylib lands next to the weft binary in the same profile dir.
    Path::new(weft_bin())
        .parent()
        .unwrap()
        .join("libweft_shim.so")
}

fn weft_run(seed: u64, extra: &[&str], example: &str) -> Output {
    let prog = built_examples().join(example);
    let out = Command::new(weft_bin())
        .arg("run")
        .arg("--seed")
        .arg(seed.to_string())
        .args(extra)
        .arg("--")
        .arg(&prog)
        .output()
        .expect("failed to spawn weft");
    assert!(
        out.status.success(),
        "weft run {example} (seed {seed}) failed: status={:?}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

#[test]
fn shim_library_was_built() {
    assert!(
        shim_path().is_file(),
        "libweft_shim.so not found at {} — build the workspace first",
        shim_path().display()
    );
}

#[test]
fn same_seed_means_identical_output() {
    for example in EXAMPLES {
        let a = weft_run(42, &[], example);
        let b = weft_run(42, &[], example);
        assert_eq!(
            a.stdout,
            b.stdout,
            "{example}: same seed produced different stdout:\n--- run A\n{}\n--- run B\n{}",
            String::from_utf8_lossy(&a.stdout),
            String::from_utf8_lossy(&b.stdout)
        );
    }
}

#[test]
fn different_seeds_mean_different_output() {
    for example in EXAMPLES {
        let a = weft_run(1, &[], example);
        let b = weft_run(2, &[], example);
        assert_ne!(
            a.stdout, b.stdout,
            "{example}: different seeds produced identical stdout — seed is not flowing"
        );
    }
}

#[test]
fn many_seeds_all_self_consistent() {
    // Cheap adversarial sweep on the tight-loop program.
    for seed in [0, 1, u64::MAX, 0xDEAD_BEEF] {
        let a = weft_run(seed, &[], "montecarlo");
        let b = weft_run(seed, &[], "montecarlo");
        assert_eq!(a.stdout, b.stdout, "seed {seed} not reproducible");
    }
}

#[test]
fn preloaded_but_unseeded_shim_is_invisible() {
    // The do-no-harm rule: LD_PRELOAD set, WEFT_SEED absent — the program
    // must run normally (and nondeterministically).
    let prog = built_examples().join("chrono");
    let run = || {
        let out = Command::new(&prog)
            .env("LD_PRELOAD", shim_path())
            .env_remove("WEFT_SEED")
            .output()
            .unwrap();
        assert!(out.status.success(), "passthrough run failed");
        out.stdout
    };
    let a = run();
    // Real time moved between runs, so output should differ (sanity check
    // that we're genuinely on the passthrough path, not the virtual clock).
    let b = run();
    assert_ne!(
        a, b,
        "unseeded runs were identical — passthrough suspicious"
    );
}

#[test]
fn trace_flag_reports_interceptions() {
    let out = weft_run(7, &["--trace"], "chrono");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("[weft] shim active, seed=7"),
        "missing activation banner in stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("clock_gettime") || stderr.contains("time()"),
        "missing per-call trace lines in stderr:\n{stderr}"
    );
}

#[test]
fn seed_zero_and_seed_max_are_valid() {
    for seed in [0, u64::MAX] {
        let out = weft_run(seed, &[], "entropy");
        assert!(!out.stdout.is_empty());
    }
}
