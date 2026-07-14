//! The fuzz loop: sweep fault seeds against a fixed workload and its
//! invariants, shrink the first occurrence of each distinct violation, and
//! produce a report a human (or CI) can act on directly.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use weft_replay::invariant::{Invariant, NoDuplicateDelivery, PerChannelFifo};

use crate::config::FuzzConfig;
use crate::gen::generate;
use crate::input::{execute, execute_and_record, OpInput};
use crate::shrink::{shrink, ShrinkStats, ViolationKey};

/// One distinct violation the sweep found.
#[derive(Clone, Debug)]
pub struct Found {
    pub key: ViolationKey,
    /// First seed that triggered it.
    pub seed: u64,
    /// The human-readable message from the first occurrence.
    pub message: String,
    /// Every seed (within the sweep) that triggered this key.
    pub seeds_hit: Vec<u64>,
    pub shrink: Option<ShrinkStats>,
    /// Where the (shrunk, or full if shrinking is off) reproducer log lives.
    pub log_path: Option<PathBuf>,
}

/// The sweep's outcome.
#[derive(Debug)]
pub struct FuzzReport {
    pub seeds_tested: u64,
    pub seeds_failed: u64,
    pub violations: Vec<Found>,
    pub elapsed: Duration,
    /// True when the time budget ended the sweep before `seed_count` seeds.
    pub budget_exhausted: bool,
}

pub(crate) fn invariant_set(names: &[String]) -> Vec<Box<dyn Invariant>> {
    let mut out: Vec<Box<dyn Invariant>> = Vec::new();
    for n in names {
        match n.as_str() {
            "fifo" | "per-channel-fifo" => out.push(Box::new(PerChannelFifo::new())),
            "dup" | "no-duplicate-delivery" => out.push(Box::new(NoDuplicateDelivery::new())),
            _ => unreachable!("config validation rejects unknown invariants"),
        }
    }
    out
}

fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-'); // collapse runs of separators into one dash
        }
    }
    out.trim_matches('-').to_string()
}

/// Run the sweep described by `cfg`, in two phases so the result is
/// deterministic (given no time budget) regardless of thread timing:
///
/// 1. **Sweep** (parallel): execute every seed, collect the set of failing
///    seeds per distinct [`ViolationKey`]. Regression seeds go first so a
///    tight time budget can never skip them.
/// 2. **Shrink** (parallel across keys): each distinct violation is shrunk
///    from its *smallest* failing seed — a stable representative — and the
///    reproducer log lands in `cfg.out_dir`.
///
/// # Errors
/// Setup errors (bad net, output dir not creatable), as a message.
///
/// # Panics
/// If an internal lock is poisoned, which cannot happen: no holder performs
/// a panicking operation.
#[allow(clippy::too_many_lines)] // two sequential phases; splitting obscures the flow
pub fn run_fuzz(cfg: &FuzzConfig) -> Result<FuzzReport, String> {
    /// First message + every failing seed, per distinct violation.
    type Hit = (String, Vec<u64>);

    let started = Instant::now();
    let deadline =
        (cfg.time_budget_secs > 0).then(|| started + Duration::from_secs(cfg.time_budget_secs));
    std::fs::create_dir_all(&cfg.out_dir)
        .map_err(|e| format!("cannot create {}: {e}", cfg.out_dir.display()))?;

    let ops = generate(&cfg.workload.resolve());
    let ops: &[OpInput] = &ops;
    let invariants = &cfg.invariants;

    // Phase 1 — sweep. Regressions first, then the range.
    let sweep = cfg.seed_start..cfg.seed_start.saturating_add(cfg.seed_count);
    let queue: Vec<u64> = cfg.regression_seeds.iter().copied().chain(sweep).collect();
    let next = AtomicUsize::new(0);
    let tested = AtomicU64::new(0);
    let failed = AtomicU64::new(0);
    let found: Mutex<HashMap<ViolationKey, Hit>> = Mutex::new(HashMap::new());
    let budget_hit = std::sync::atomic::AtomicBool::new(false);

    std::thread::scope(|s| {
        for _ in 0..cfg.jobs.max(1) {
            s.spawn(|| loop {
                if let Some(d) = deadline {
                    if Instant::now() >= d {
                        budget_hit.store(true, Ordering::Relaxed);
                        return;
                    }
                }
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(&seed) = queue.get(i) else { return };
                tested.fetch_add(1, Ordering::Relaxed);

                let Ok(out) = execute(seed, &cfg.net, ops, invariant_set(invariants)) else {
                    return; // net validated at load; unreachable
                };
                if out.violations.is_empty() {
                    continue;
                }
                failed.fetch_add(1, Ordering::Relaxed);
                let mut map = found.lock().unwrap();
                for v in &out.violations {
                    let entry = map
                        .entry(ViolationKey::of(v))
                        .or_insert_with(|| (v.message.clone(), Vec::new()));
                    if !entry.1.contains(&seed) {
                        entry.1.push(seed);
                    }
                }
            });
        }
    });

    // Phase 2 — shrink each distinct violation from its smallest seed.
    let mut keys: Vec<(ViolationKey, Hit)> = found.into_inner().unwrap().into_iter().collect();
    keys.sort_by_key(|a| a.0.to_string());
    let violations: Mutex<Vec<Found>> = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for (key, (message, mut seeds_hit)) in keys {
            let violations = &violations;
            s.spawn(move || {
                seeds_hit.sort_unstable();
                let seed = seeds_hit[0];
                let (min_ops, stats) = if cfg.shrink {
                    shrink(seed, &cfg.net, ops, &|| invariant_set(invariants), &key)
                } else {
                    (
                        ops.to_vec(),
                        ShrinkStats {
                            ops_before: ops.len(),
                            ops_after: ops.len(),
                            ..ShrinkStats::default()
                        },
                    )
                };
                let file = cfg.out_dir.join(format!(
                    "repro-seed{seed}-{}.weftlog",
                    sanitize(&key.to_string())
                ));
                let label = format!(
                    "shrunk reproducer: {key}, seed {seed}, {} → {} ops",
                    stats.ops_before, stats.ops_after
                );
                let write = execute_and_record(
                    &file,
                    seed,
                    &cfg.net,
                    &min_ops,
                    invariant_set(invariants),
                    &label,
                );
                violations.lock().unwrap().push(Found {
                    key,
                    seed,
                    message,
                    seeds_hit,
                    shrink: Some(stats),
                    log_path: write.is_ok().then_some(file),
                });
            });
        }
    });

    let mut violations = violations.into_inner().unwrap();
    violations.sort_by_key(|a| a.key.to_string());
    Ok(FuzzReport {
        seeds_tested: tested.load(Ordering::Relaxed),
        seeds_failed: failed.load(Ordering::Relaxed),
        violations,
        elapsed: started.elapsed(),
        budget_exhausted: budget_hit.load(Ordering::Relaxed),
    })
}

