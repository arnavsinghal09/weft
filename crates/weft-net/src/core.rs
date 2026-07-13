//! The broker's pure decision core: a single-threaded state machine with no
//! I/O, no locks, and no clock. The live broker ([`crate::Broker`]) drives one
//! instance under its state lock; the replayer (`weft-replay`) drives another
//! from a recorded log. Because both paths execute *this* code, a replay
//! cannot drift from live behavior without failing loudly.
//!
//! Semantics are exactly the live broker's:
//! - per-channel (src→dst) sequence numbers advance on every send, dropped or
//!   not, so a datagram's fate never depends on other channels;
//! - the global `tie` counter advances only when a datagram is actually
//!   enqueued, and orders same-delivery-time datagrams deterministically;
//! - `recv` pops the pending datagram with the smallest `(deliv_ns, tie)`;
//! - a send to an unbound address is discarded like UDP to a closed port.

use std::collections::{BinaryHeap, HashMap};

use crate::fault::FaultModel;
use crate::wire::VAddr;

/// A datagram waiting in a destination queue.
pub(crate) struct Pending {
    pub deliv: u64,
    pub tie: u64,
    pub src: VAddr,
    pub dst: VAddr,
    pub payload: Vec<u8>,
}

impl PartialEq for Pending {
    fn eq(&self, o: &Self) -> bool {
        (self.deliv, self.tie) == (o.deliv, o.tie)
    }
}
impl Eq for Pending {}
impl Ord for Pending {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        // Reversed so a `BinaryHeap` (max-heap) yields the *smallest*
        // (deliv, tie) first.
        (o.deliv, o.tie).cmp(&(self.deliv, self.tie))
    }
}
impl PartialOrd for Pending {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

/// The core's decision for one send.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SendResult {
    /// The fault model dropped it (loss or partition).
    Dropped,
    /// No connection has bound the destination address.
    NoReceiver,
    /// Enqueued for `to_conn` with the sampled virtual delivery time.
    Enqueued {
        to_conn: u64,
        deliv_ns: u64,
        tie: u64,
    },
}

/// The core's answer to one (non-waiting) receive.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RecvResult {
    Empty,
    Delivered {
        src: VAddr,
        dst: VAddr,
        deliv_ns: u64,
        tie: u64,
        payload: Vec<u8>,
    },
}

/// Pure broker state. See the module docs for the semantics contract.
pub struct Core {
    model: FaultModel,
    /// Which connection receives datagrams for a bound address.
    bound: HashMap<VAddr, u64>,
    queues: HashMap<u64, BinaryHeap<Pending>>,
    /// Per-channel (src→dst) datagram counter, feeding the fault model.
    seq: HashMap<(VAddr, VAddr), u64>,
    /// Global enqueue counter, the deterministic delivery tiebreaker.
    tie: u64,
    /// Virtual-time high-water mark: the largest delivery time scheduled so
    /// far. This is the logical-timeline coordinate events are stamped with.
    vt: u64,
    sent: u64,
    dropped: u64,
}

impl Core {
    #[must_use]
    pub fn new(model: FaultModel) -> Self {
        Self {
            model,
            bound: HashMap::new(),
            queues: HashMap::new(),
            seq: HashMap::new(),
            tie: 0,
            vt: 0,
            sent: 0,
            dropped: 0,
        }
    }

    /// Register a connection (gives it an empty delivery queue).
    pub fn connect(&mut self, conn: u64) {
        self.queues.entry(conn).or_default();
    }

    /// Claim `addr` for `conn`: subsequent sends to `addr` land in its queue.
    pub fn bind(&mut self, conn: u64, addr: VAddr) {
        self.bound.insert(addr, conn);
    }

