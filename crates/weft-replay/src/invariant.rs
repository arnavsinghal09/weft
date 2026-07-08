//! Invariant checking against the event stream.
//!
//! An [`Invariant`] sees every linearized broker event and may flag a
//! violation. The same invariant runs in two modes with identical results:
//!
//! - **in-process**: a [`Monitor`] attached to a live recording observes
//!   events as the broker serializes them, so a violation is known the moment
//!   it happens;
//! - **external**: [`Monitor::check_log`] replays a finished (or truncated)
//!   log file through the same invariants from a separate checker process.
//!
//! Every violation is anchored to a precise point on the logical timeline:
//! the `op` index (position in the linearization) and `vt` (the broker's
//! virtual-time high-water mark in nanoseconds at that operation) — never
//! "sometime during the run".

use std::collections::HashMap;

use crate::log::{Event, Log, RecvOutcome, SendOutcome};

/// A recorded invariant violation, anchored to the logical timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct ViolationRecord {
    pub invariant: String,
    /// Position in the linearized operation order.
    pub op: u64,
    /// Virtual time (ns) of the violating operation.
    pub vt: u64,
    pub message: String,
    /// The event that completed the violation.
    pub event: Event,
}

/// A predicate over the event stream. Implementations keep their own state;
/// `on_event` returns a human-readable message when the event completes a
/// violation.
pub trait Invariant: Send {
    fn name(&self) -> &str;
    fn on_event(&mut self, op: u64, vt: u64, e: &Event) -> Option<String>;
}

/// Adapter so ad-hoc invariants can be written as closures.
pub struct FnInvariant<F> {
    name: String,
    f: F,
}

impl<F> FnInvariant<F>
where
    F: FnMut(u64, u64, &Event) -> Option<String> + Send,
{
    pub fn new(name: impl Into<String>, f: F) -> Self {
        Self {
            name: name.into(),
            f,
        }
    }
}

impl<F> Invariant for FnInvariant<F>
where
    F: FnMut(u64, u64, &Event) -> Option<String> + Send,
{
    fn name(&self) -> &str {
        &self.name
    }
    fn on_event(&mut self, op: u64, vt: u64, e: &Event) -> Option<String> {
        (self.f)(op, vt, e)
    }
}

/// Runs a set of invariants over an event stream and collects violations.
#[derive(Default)]
pub struct Monitor {
    invariants: Vec<Box<dyn Invariant>>,
    violations: Vec<ViolationRecord>,
}

impl Monitor {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, inv: Box<dyn Invariant>) {
        self.invariants.push(inv);
    }

    /// Feed one event (in-process use). Returns how many *new* violations it
    /// produced, so a live recorder can react (e.g. log them) immediately.
    pub fn observe(&mut self, op: u64, vt: u64, e: &Event) -> usize {
        let before = self.violations.len();
        for inv in &mut self.invariants {
            if let Some(message) = inv.on_event(op, vt, e) {
                self.violations.push(ViolationRecord {
                    invariant: inv.name().to_string(),
                    op,
                    vt,
                    message,
                    event: e.clone(),
                });
            }
        }
        self.violations.len() - before
    }

    #[must_use]
    pub fn violations(&self) -> &[ViolationRecord] {
        &self.violations
    }

    /// External-checker entry point: run this monitor's invariants over a
    /// verified log and return every violation found.
    #[must_use]
    pub fn check_log(mut self, log: &Log) -> Vec<ViolationRecord> {
        for r in &log.records {
            self.observe(r.op, r.vt, &r.e);
        }
        self.violations
    }
}

/// Built-in: deliveries on each channel (src→dst) must arrive in send order.
///
/// Any latency distribution with variance violates this by design — Weft's
/// reordering is not a bug — so this invariant is the standard demo of
/// record-a-violation / replay-the-violation. Against a real protocol you
/// would instead assert an application property (e.g. "reads never return a
/// version older than the last acknowledged write").
#[derive(Default)]
pub struct PerChannelFifo {
    /// tie → (channel, chan_seq), recorded at enqueue time.
    enqueued: HashMap<u64, ((crate::log::Addr, crate::log::Addr), u64)>,
    /// Highest chan_seq delivered so far per channel.
    delivered_max: HashMap<(u32, u16, u32, u16), u64>,
}

impl PerChannelFifo {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Invariant for PerChannelFifo {
    fn name(&self) -> &'static str {
        "per-channel-fifo"
    }