impl FuzzReport {
    /// Render the human/CI report.
    #[must_use]
    pub fn render(&self, cfg: &FuzzConfig) -> String {
        use std::fmt::Write as _;
        let mut o = String::new();
        let _ = writeln!(
            o,
            "==================== WEFT FUZZ REPORT ===================="
        );
        let secs = self.elapsed.as_secs_f64();
        let took = if secs < 1.0 {
            format!("{:.0}ms", secs * 1000.0)
        } else {
            format!("{secs:.1}s")
        };
        let _ = writeln!(
            o,
            "swept     : {} seed(s) in {took} ({} failing){}",
            self.seeds_tested,
            self.seeds_failed,
            if self.budget_exhausted {
                "  [time budget hit]"
            } else {
                ""
            },
        );
        let _ = writeln!(o, "net       : {}", cfg.net);
        let _ = writeln!(
            o,
            "workload  : {} node(s), {} send(s), workload_seed {}",
            cfg.workload.nodes, cfg.workload.sends, cfg.workload.workload_seed
        );
        let _ = writeln!(o, "invariants: {}", cfg.invariants.join(", "));
        if self.violations.is_empty() {
            let _ = writeln!(o, "\nNo invariant violations found.");
        } else {
            let _ = writeln!(o, "\n{} distinct violation(s):", self.violations.len());
            for f in &self.violations {
                let _ = writeln!(o, "\n  ✗ {}", f.key);
                let _ = writeln!(o, "    what   : {}", f.message);
                let _ = writeln!(
                    o,
                    "    seeds  : {} of {} tested (first: {})",
                    f.seeds_hit.len(),
                    self.seeds_tested,
                    f.seed
                );
                if let Some(s) = &f.shrink {
                    let _ = writeln!(
                        o,
                        "    shrunk : {} → {} ops in {} execution(s){}",
                        s.ops_before,
                        s.ops_after,
                        s.executions,
                        if s.budget_exhausted {
                            " [budget hit]"
                        } else {
                            ""
                        }
                    );
                }
                if let Some(p) = &f.log_path {
                    let _ = writeln!(o, "    repro  : {}", p.display());
                    let _ = writeln!(
                        o,
                        "    verify : weft replay {} --check {}",
                        p.display(),
                        cfg.invariants.join(",")
                    );
                }
            }
        }
        let _ = writeln!(
            o,
            "=========================================================="
        );
        o
    }

    /// The regression file body: every failing seed, deduped and sorted.
    #[must_use]
    pub fn regression_seeds(&self) -> Vec<u64> {
        let mut seeds: Vec<u64> = self
            .violations
            .iter()
            .flat_map(|f| f.seeds_hit.iter().copied())
            .collect();
        seeds.sort_unstable();
        seeds.dedup();
        seeds
    }
}