    /// Route one datagram. `send_vt` is the sender's local virtual time at the
    /// moment of the send; the delivery time is anchored to it
    /// (`deliv = send_vt + seeded_latency`) so a datagram lands at a coherent
    /// point on the timeline rather than at a bare latency offset. Single-host
    /// callers pass `send_vt = 0`, recovering the original latency-only
    /// delivery order (and leaving same-seed outcomes unchanged); the windowed
    /// multi-host broker passes the real local time so cross-host deliveries
    /// order on one shared timeline. Returns the channel sequence number
    /// consumed and the decision; deterministic given the same call sequence.
    pub fn send(
        &mut self,
        src: VAddr,
        dst: VAddr,
        payload: &[u8],
        send_vt: u64,
    ) -> (u64, SendResult) {
        self.sent += 1;
        let seq = {
            let e = self.seq.entry((src, dst)).or_insert(0);
            let v = *e;
            *e += 1;
            v
        };
        let fate = self.model.fate(src, dst, seq, payload.len());
        if fate.dropped {
            self.dropped += 1;
            return (seq, SendResult::Dropped);
        }
        let Some(&conn) = self.bound.get(&dst) else {
            return (seq, SendResult::NoReceiver);
        };
        let deliv = send_vt.saturating_add(fate.delay_ns);
        let tie = self.tie;
        self.tie += 1;
        self.vt = self.vt.max(deliv);
        if let Some(q) = self.queues.get_mut(&conn) {
            q.push(Pending {
                deliv,
                tie,
                src,
                dst,
                payload: payload.to_vec(),
            });
        }
        (
            seq,
            SendResult::Enqueued {
                to_conn: conn,
                deliv_ns: deliv,
                tie,
            },
        )
    }

    /// Pop the next deliverable datagram for `conn`, if any. (The live
    /// broker's *blocking* recv is this same pop, retried when new sends
    /// arrive; its linearization point is the successful pop.)
    pub fn recv(&mut self, conn: u64) -> RecvResult {
        match self.queues.get_mut(&conn).and_then(BinaryHeap::pop) {
            Some(p) => RecvResult::Delivered {
                src: p.src,
                dst: p.dst,
                deliv_ns: p.deliv,
                tie: p.tie,
                payload: p.payload,
            },
            None => RecvResult::Empty,
        }
    }

    /// Whether `conn` currently has a deliverable datagram (without popping).
    #[must_use]
    pub fn has_pending(&self, conn: u64) -> bool {
        self.queues.get(&conn).is_some_and(|q| !q.is_empty())
    }

    /// Whether `conn` is registered (i.e. has connected and not disconnected).
    #[must_use]
    pub fn is_connected(&self, conn: u64) -> bool {
        self.queues.contains_key(&conn)
    }

    /// Drop the connection: its queue and every address bound to it go away.
    pub fn disconnect(&mut self, conn: u64) {
        self.queues.remove(&conn);
        self.bound.retain(|_, c| *c != conn);
    }

    /// The virtual-time high-water mark (ns): the logical-timeline coordinate
    /// to stamp on events observed now.
    #[must_use]
    pub fn vt(&self) -> u64 {
        self.vt
    }

