//! Op inputs and the deterministic executor.
//!
//! A recorded log's outcomes (`chan_seq`, fates, ties, `vt`) are *derived*
//! data — deleting records from the log text just breaks the chain. So the
//! shrinker and the fuzzer both operate on [`OpInput`]s: events stripped to
//! the parts a client actually chose (who connected, what was bound, which
//! bytes were sent where, when a poll happened). [`execute`] re-runs an input
//! sequence through the same [`weft_net::core::Core`] the live broker and
//! the replayer use, recomputing every outcome; [`execute_and_record`] writes
//! the result back out as a fresh, fully consistent `weft-log` that `weft
//! replay` verifies like any recording.

use std::path::Path;

use weft_net::core::{Core, RecvResult, SendResult};
use weft_net::VAddr;
use weft_replay::hash::to_hex;
use weft_replay::invariant::{Invariant, Monitor, ViolationRecord};
use weft_replay::log::{Event, Header, LogWriter, Meta, RecvOutcome, SendOutcome, FORMAT, VERSION};
use weft_replay::Log;

/// The client-chosen part of one broker operation.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum OpInput {
    Connect {
        conn: u64,
    },
    Hello {
        conn: u64,
        node: u32,
    },
    Bind {
        conn: u64,
        addr: VAddr,
    },
    Send {
        conn: u64,
        src: VAddr,
        dst: VAddr,
        payload: Vec<u8>,
    },
    /// A receive poll. Execution always treats it as a non-blocking pop —
    /// the same linearization a blocking recv has at its success point.
    Recv {
        conn: u64,
        blocking: bool,
    },
    Disconnect {
        conn: u64,
    },
}

impl OpInput {
    /// The connection this op belongs to.
    #[must_use]
    pub fn conn(&self) -> u64 {
        match self {
            Self::Connect { conn }
            | Self::Hello { conn, .. }
            | Self::Bind { conn, .. }
            | Self::Send { conn, .. }
            | Self::Recv { conn, .. }
            | Self::Disconnect { conn } => *conn,
        }
    }

    /// Strip a verified log down to its input sequence (violation and end
    /// records carry no input and are dropped).
    #[must_use]
    pub fn from_log(log: &Log) -> Vec<Self> {
        log.records
            .iter()
            .filter_map(|r| match &r.e {
                Event::Connect { conn } => Some(Self::Connect { conn: *conn }),
                Event::Hello { conn, node } => Some(Self::Hello {
                    conn: *conn,
                    node: *node,
                }),
                Event::Bind { conn, addr } => Some(Self::Bind {
                    conn: *conn,
                    addr: (*addr).into(),
                }),
                Event::Send {
                    conn,
                    src,
                    dst,
                    payload,
                    ..
                } => Some(Self::Send {
                    conn: *conn,
                    src: (*src).into(),
                    dst: (*dst).into(),
                    payload: weft_replay::hash::from_hex(payload).unwrap_or_default(),
                }),
                Event::Recv { conn, blocking, .. } => Some(Self::Recv {
                    conn: *conn,
                    blocking: *blocking,
                }),
                Event::Disconnect { conn } => Some(Self::Disconnect { conn: *conn }),
                Event::Violation { .. } | Event::End { .. } => None,
            })
            .collect()
    }
}

/// One executed op: its input index, the virtual time after it, and the full
/// event (with recomputed outcome).
pub type ExecEvent = (u64, u64, Event);

/// The result of executing an input sequence under one (seed, net).
pub struct ExecOutcome {
    pub events: Vec<ExecEvent>,
    /// Violations raised by the supplied invariants; `op` indexes into the
    /// *input sequence* (== into `events`).
    pub violations: Vec<ViolationRecord>,
    /// `(sent, dropped)` totals from the core.
    pub stats: (u64, u64),
    pub final_vt: u64,
}

fn apply(core: &mut Core, input: &OpInput) -> Event {
    match input {
        OpInput::Connect { conn } => {
            core.connect(*conn);
            Event::Connect { conn: *conn }
        }
        OpInput::Hello { conn, node } => Event::Hello {
            conn: *conn,
            node: *node,
        },
        OpInput::Bind { conn, addr } => {
            core.bind(*conn, *addr);
            Event::Bind {
                conn: *conn,
                addr: (*addr).into(),
            }
        }
        OpInput::Send {
            conn,
            src,
            dst,
            payload,
        } => {
            let (chan_seq, res) = core.send(*src, *dst, payload);
            Event::Send {
                conn: *conn,
                src: (*src).into(),
                dst: (*dst).into(),
                chan_seq,
                payload: to_hex(payload),
                outcome: match res {
                    SendResult::Dropped => SendOutcome::Dropped,
                    SendResult::NoReceiver => SendOutcome::NoReceiver,
                    SendResult::Enqueued {
                        to_conn,
                        deliv_ns,
                        tie,
                    } => SendOutcome::Enqueued {
                        to_conn,
                        deliv_ns,
                        tie,
                    },
                },
            }
        }
        OpInput::Recv { conn, blocking } => Event::Recv {
            conn: *conn,
            blocking: *blocking,
            outcome: match core.recv(*conn) {
                RecvResult::Empty => RecvOutcome::Empty,
                RecvResult::Delivered {
                    src,
                    dst,
                    deliv_ns,
                    tie,
                    payload,
                } => RecvOutcome::Delivered {
                    src: src.into(),
                    dst: dst.into(),
                    deliv_ns,
                    tie,
                    payload: to_hex(&payload),
                },
            },
        },
        OpInput::Disconnect { conn } => {
            core.disconnect(*conn);
            Event::Disconnect { conn: *conn }
        }
    }
}