    fn on_event(&mut self, _op: u64, _vt: u64, e: &Event) -> Option<String> {
        match e {
            Event::Send {
                src,
                dst,
                chan_seq,
                outcome: SendOutcome::Enqueued { tie, .. },
                ..
            } => {
                self.enqueued.insert(*tie, ((*src, *dst), *chan_seq));
                None
            }
            Event::Recv {
                outcome: RecvOutcome::Delivered { tie, src, dst, .. },
                ..
            } => {
                let &(_, seq) = self.enqueued.get(tie)?;
                let key = (src.ip, src.port, dst.ip, dst.port);
                let max = self.delivered_max.entry(key).or_insert(seq);
                if seq < *max {
                    return Some(format!(
                        "channel {src} → {dst}: datagram with send order {seq} \
                         delivered after send order {max} (reordered)"
                    ));
                }
                *max = seq;
                None
            }
            _ => None,
        }
    }
}

/// Built-in: no datagram (identified by its unique tie) is delivered twice.
#[derive(Default)]
pub struct NoDuplicateDelivery {
    seen: HashMap<u64, u64>, // tie → op of first delivery
}

impl NoDuplicateDelivery {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Invariant for NoDuplicateDelivery {
    fn name(&self) -> &'static str {
        "no-duplicate-delivery"
    }

    fn on_event(&mut self, op: u64, _vt: u64, e: &Event) -> Option<String> {
        if let Event::Recv {
            outcome: RecvOutcome::Delivered { tie, .. },
            ..
        } = e
        {
            if let Some(first) = self.seen.insert(*tie, op) {
                return Some(format!(
                    "datagram tie={tie} delivered twice (first at op {first})"
                ));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::Addr;

    fn a() -> Addr {
        Addr {
            ip: 0x7f00_0001,
            port: 100,
        }
    }
    fn b() -> Addr {
        Addr {
            ip: 0x7f00_0002,
            port: 200,
        }
    }

    fn send(seq: u64, tie: u64) -> Event {
        Event::Send {
            conn: 1,
            src: b(),
            dst: a(),
            chan_seq: seq,
            payload: String::new(),
            outcome: SendOutcome::Enqueued {
                to_conn: 0,
                deliv_ns: 0,
                tie,
            },
        }
    }

    fn deliver(tie: u64) -> Event {
        Event::Recv {
            conn: 0,
            blocking: false,
            outcome: RecvOutcome::Delivered {
                src: b(),
                dst: a(),
                deliv_ns: 0,
                tie,
                payload: String::new(),
            },
        }
    }

    #[test]
    fn fifo_passes_in_order_and_flags_reorder() {
        let mut m = Monitor::new();
        m.register(Box::new(PerChannelFifo::new()));
        // seq 0 and 1 enqueued; 1 delivered before 0 → violation on 0.
        assert_eq!(m.observe(0, 0, &send(0, 10)), 0);
        assert_eq!(m.observe(1, 0, &send(1, 11)), 0);
        assert_eq!(m.observe(2, 5, &deliver(11)), 0);
        assert_eq!(m.observe(3, 6, &deliver(10)), 1);
        let v = &m.violations()[0];
        assert_eq!(v.invariant, "per-channel-fifo");
        assert_eq!((v.op, v.vt), (3, 6));
        assert!(v.message.contains("send order 0"), "{}", v.message);
    }

    #[test]
    fn duplicate_delivery_is_flagged() {
        let mut m = Monitor::new();
        m.register(Box::new(NoDuplicateDelivery::new()));
        m.observe(0, 0, &send(0, 42));
        assert_eq!(m.observe(1, 1, &deliver(42)), 0);
        assert_eq!(m.observe(2, 2, &deliver(42)), 1);
        assert!(m.violations()[0].message.contains("tie=42"));
    }

    #[test]
    fn fn_invariant_and_check_log_agree_with_observe() {
        // The same invariant must produce identical results in-process
        // (observe) and externally (check_log).
        use crate::log::{Header, Log, LogWriter, Meta, FORMAT, VERSION};
        let header = Header {
            format: FORMAT.into(),
            version: VERSION,
            seed: 1,
            net: String::new(),
            meta: Meta::default(),
        };
        let events = [send(0, 10), send(1, 11), deliver(11), deliver(10)];

        let mut buf = Vec::new();
        let mut w = LogWriter::new(&mut buf, &header).unwrap();
        let mut live = Monitor::new();
        live.register(Box::new(PerChannelFifo::new()));
        for (i, e) in events.iter().enumerate() {
            let op = w.append(i as u64, e.clone()).unwrap();
            live.observe(op, i as u64, e);
        }
        w.finish(4).unwrap();

        let log = Log::from_reader(buf.as_slice()).unwrap();
        let mut external = Monitor::new();
        external.register(Box::new(PerChannelFifo::new()));
        let found = external.check_log(&log);

        assert_eq!(live.violations(), found.as_slice());
        assert_eq!(found.len(), 1);
    }
}
