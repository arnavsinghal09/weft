//! Ground truth for the shrinker: three synthetic cases where the exact
//! minimal fault/schedule combination is known by construction, deliberately
//! buried inside a much longer generated log. The shrinker must converge to
//! the known minimum — not "small", the *known* sequence — before it is
//! allowed anywhere near real scenarios.
//!
//! Construction principle: the trigger ops live on their own channel and
//! their own connection, and every noise op stays off that channel and off
//! that queue. Removing noise therefore never shifts the trigger channel's
//! sequence numbers, so the true minimum is stable and exactly known.

use weft_abi::splitmix64;
use weft_net::{FaultModel, VAddr};
use weft_replay::invariant::{FnInvariant, Invariant, PerChannelFifo};
use weft_replay::log::{Event, RecvOutcome, SendOutcome};
use weft_replay::{replay_log, Log};

use weft_fuzz::input::{execute, execute_and_record, OpInput};
use weft_fuzz::shrink::{shrink, ViolationKey};

const SEED: u64 = 11;
const NET: &str = "latency=uniform:1000-100000";

fn addr(node: u32, port: u16) -> VAddr {
    VAddr::new(0x7f00_0001 + node, port)
}

/// Find a source port whose channel to `dst` inverts its first two
/// datagrams' delivery order under (SEED, NET): delay(seq 0) > delay(seq 1).
fn inverted_channel(dst: VAddr, src_node: u32) -> VAddr {
    let model = weft_net::config::parse(SEED, NET).unwrap();
    for port in 1000..u16::MAX {
        let src = addr(src_node, port);
        let f0 = model.fate(src, dst, 0, 1);
        let f1 = model.fate(src, dst, 1, 1);
        if !f0.dropped && !f1.dropped && f0.delay_ns > f1.delay_ns {
            return src;
        }
    }
    panic!("no inverted channel found (statistically impossible)");
}

/// Sanity helper used while building cases; not part of the shrinker.
#[allow(dead_code)]
fn assert_fifo_inverts(src: VAddr, dst: VAddr, model: &FaultModel) {
    assert!(model.fate(src, dst, 0, 1).delay_ns > model.fate(src, dst, 1, 1).delay_ns);
}

/// Deterministic noise: `count` ops across `noise_conns` connections, each
/// bound to its own port, sending only among themselves — never touching
/// `avoid_dst` or the trigger connection's queue.
fn noise_ops(count: usize, noise_conns: u64, mut rng: u64) -> Vec<OpInput> {
    let mut ops = Vec::new();
    let base_conn = 100u64; // far from trigger conns
    for c in 0..noise_conns {
        let conn = base_conn + c;
        #[allow(clippy::cast_possible_truncation)]
        let port = 20_000 + (c as u16) * 7;
        ops.push(OpInput::Connect { conn });
        ops.push(OpInput::Bind {
            conn,
            addr: addr(3, port),
        });
    }
    while ops.len() < count {
        let c = splitmix64(&mut rng) % noise_conns;
        let conn = base_conn + c;
        #[allow(clippy::cast_possible_truncation)]
        let port = 20_000 + (c as u16) * 7;
        if splitmix64(&mut rng) % 3 == 0 {
            ops.push(OpInput::Recv {
                conn,
                blocking: false,
            });
        } else {
            let payload = splitmix64(&mut rng).to_le_bytes().to_vec();
            ops.push(OpInput::Send {
                conn,
                src: addr(4, 500 + (splitmix64(&mut rng) % 90) as u16),
                dst: addr(3, port),
                payload,
            });
        }
    }
    ops
}

/// Bury `trigger` inside `noise`, preserving the trigger's relative order:
/// trigger op k goes after roughly k/(k_total) of the noise.
fn bury(trigger: &[OpInput], noise: Vec<OpInput>) -> Vec<OpInput> {
    let mut out = Vec::with_capacity(trigger.len() + noise.len());
    let stride = noise.len() / (trigger.len() + 1);
    let mut noise_iter = noise.into_iter();
    for t in trigger {
        for _ in 0..stride {
            if let Some(n) = noise_iter.next() {
                out.push(n);
            }
        }
        out.push(t.clone());
    }
    out.extend(noise_iter);
    out
}

fn key_for(
    seed: u64,
    net: &str,
    ops: &[OpInput],
    invs: impl Fn() -> Vec<Box<dyn Invariant>>,
    name: &str,
    subject_contains: &str,
) -> ViolationKey {
    let out = execute(seed, net, ops, invs()).unwrap();
    out.violations
        .iter()
        .map(ViolationKey::of)
        .find(|k| k.invariant == name && k.subject.contains(subject_contains))
        .expect("constructed trigger must fire")
}

// ---------------------------------------------------------------- Case A --

