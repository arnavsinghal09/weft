//! Deterministic replay: re-execute a recorded log and verify the result is
//! byte-for-byte identical.
//!
//! The log supplies only the linearized operation order and payloads; every
//! decision (fate, tie, delivery choice, virtual time) is *recomputed* by
//! driving the same [`weft_net::core::Core`] state machine the live broker
//! uses, seeded from the log header. Each recomputed event is compared
//! against the recorded one; the first mismatch is reported as a
//! [`Divergence`] with both sides serialized. A clean replay reproduces the
//! recorded stream digest exactly, on any machine — the replayer never reads
//! a clock, spawns a thread, or draws entropy.

use std::collections::VecDeque;

use weft_net::core::{Core, RecvResult, SendResult};

use crate::hash::{fnv1a, from_hex, FNV_OFFSET};
use crate::invariant::{Invariant, Monitor, ViolationRecord};
use crate::log::{canon, Event, Log, RecvOutcome, SendOutcome};

/// Why a replay could not run at all (as opposed to running and diverging).
#[derive(Debug)]
pub enum ReplayError {
    /// The header's net spec did not parse — the log cannot be interpreted.
    BadNetSpec(String),
    /// A recorded payload was not valid hex at this op.
    BadPayload { op: u64 },
}

impl std::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadNetSpec(e) => write!(f, "log header net spec unusable: {e}"),
            Self::BadPayload { op } => write!(f, "op {op}: payload is not valid hex"),
        }
    }
}

impl std::error::Error for ReplayError {}

/// The first point where re-execution disagreed with the recording.
#[derive(Clone, Debug, PartialEq)]
pub struct Divergence {
    pub op: u64,
    /// Canonical JSON of the recorded (op, vt, event).
    pub recorded: String,
    /// Canonical JSON of the recomputed (op, vt, event).
    pub replayed: String,
}

/// The result of a replay run.
#[derive(Debug)]
pub struct ReplayOutcome {
    /// Whether every replayed record matched the recording exactly.
    pub identical: bool,
    /// Records replayed (≤ log length when `until` was used or a divergence
    /// stopped the run).
    pub ops_replayed: u64,
    pub divergence: Option<Divergence>,
    /// Violations the supplied invariants raised during replay.
    pub violations: Vec<ViolationRecord>,
    /// FNV-1a digest of the replayed stream; equals `Log::stream_digest()`
    /// when `identical` and the whole log was replayed.
    pub stream_digest: u64,
}