    /// `(datagrams_sent, datagrams_dropped)` so far.
    #[must_use]
    pub fn stats(&self) -> (u64, u64) {
        (self.sent, self.dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault::Latency;

    fn addr(n: u32, p: u16) -> VAddr {
        VAddr::new(0x7f00_0001 + n, p)
    }

    fn model(seed: u64) -> FaultModel {
        FaultModel {
            seed,
            latency: Latency::Uniform {
                lo: 1_000,
                hi: 100_000,
            },
            loss: 0.0,
            bandwidth_bps: 0,
            partition: crate::fault::Partition::none(),
        }
    }

    /// Drive the same operation sequence twice; every decision must match.
    #[test]
    fn identical_op_sequences_produce_identical_decisions() {
        let run = || {
            let mut c = Core::new(model(3));
            c.connect(0);
            c.connect(1);
            c.bind(0, addr(0, 100));
            let mut sends = Vec::new();
            for i in 0u32..20 {
                sends.push(c.send(addr(1, 200), addr(0, 100), &i.to_le_bytes(), 0));
            }
            let mut delivered = Vec::new();
            while let RecvResult::Delivered { tie, payload, .. } = c.recv(0) {
                delivered.push((tie, payload));
            }
            (sends, delivered, c.vt(), c.stats())
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn unbound_destination_consumes_seq_but_not_tie() {
        let mut c = Core::new(model(1));
        c.connect(0);
        let (seq0, r0) = c.send(addr(1, 9), addr(0, 100), b"x", 0);
        assert_eq!((seq0, r0), (0, SendResult::NoReceiver));
        // Now bind and send again on the same channel: seq advanced, and the
        // first real enqueue takes tie 0 (no tie was burned on the miss).
        c.bind(0, addr(0, 100));
        let (seq1, r1) = c.send(addr(1, 9), addr(0, 100), b"y", 0);
        assert_eq!(seq1, 1);
        assert!(matches!(r1, SendResult::Enqueued { tie: 0, .. }), "{r1:?}");
    }

    #[test]
    fn recv_pops_smallest_delivery_time_then_tie() {
        let mut c = Core::new(FaultModel::reliable(0));
        c.connect(0);
        c.bind(0, addr(0, 1));
        // Reliable model: every delay is 0, so pops are pure tie order (FIFO).
        for i in 0u8..5 {
            c.send(addr(1, 2), addr(0, 1), &[i], 0);
        }
        let mut got = Vec::new();
        while let RecvResult::Delivered { payload, .. } = c.recv(0) {
            got.push(payload[0]);
        }
        assert_eq!(got, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn vt_tracks_the_largest_scheduled_delivery() {
        let mut c = Core::new(model(7));
        c.connect(0);
        c.bind(0, addr(0, 1));
        assert_eq!(c.vt(), 0);
        let mut max = 0;
        for i in 0u32..10 {
            if let (_, SendResult::Enqueued { deliv_ns, .. }) =
                c.send(addr(1, 2), addr(0, 1), &i.to_le_bytes(), 0)
            {
                max = max.max(deliv_ns);
            }
            assert_eq!(c.vt(), max);
        }
        assert!(max > 0);
    }

    #[test]
    fn delivery_time_is_anchored_to_send_vt() {
        // With a reliable model every latency is 0, so the delivery time is
        // exactly the sender's local virtual time: a datagram sent "later"
        // (higher send_vt) is scheduled later on the timeline, and `recv`
        // pops in that order regardless of enqueue order.
        let mut c = Core::new(FaultModel::reliable(0));
        c.connect(0);
        c.bind(0, addr(0, 1));
        // Enqueue out of send-time order: vt 500 first, then vt 100.
        let (_, r_late) = c.send(addr(1, 2), addr(0, 1), b"late", 500);
        let (_, r_early) = c.send(addr(1, 2), addr(0, 1), b"early", 100);
        assert!(matches!(r_late, SendResult::Enqueued { deliv_ns: 500, .. }));
        assert!(matches!(
            r_early,
            SendResult::Enqueued { deliv_ns: 100, .. }
        ));
        // Smallest deliv (100 = "early") pops first even though it was sent
        // second; vt tracks the largest scheduled delivery.
        let first = c.recv(0);
        assert!(matches!(&first, RecvResult::Delivered { payload, .. } if payload == b"early"));
        assert_eq!(c.vt(), 500);
    }

    #[test]
    fn disconnect_releases_bindings_and_queue() {
        let mut c = Core::new(FaultModel::reliable(0));
        c.connect(0);
        c.bind(0, addr(0, 1));
        c.send(addr(1, 2), addr(0, 1), b"pending", 0);
        c.disconnect(0);
        assert_eq!(c.recv(0), RecvResult::Empty);
        let (_, r) = c.send(addr(1, 2), addr(0, 1), b"gone", 0);
        assert_eq!(r, SendResult::NoReceiver);
    }
}