/// Execute `ops` under `(seed, net)`, feeding every event to `invariants`.
///
/// Fully deterministic and side-effect free: same arguments, same outcome,
/// on any machine.
///
/// # Errors
/// A message when the net spec does not parse.
pub fn execute(
    seed: u64,
    net: &str,
    ops: &[OpInput],
    invariants: Vec<Box<dyn Invariant>>,
) -> Result<ExecOutcome, String> {
    let model = weft_net::config::parse(seed, net).map_err(|e| e.to_string())?;
    let mut core = Core::new(model);
    let mut monitor = Monitor::new();
    for inv in invariants {
        monitor.register(inv);
    }
    let mut events = Vec::with_capacity(ops.len());
    for (i, input) in ops.iter().enumerate() {
        let e = apply(&mut core, input);
        let op = i as u64;
        let vt = core.vt();
        monitor.observe(op, vt, &e);
        events.push((op, vt, e));
    }
    Ok(ExecOutcome {
        events,
        violations: monitor.violations().to_vec(),
        stats: core.stats(),
        final_vt: core.vt(),
    })
}

/// Execute `ops` and write the result as a fresh `weft-log` at `path` —
/// violations interleaved at their trigger points, `end` record included —
/// so the file replays and checks like any live recording.
///
/// # Errors
/// Net-spec or I/O errors, as a message.
///
/// # Panics
/// Only if JSON serialization fails, which cannot happen for these types.
pub fn execute_and_record(
    path: &Path,
    seed: u64,
    net: &str,
    ops: &[OpInput],
    invariants: Vec<Box<dyn Invariant>>,
    label: &str,
) -> Result<Vec<ViolationRecord>, String> {
    let out = execute(seed, net, ops, invariants)?;
    let header = Header {
        format: FORMAT.into(),
        version: VERSION,
        seed,
        net: net.into(),
        meta: Meta {
            label: Some(label.into()),
            ..Meta::default()
        },
    };
    let mut w = LogWriter::create(path, &header).map_err(|e| e.to_string())?;
    let mut viol = out.violations.iter().peekable();
    for (op, vt, e) in &out.events {
        w.append(*vt, e.clone()).map_err(|e| e.to_string())?;
        while viol.peek().is_some_and(|v| v.op == *op) {
            let v = viol.next().expect("peeked");
            w.append(
                *vt,
                Event::Violation {
                    invariant: v.invariant.clone(),
                    message: v.message.clone(),
                },
            )
            .map_err(|e| e.to_string())?;
        }
    }
    w.finish(out.final_vt).map_err(|e| e.to_string())?;
    w.finalize().map_err(|e| e.to_string())?;
    Ok(out.violations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use weft_replay::invariant::PerChannelFifo;
    use weft_replay::replay_log;

    fn addr(node: u32, port: u16) -> VAddr {
        VAddr::new(0x7f00_0001 + node, port)
    }

    fn burst_ops(n: u32) -> Vec<OpInput> {
        let mut ops = vec![
            OpInput::Connect { conn: 0 },
            OpInput::Bind {
                conn: 0,
                addr: addr(0, 100),
            },
        ];
        for i in 0..n {
            ops.push(OpInput::Send {
                conn: 1,
                src: addr(1, 200),
                dst: addr(0, 100),
                payload: i.to_le_bytes().to_vec(),
            });
        }
        for _ in 0..n {
            ops.push(OpInput::Recv {
                conn: 0,
                blocking: false,
            });
        }
        ops
    }

    #[test]
    fn execute_is_deterministic() {
        let ops = burst_ops(20);
        let run = || {
            let out = execute(
                3,
                "latency=uniform:1000-100000",
                &ops,
                vec![Box::new(PerChannelFifo::new())],
            )
            .unwrap();
            (out.events, out.violations, out.stats, out.final_vt)
        };
        let a = run();
        assert_eq!(a, run());
        assert!(!a.1.is_empty(), "burst under variance must reorder");
    }

    #[test]
    fn recorded_execution_replays_and_round_trips_inputs() {
        let ops = burst_ops(12);
        let path =
            std::env::temp_dir().join(format!("weft-fuzz-exec-{}.weftlog", std::process::id()));
        execute_and_record(
            &path,
            3,
            "latency=uniform:1000-100000",
            &ops,
            Vec::new(),
            "t",
        )
        .unwrap();
        let log = Log::read(&path).unwrap();
        let replayed = replay_log(&log, Vec::new(), None).unwrap();
        assert!(replayed.identical, "{:?}", replayed.divergence);
        // Inputs extracted from the recorded log equal the inputs we ran.
        assert_eq!(OpInput::from_log(&log), ops);
        let _ = std::fs::remove_file(&path);
    }
}