/// Replay `log`, checking `invariants` along the way.
///
/// `until`: stop after replaying the record with this op index (inclusive) —
/// useful to halt right after a violation and inspect state.
///
/// Recorded `Violation` events are handled two ways: if `invariants` were
/// supplied, the replayed invariants must re-raise an identical violation at
/// the same position (a mismatch is a divergence); with no invariants they
/// are carried through verbatim so the stream digest still verifies.
///
/// # Errors
/// [`ReplayError`] when the log cannot be interpreted at all; a log that
/// replays but disagrees is reported via [`ReplayOutcome::divergence`].
#[allow(clippy::too_many_lines)] // one match arm per event type; splitting obscures it
pub fn replay_log(
    log: &Log,
    invariants: Vec<Box<dyn Invariant>>,
    until: Option<u64>,
) -> Result<ReplayOutcome, ReplayError> {
    let model = weft_net::config::parse(log.header.seed, &log.header.net)
        .map_err(|e| ReplayError::BadNetSpec(e.to_string()))?;
    let mut core = Core::new(model);
    let checking = !invariants.is_empty();
    let mut monitor = Monitor::new();
    for inv in invariants {
        monitor.register(inv);
    }
    // Violations the monitor has raised that the log has not yet accounted for.
    let mut unmatched: VecDeque<ViolationRecord> = VecDeque::new();
    let mut seen_violations = 0usize;

    let mut digest = FNV_OFFSET;
    let mut ops_replayed = 0u64;

    for r in &log.records {
        if let Some(u) = until {
            if r.op > u {
                break;
            }
        }

        // Re-execute the operation's *inputs*; recompute its outcome.
        let replayed: Event = match &r.e {
            Event::Connect { conn } => {
                core.connect(*conn);
                r.e.clone()
            }
            Event::Hello { .. } | Event::Bind { .. } | Event::Disconnect { .. } => {
                match &r.e {
                    Event::Bind { conn, addr } => core.bind(*conn, (*addr).into()),
                    Event::Disconnect { conn } => core.disconnect(*conn),
                    _ => {}
                }
                r.e.clone()
            }
            Event::Send {
                conn,
                src,
                dst,
                send_vt,
                payload,
                ..
            } => {
                let bytes = from_hex(payload).ok_or(ReplayError::BadPayload { op: r.op })?;
                let (chan_seq, res) = core.send((*src).into(), (*dst).into(), &bytes, *send_vt);
                let outcome = match res {
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
                };
                Event::Send {
                    conn: *conn,
                    src: *src,
                    dst: *dst,
                    chan_seq,
                    send_vt: *send_vt,
                    payload: payload.clone(),
                    outcome,
                }
            }
            Event::Recv { conn, blocking, .. } => {
                let outcome = match core.recv(*conn) {
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
                        payload: crate::hash::to_hex(&payload),
                    },
                };
                Event::Recv {
                    conn: *conn,
                    blocking: *blocking,
                    outcome,
                }
            }
            Event::Violation { .. } => {
                if checking {
                    // The replayed invariants must have raised this violation
                    // already (violations are logged right after their
                    // triggering op).
                    match unmatched.pop_front() {
                        Some(v) => Event::Violation {
                            invariant: v.invariant,
                            message: v.message,
                        },
                        None => Event::Violation {
                            invariant: "(none raised)".into(),
                            message: "replayed invariants raised no violation here".into(),
                        },
                    }
                } else {
                    r.e.clone() // carried through unchecked
                }
            }
            Event::End { .. } => {
                let (sent, dropped) = core.stats();
                Event::End {
                    events: r.op + 1,
                    sent,
                    dropped,
                }
            }
        };

        let vt = core.vt();
        digest = fnv1a(digest, canon(r.op, vt, &replayed).as_bytes());
        ops_replayed += 1;

        // Feed boundary ops (not violation/end markers) to the invariants,
        // exactly as the live monitor sees them.
        if checking && !matches!(replayed, Event::Violation { .. } | Event::End { .. }) {
            let raised = monitor.observe(r.op, vt, &replayed);
            let all = monitor.violations();
            for v in &all[all.len() - raised..] {
                unmatched.push_back(v.clone());
            }
            seen_violations = all.len();
        }
        let _ = seen_violations;

        if replayed != r.e || vt != r.vt {
            return Ok(ReplayOutcome {
                identical: false,
                ops_replayed,
                divergence: Some(Divergence {
                    op: r.op,
                    recorded: canon(r.op, r.vt, &r.e),
                    replayed: canon(r.op, vt, &replayed),
                }),
                violations: monitor.violations().to_vec(),
                stream_digest: digest,
            });
        }
    }

    Ok(ReplayOutcome {
        identical: true,
        ops_replayed,
        divergence: None,
        violations: monitor.violations().to_vec(),
        stream_digest: digest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invariant::PerChannelFifo;
    use crate::log::{Addr, Header, LogWriter, Meta, FORMAT, VERSION};

    /// Record a synthetic-but-honest run by driving the same Core the broker
    /// uses, then replay it. This is the single-process-level proof; the
    /// live-broker proof lives in tests/record_replay.rs.
    fn record_run(seed: u64, net: &str) -> Vec<u8> {
        let model = weft_net::config::parse(seed, net).unwrap();
        let mut core = Core::new(model);
        let header = Header {
            format: FORMAT.into(),
            version: VERSION,
            seed,
            net: net.into(),
            window_ns: 0,
            meta: Meta::default(),
        };
        let mut buf = Vec::new();
        let mut w = LogWriter::new(&mut buf, &header).unwrap();
        let a = Addr {
            ip: 0x7f00_0001,
            port: 100,
        };
        let b = Addr {
            ip: 0x7f00_0002,
            port: 200,
        };

        let log_ev = |core: &Core, w: &mut LogWriter<&mut Vec<u8>>, e: Event| {
            w.append(core.vt(), e).unwrap();
        };

        core.connect(0);
        log_ev(&core, &mut w, Event::Connect { conn: 0 });
        core.connect(1);
        log_ev(&core, &mut w, Event::Connect { conn: 1 });
        core.bind(0, a.into());
        log_ev(&core, &mut w, Event::Bind { conn: 0, addr: a });

        for i in 0u32..12 {
            let payload = i.to_le_bytes();
            let (chan_seq, res) = core.send(b.into(), a.into(), &payload, 0);
            let outcome = match res {
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
            };
            log_ev(
                &core,
                &mut w,
                Event::Send {
                    conn: 1,
                    src: b,
                    dst: a,
                    chan_seq,
                    send_vt: 0,
                    payload: crate::hash::to_hex(&payload),
                    outcome,
                },
            );
        }
        loop {
            let out = match core.recv(0) {
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
                    payload: crate::hash::to_hex(&payload),
                },
            };
            let done = out == RecvOutcome::Empty;
            log_ev(
                &core,
                &mut w,
                Event::Recv {
                    conn: 0,
                    blocking: false,
                    outcome: out,
                },
            );
            if done {
                break;
            }
        }
        w.finish(core.vt()).unwrap();
        buf
    }

    #[test]
    fn replay_reproduces_the_recording_exactly() {
        let buf = record_run(3, "latency=uniform:1000-100000");
        let log = Log::from_reader(buf.as_slice()).unwrap();
        let out = replay_log(&log, Vec::new(), None).unwrap();
        assert!(out.identical, "divergence: {:?}", out.divergence);
        assert_eq!(out.ops_replayed, log.records.len() as u64);
        assert_eq!(out.stream_digest, log.stream_digest());
    }

    #[test]
    fn replay_is_stable_across_many_repetitions() {
        let buf = record_run(3, "latency=uniform:1000-100000");
        let log = Log::from_reader(buf.as_slice()).unwrap();
        let reference = log.stream_digest();
        for i in 0..10 {
            let out = replay_log(&log, Vec::new(), None).unwrap();
            assert!(out.identical, "run {i} diverged: {:?}", out.divergence);
            assert_eq!(out.stream_digest, reference, "run {i} digest changed");
        }
    }

    #[test]
    fn tampered_seed_diverges_instead_of_lying() {
        let buf = record_run(3, "latency=uniform:1000-100000");
        let mut log = Log::from_reader(buf.as_slice()).unwrap();
        log.header.seed = 4; // same ops, different fault decisions
        let out = replay_log(&log, Vec::new(), None).unwrap();
        assert!(!out.identical);
        let d = out.divergence.unwrap();
        assert!(d.recorded != d.replayed);
    }

    #[test]
    fn replay_until_stops_early() {
        let buf = record_run(3, "latency=uniform:1000-100000");
        let log = Log::from_reader(buf.as_slice()).unwrap();
        let out = replay_log(&log, Vec::new(), Some(5)).unwrap();
        assert!(out.identical);
        assert_eq!(out.ops_replayed, 6); // ops 0..=5
    }

    #[test]
    fn invariants_fire_identically_on_replay() {
        // Uniform latency reorders → FIFO violation must appear on replay.
        let buf = record_run(3, "latency=uniform:1000-100000");
        let log = Log::from_reader(buf.as_slice()).unwrap();
        let out = replay_log(&log, vec![Box::new(PerChannelFifo::new())], None).unwrap();
        assert!(out.identical);
        assert!(
            !out.violations.is_empty(),
            "uniform 1000-100000 over a 12-datagram burst must reorder"
        );
        // And identically on every repetition.
        let first: Vec<_> = out.violations.clone();
        for _ in 0..3 {
            let again = replay_log(&log, vec![Box::new(PerChannelFifo::new())], None).unwrap();
            assert_eq!(again.violations, first);
        }
    }
}