/// FIFO reorder: known minimum is exactly
/// `[Connect, Bind, Send(seq0), Send(seq1), Recv, Recv]` — the pair whose
/// fates invert, on an otherwise untouched channel.
#[test]
fn case_a_fifo_pair_buried_in_300_noise_ops() {
    let dst = addr(0, 100);
    let src = inverted_channel(dst, 1);
    let trigger = vec![
        OpInput::Connect { conn: 0 },
        OpInput::Bind { conn: 0, addr: dst },
        OpInput::Send {
            conn: 1,
            src,
            dst,
            payload: vec![0xa0],
        },
        OpInput::Send {
            conn: 1,
            src,
            dst,
            payload: vec![0xa1],
        },
        OpInput::Recv {
            conn: 0,
            blocking: false,
        },
        OpInput::Recv {
            conn: 0,
            blocking: false,
        },
    ];
    let buried = bury(&trigger, noise_ops(300, 4, 7));
    assert!(buried.len() > 300);

    let invs = || -> Vec<Box<dyn Invariant>> { vec![Box::new(PerChannelFifo::new())] };
    let target = key_for(
        SEED,
        NET,
        &buried,
        invs,
        "per-channel-fifo",
        &src.to_string(),
    );

    let (min_ops, stats) = shrink(SEED, NET, &buried, &invs, &target);
    assert_eq!(min_ops, trigger, "must converge to the exact known minimum");
    assert_eq!(stats.ops_after, 6);
    assert!(
        !stats.budget_exhausted,
        "took {} executions",
        stats.executions
    );

    // The reproducer must record to a log that replays clean and still fails.
    let path = std::env::temp_dir().join(format!("weft-gt-a-{}.weftlog", std::process::id()));
    let violations =
        execute_and_record(&path, SEED, NET, &min_ops, invs(), "ground-truth-a").unwrap();
    assert!(violations.iter().any(|v| ViolationKey::of(v) == target));
    let log = Log::read(&path).unwrap();
    let out = replay_log(&log, invs(), None).unwrap();
    assert!(out.identical);
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------- Case B --

/// Poison payload: a single delivery chain. Known minimum is exactly
/// `[Connect, Bind, Send(0xdd), Recv]`.
#[test]
fn case_b_poison_payload_buried_in_400_noise_ops() {
    let dst = addr(0, 130);
    let src = addr(1, 4242);
    let trigger = vec![
        OpInput::Connect { conn: 9 },
        OpInput::Bind { conn: 9, addr: dst },
        OpInput::Send {
            conn: 8,
            src,
            dst,
            payload: vec![0xdd],
        },
        OpInput::Recv {
            conn: 9,
            blocking: false,
        },
    ];
    let buried = bury(&trigger, noise_ops(400, 5, 21));

    let poison = || -> Vec<Box<dyn Invariant>> {
        vec![Box::new(FnInvariant::new(
            "no-poison-delivery",
            |_op, _vt, e: &Event| {
                if let Event::Recv {
                    outcome: RecvOutcome::Delivered { payload, .. },
                    ..
                } = e
                {
                    if payload == "dd" {
                        return Some("poison byte 0xdd was delivered".into());
                    }
                }
                None
            },
        ))]
    };
    let target = key_for(SEED, NET, &buried, poison, "no-poison-delivery", "");
    let (min_ops, stats) = shrink(SEED, NET, &buried, &poison, &target);
    assert_eq!(min_ops, trigger, "must converge to the exact known minimum");
    assert!(!stats.budget_exhausted);
    assert!(stats.executions < 4000);
}

// ---------------------------------------------------------------- Case C --

/// Two-fault combination: B overtakes A. The invariant fires only when A was
/// *sent* and B is *delivered first* — so both sends are load-bearing and
/// the known minimum is exactly `[Connect, Bind, Send(A), Send(B), Recv]`.
#[test]
fn case_c_two_fault_combination_buried_in_350_noise_ops() {
    let dst = addr(0, 160);
    let src = inverted_channel(dst, 2); // seq0 (A) slower than seq1 (B)
    let trigger = vec![
        OpInput::Connect { conn: 3 },
        OpInput::Bind { conn: 3, addr: dst },
        OpInput::Send {
            conn: 4,
            src,
            dst,
            payload: vec![0xaa],
        },
        OpInput::Send {
            conn: 4,
            src,
            dst,
            payload: vec![0xbb],
        },
        OpInput::Recv {
            conn: 3,
            blocking: false,
        },
    ];
    let buried = bury(&trigger, noise_ops(350, 4, 33));

    let overtake = || -> Vec<Box<dyn Invariant>> {
        // Stateful: remembers whether A was enqueued and not yet delivered.
        let mut a_sent = false;
        let mut a_delivered = false;
        vec![Box::new(FnInvariant::new(
            "b-overtakes-a",
            move |_op, _vt, e: &Event| {
                match e {
                    Event::Send {
                        payload,
                        outcome: SendOutcome::Enqueued { .. },
                        ..
                    } if payload == "aa" => {
                        a_sent = true;
                    }
                    Event::Recv {
                        outcome: RecvOutcome::Delivered { payload, .. },
                        ..
                    } => {
                        if payload == "aa" {
                            a_delivered = true;
                        } else if payload == "bb" && a_sent && !a_delivered {
                            return Some(
                                "B delivered while A (sent earlier) still in flight".into(),
                            );
                        }
                    }
                    _ => {}
                }
                None
            },
        ))]
    };
    let target = key_for(SEED, NET, &buried, overtake, "b-overtakes-a", "");
    let (min_ops, stats) = shrink(SEED, NET, &buried, &overtake, &target);
    assert_eq!(min_ops, trigger, "must converge to the exact known minimum");
    assert!(!stats.budget_exhausted);

    // Interpretability: the minimum keeps both faults visible — the send
    // that was overtaken AND the send that overtook it.
    let sends: Vec<_> = min_ops
        .iter()
        .filter(|o| matches!(o, OpInput::Send { .. }))
        .collect();
    assert_eq!(
        sends.len(),
        2,
        "a 'minimal' repro without both sends lost the story"
    );
}
