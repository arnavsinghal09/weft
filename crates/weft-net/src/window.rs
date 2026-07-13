//! The windowed sequencer: the deterministic ordering core of the multi-host
//! protocol (docs/MULTI_HOST_CLOCK_PROTOCOL.md §4).
//!
//! Pure logic — no I/O, no clock, no locks, no RNG. The live broker feeds it
//! ops as connections deliver them (in *arrival* order, which this component
//! deliberately ignores) and, once a window seals, it emits the ops of that
//! window in the one **arrival-independent** order the protocol assigns:
//!
//! ```text
//! sort key = (local_vt, host_id, node_id, conn_seq)
//! ```
//!
//! Every component of that key is seed-derived (local_vt by the single-guest
//! determinism lemma; the identifiers are static config; conn_seq is program
//! order), so two runs of one seed — however their packets interleave in real
//! time — seal identical window contents in identical order. That invariant
//! is what the unit tests below pin down: the same ops admitted in *different*
//! arrival interleavings assign identically.
//!
//! Window `k` is the virtual-time interval `[k·W, (k+1)·W)`. It seals once
//! every live connection has promised (its *frontier*) never to emit an op
//! below `(k+1)·W`. Sealing is the only place the real protocol waits on the
//! real world, and waiting changes *when* a window seals, never *what* it
//! contains — see the correctness argument in the design doc.

use std::collections::HashMap;

use crate::wire::VAddr;

/// A frontier value meaning "this connection will emit nothing further": a
/// blocked-on-recv, exited, or closed guest. Never blocks sealing.
pub const INFINITY: u64 = u64::MAX;

/// Broker-assigned connection id.
pub type ConnId = u64;

/// A buffered send awaiting window assignment. The delivery fate and time are
/// drawn later, by `Core`, in assigned order — this component only orders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeqSend {
    pub conn: ConnId,
    pub host_id: u32,
    pub node_id: u32,
    /// Program-order index of this op on its connection (FIFO, broker-assigned).
    pub conn_seq: u64,
    /// The sender's local virtual time when it issued the send.
    pub local_vt: u64,
    pub src: VAddr,
    pub dst: VAddr,
    pub payload: Vec<u8>,
}

impl SeqSend {
    /// The protocol's total order key (design doc §4.1).
    fn key(&self) -> (u64, u32, u32, u64) {
        (self.local_vt, self.host_id, self.node_id, self.conn_seq)
    }
}

/// Why an op was refused. Every variant is a loud protocol violation that the
/// broker must turn into a run abort (design doc §8, F5) — never a silent
/// reorder.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SeqError {
    /// An op referenced a connection that was never registered.
    UnknownConn(ConnId),
    /// `local_vt` went backwards on a connection, or a frontier declaration
    /// did: the shim clock must be monotone (L2 in the design doc).
    NonMonotone { conn: ConnId, prev: u64, got: u64 },
    /// An op arrived carrying a `local_vt` in an already-sealed window — it
    /// should have been declared before the window closed.
    LateOp {
        conn: ConnId,
        local_vt: u64,
        horizon: u64,
    },
}

struct ConnState {
    host_id: u32,
    node_id: u32,
    /// The real lower bound on this connection's future `local_vt` — its LVT
    /// as last observed. Kept even while blocked, so a woken receiver resumes
    /// from `max(this, deliv_vt)` rather than losing its place.
    frontier: u64,
    /// True while the guest is parked in a blocking recv: it will emit nothing
    /// until the broker delivers to it, so it does not stall sealing (its
    /// effective frontier is `INFINITY`).
    blocked: bool,
    /// Next program-order index to hand out.
    next_seq: u64,
    /// Whether this connection still counts toward the sealing quorum (false
    /// once closed/exited — effective frontier `INFINITY`).
    live: bool,
}

impl ConnState {
    /// The value this connection contributes to the sealing quorum: its real
    /// lower bound, or `INFINITY` when it cannot emit (blocked, or not live).
    fn effective_frontier(&self) -> u64 {
        if self.live && !self.blocked {
            self.frontier
        } else {
            INFINITY
        }
    }
}

/// The windowed sequencer. One per broker (multi-host mode).
pub struct WindowSequencer {
    width: u64,
    conns: HashMap<ConnId, ConnState>,
    /// Admitted-but-not-yet-assigned sends, in arrival order (irrelevant).
    pending: Vec<SeqSend>,
    /// The sealed horizon: every window below this is sealed and assigned.
    /// `deliv_vt < horizon` is the poppable test for arrival-gated recv.
    horizon: u64,
}

impl WindowSequencer {
    /// Create a sequencer with window width `width` ns (must be non-zero).
    ///
    /// # Panics
    /// Panics if `width` is zero — a zero-width window can never seal.
    #[must_use]
    pub fn new(width: u64) -> Self {
        assert!(width > 0, "window width must be non-zero");
        Self {
            width,
            conns: HashMap::new(),
            pending: Vec::new(),
            horizon: 0,
        }
    }

    /// Register a live connection at frontier 0. Idempotent re-registration is
    /// a caller bug and panics in debug via the `HashMap` overwrite being
    /// silent; the broker registers each connection exactly once.
    pub fn register(&mut self, conn: ConnId, host_id: u32, node_id: u32) {
        self.conns.insert(
            conn,
            ConnState {
                host_id,
                node_id,
                frontier: 0,
                blocked: false,
                next_seq: 0,
                live: true,
            },
        );
    }

    /// The sealed horizon: `deliv_vt < horizon()` deliveries are poppable
    /// (arrival-gated recv, design doc §4.3). `INFINITY` once every live
    /// connection has released its frontier.
    #[must_use]
    pub fn horizon(&self) -> u64 {
        self.horizon
    }

    /// Admit one send. Returns the assigned `conn_seq`. The send is buffered,
    /// not ordered, until its window seals (`seal`). Advances the connection's
    /// frontier to `local_vt` (op-carried frontier declaration, §4.2).
    ///
    /// # Errors
    /// [`SeqError`] on an unknown connection, a non-monotone `local_vt`, or an
    /// op landing in an already-sealed window — all abort-worthy.
    pub fn admit_send(
        &mut self,
        conn: ConnId,
        local_vt: u64,
        src: VAddr,
        dst: VAddr,
        payload: Vec<u8>,
    ) -> Result<u64, SeqError> {
        if self.horizon != INFINITY && local_vt < self.horizon {
            return Err(SeqError::LateOp {
                conn,
                local_vt,
                horizon: self.horizon,
            });
        }
        let c = self
            .conns
            .get_mut(&conn)
            .ok_or(SeqError::UnknownConn(conn))?;
        // Op-carried frontier: local_vt must not go below what the connection
        // has already promised. (A blocked conn at INFINITY that speaks again
        // is likewise non-monotone — it promised silence.)
        if local_vt < c.frontier {
            return Err(SeqError::NonMonotone {
                conn,
                prev: c.frontier,
                got: local_vt,
            });
        }
        let conn_seq = c.next_seq;
        c.next_seq += 1;
        c.frontier = local_vt;
        let (host_id, node_id) = (c.host_id, c.node_id);
        self.pending.push(SeqSend {
            conn,
            host_id,
            node_id,
            conn_seq,
            local_vt,
            src,
            dst,
            payload,
        });
        Ok(conn_seq)
    }

    /// Declare a frontier without emitting an op (the explicit `Frontier`
    /// message, §4.2): "I will emit nothing below `f`."
    ///
    /// # Errors
    /// [`SeqError::NonMonotone`] if `f` is below the current frontier, or
    /// [`SeqError::UnknownConn`].
    pub fn declare_frontier(&mut self, conn: ConnId, f: u64) -> Result<(), SeqError> {
        let c = self
            .conns
            .get_mut(&conn)
            .ok_or(SeqError::UnknownConn(conn))?;
        if f < c.frontier {
            return Err(SeqError::NonMonotone {
                conn,
                prev: c.frontier,
                got: f,
            });
        }
        c.frontier = f;
        Ok(())
    }

    /// Mark a connection parked in a blocking receive at LVT `at_vt`: it
    /// leaves the sealing quorum (effective frontier `INFINITY`) but keeps its
    /// lower bound so [`Self::wake`] can resume it correctly (§4.2,
    /// release-on-block). `at_vt` advances the lower bound monotonically.
    pub fn block(&mut self, conn: ConnId, at_vt: u64) {
        if let Some(c) = self.conns.get_mut(&conn) {
            c.frontier = c.frontier.max(at_vt);
            c.blocked = true;
        }
    }

    /// Wake a blocked connection because a delivery at `deliv_vt` was popped
    /// for it: it rejoins the quorum with its LVT advanced to
    /// `max(prev, deliv_vt)` (the Lamport merge the guest itself performs).
    /// A no-op on a connection that was not blocked.
    pub fn wake(&mut self, conn: ConnId, deliv_vt: u64) {
        if let Some(c) = self.conns.get_mut(&conn) {
            if c.blocked {
                c.blocked = false;
                c.frontier = c.frontier.max(deliv_vt);
            }
        }
    }

    /// A connection closed (guest exited / TCP hangup): it leaves the sealing
    /// quorum forever (effective frontier `INFINITY`). Its already-buffered
    /// ops still get assigned in their windows.
    pub fn close(&mut self, conn: ConnId) {
        if let Some(c) = self.conns.get_mut(&conn) {
            c.live = false;
        }
    }

    /// Seal every window that can now seal and return the ops they contain in
    /// assigned order. A window `k` seals once every *live* connection's
    /// frontier is `≥ (k+1)·W`; when all live connections have released
    /// (`INFINITY`), or none remain, the horizon jumps to `INFINITY` and all
    /// remaining ops are assigned (quiescence).
    ///
    /// Returns an empty vec when nothing new sealed. The returned order is a
    /// pure function of the admitted ops and declared frontiers — never of the
    /// order in which they were admitted.
    pub fn seal(&mut self) -> Vec<SeqSend> {
        let min_frontier = self
            .conns
            .values()
            .map(ConnState::effective_frontier)
            .min()
            .unwrap_or(INFINITY);
        let new_horizon = if min_frontier == INFINITY {
            INFINITY
        } else {
            // Floor to a window boundary: window k is sealed iff (k+1)·W ≤ F,
            // so the sealed horizon is ⌊F/W⌋·W.
            (min_frontier / self.width) * self.width
        };
        if new_horizon <= self.horizon {
            return Vec::new();
        }
        let mut ready = Vec::new();
        let mut keep = Vec::with_capacity(self.pending.len());
        for op in self.pending.drain(..) {
            if new_horizon == INFINITY || op.local_vt < new_horizon {
                ready.push(op);
            } else {
                keep.push(op);
            }
        }
        self.pending = keep;
        ready.sort_by_key(SeqSend::key);
        self.horizon = new_horizon;
        ready
    }

    /// Whether the whole cluster has quiesced: no live connection can emit and
    /// nothing is left buffered. Mirrors the single-host scheduler's deadlock
    /// detection (design doc §8, F6).
    #[must_use]
    pub fn is_quiescent(&self) -> bool {
        self.pending.is_empty()
            && self
                .conns
                .values()
                .all(|c| c.effective_frontier() == INFINITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(n: u32, p: u16) -> VAddr {
        VAddr::new(0x7f00_0001 + n, p)
    }

    /// Drive a scripted sequence of admits/frontiers, then seal to completion,
    /// and return the fully-assigned order. `script` is a list of actions;
    /// this lets a test replay the *same logical ops* under different arrival
    /// interleavings.
    enum Act {
        Reg(ConnId, u32, u32),
        Send(ConnId, u64, u16), // conn, local_vt, payload tag
        Frontier(ConnId, u64),
        Seal,
    }

    fn run(width: u64, script: &[Act]) -> Vec<(ConnId, u64, u16, u64)> {
        let mut s = WindowSequencer::new(width);
        let mut out = Vec::new();
        let collect = |assigned: Vec<SeqSend>, out: &mut Vec<(ConnId, u64, u16, u64)>| {
            for op in assigned {
                let tag = u16::from(op.payload[0]);
                out.push((op.conn, op.local_vt, tag, op.conn_seq));
            }
        };
        for a in script {
            match a {
                Act::Reg(c, h, n) => s.register(*c, *h, *n),
                Act::Send(c, vt, tag) => {
                    #[allow(clippy::cast_possible_truncation)]
                    s.admit_send(*c, *vt, addr(0, 1), addr(1, 2), vec![*tag as u8])
                        .expect("admit");
                }
                Act::Frontier(c, f) => s.declare_frontier(*c, *f).expect("frontier"),
                Act::Seal => collect(s.seal(), &mut out),
            }
        }
        // Final drain: everyone releases, seal the rest.
        collect(s.seal(), &mut out);
        out
    }

    #[test]
    fn assignment_is_independent_of_arrival_order() {
        // Two connections on two hosts send into the same windows. The two
        // scripts admit the very same logical ops but interleave the two
        // connections' arrivals oppositely; the assigned order must match.
        let width = 100;
        let a_first = [
            Act::Reg(0, 0, 0),
            Act::Reg(1, 1, 1),
            Act::Send(0, 10, b'a'.into()),
            Act::Send(0, 60, b'b'.into()),
            Act::Send(1, 20, b'c'.into()),
            Act::Send(1, 90, b'd'.into()),
            Act::Frontier(0, 1000),
            Act::Frontier(1, 1000),
        ];
        let b_first = [
            Act::Reg(1, 1, 1),
            Act::Reg(0, 0, 0),
            Act::Send(1, 20, b'c'.into()),
            Act::Send(0, 10, b'a'.into()),
            Act::Send(1, 90, b'd'.into()),
            Act::Send(0, 60, b'b'.into()),
            Act::Frontier(1, 1000),
            Act::Frontier(0, 1000),
        ];
        let out_a = run(width, &a_first);
        let out_b = run(width, &b_first);
        assert_eq!(out_a, out_b, "arrival order changed the assignment");
        // And the order is the sort-key order: vt 10,20,60,90.
        let vts: Vec<u64> = out_a.iter().map(|t| t.1).collect();
        assert_eq!(vts, vec![10, 20, 60, 90]);
    }

    #[test]
    fn equal_local_vt_breaks_by_host_then_node() {
        // Three sends at the same local_vt from different (host,node): the key
        // orders them (host,node) ascending, arrival notwithstanding.
        let width = 100;
        let script = [
            Act::Reg(0, 2, 9),
            Act::Reg(1, 0, 5),
            Act::Reg(2, 0, 1),
            Act::Send(0, 50, b'x'.into()), // host 2
            Act::Send(1, 50, b'y'.into()), // host 0 node 5
            Act::Send(2, 50, b'z'.into()), // host 0 node 1
            Act::Frontier(0, 1000),
            Act::Frontier(1, 1000),
            Act::Frontier(2, 1000),
        ];
        let out = run(width, &script);
        let tags: Vec<u16> = out.iter().map(|t| t.2).collect();
        // (0,1)=z, (0,5)=y, (2,9)=x
        assert_eq!(
            tags,
            vec![u16::from(b'z'), u16::from(b'y'), u16::from(b'x')]
        );
    }

    #[test]
    fn incremental_sealing_matches_one_shot() {
        // Sealing after every action must yield the same total order as
        // sealing only at the end.
        let width = 100;
        let base = [
            Act::Reg(0, 0, 0),
            Act::Reg(1, 1, 1),
            Act::Send(0, 10, b'a'.into()),
            Act::Send(1, 20, b'b'.into()),
            Act::Frontier(0, 150),
            Act::Frontier(1, 150),
            Act::Send(0, 160, b'c'.into()),
            Act::Send(1, 170, b'd'.into()),
            Act::Frontier(0, 1000),
            Act::Frontier(1, 1000),
        ];
        let one_shot = run(width, &base);
        // Interleave Seal after each step.
        let mut stepwise_script = Vec::new();
        for a in base {
            stepwise_script.push(a);
            stepwise_script.push(Act::Seal);
        }
        let stepwise = run(width, &stepwise_script);
        assert_eq!(one_shot, stepwise);
        let vts: Vec<u64> = one_shot.iter().map(|t| t.1).collect();
        assert_eq!(vts, vec![10, 20, 160, 170]);
    }

    #[test]
    fn a_window_holds_until_the_laggard_frontier_crosses() {
        // Window 0 = [0,100). It must not seal while conn 1 is still below 100.
        let width = 100;
        let mut s = WindowSequencer::new(width);
        s.register(0, 0, 0);
        s.register(1, 1, 1);
        s.admit_send(0, 10, addr(0, 1), addr(1, 2), vec![1])
            .unwrap();
        s.declare_frontier(0, 500).unwrap(); // conn 0 way ahead
                                             // conn 1 still at frontier 0 → nothing seals.
        assert!(
            s.seal().is_empty(),
            "sealed with a laggard below the window"
        );
        assert_eq!(s.horizon(), 0);
        // conn 1 crosses the window boundary → window 0 seals.
        s.declare_frontier(1, 100).unwrap();
        let out = s.seal();
        assert_eq!(out.len(), 1);
        assert_eq!(s.horizon(), 100);
    }

    #[test]
    fn blocking_recv_releases_the_frontier() {
        // A blocked receiver must not stall sealing: once it releases, the
        // sender's already-buffered window seals.
        let width = 100;
        let mut s = WindowSequencer::new(width);
        s.register(0, 0, 0); // sender
        s.register(1, 1, 1); // receiver, will block
        s.admit_send(0, 30, addr(0, 1), addr(1, 2), vec![7])
            .unwrap();
        s.declare_frontier(0, 1000).unwrap();
        assert!(s.seal().is_empty(), "receiver at frontier 0 blocks sealing");
        s.block(1, 0); // receiver enters blocking recv -> effective frontier +inf
        let out = s.seal();
        assert_eq!(out.len(), 1, "release-on-block did not free the window");
    }

    #[test]
    fn wake_restores_the_quorum_at_the_delivery_time() {
        // A blocked receiver does not bound the horizon; once woken by a
        // delivery at 250 it rejoins the quorum at 250, bounding sealing to
        // window floor(250/100) = 200.
        let width = 100;
        let mut s = WindowSequencer::new(width);
        s.register(0, 0, 0); // sender
        s.register(1, 1, 1); // receiver
        s.block(1, 40);
        s.declare_frontier(0, 10_000).unwrap();
        s.seal();
        // Blocked receiver does not stall sealing: the sender alone bounds it.
        assert_eq!(s.horizon(), 10_000, "blocked receiver wrongly stalled sealing");

        let mut s2 = WindowSequencer::new(width);
        s2.register(0, 0, 0);
        s2.register(1, 1, 1);
        s2.block(1, 40);
        s2.wake(1, 250);
        s2.declare_frontier(0, 10_000).unwrap();
        s2.seal();
        assert_eq!(s2.horizon(), 200, "woken receiver must bound the horizon");
    }

    #[test]
    fn quiescence_when_all_release() {
        let width = 100;
        let mut s = WindowSequencer::new(width);
        s.register(0, 0, 0);
        s.admit_send(0, 5, addr(0, 1), addr(1, 2), vec![1]).unwrap();
        assert!(!s.is_quiescent());
        s.close(0);
        let out = s.seal();
        assert_eq!(out.len(), 1);
        assert_eq!(s.horizon(), INFINITY);
        assert!(s.is_quiescent());
    }

    #[test]
    fn non_monotone_local_vt_is_rejected() {
        let mut s = WindowSequencer::new(100);
        s.register(0, 0, 0);
        s.admit_send(0, 50, addr(0, 1), addr(1, 2), vec![1])
            .unwrap();
        let err = s
            .admit_send(0, 40, addr(0, 1), addr(1, 2), vec![2])
            .unwrap_err();
        assert_eq!(
            err,
            SeqError::NonMonotone {
                conn: 0,
                prev: 50,
                got: 40
            }
        );
    }

    #[test]
    fn op_in_a_sealed_window_is_rejected() {
        let mut s = WindowSequencer::new(100);
        s.register(0, 0, 0);
        s.register(1, 1, 1);
        s.declare_frontier(0, 300).unwrap();
        s.declare_frontier(1, 300).unwrap();
        s.seal(); // horizon → 300
        assert_eq!(s.horizon(), 300);
        let err = s
            .admit_send(1, 250, addr(0, 1), addr(1, 2), vec![1])
            .unwrap_err();
        assert!(
            matches!(err, SeqError::LateOp { horizon: 300, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn unknown_connection_is_rejected() {
        let mut s = WindowSequencer::new(100);
        let err = s
            .admit_send(9, 10, addr(0, 1), addr(1, 2), vec![1])
            .unwrap_err();
        assert_eq!(err, SeqError::UnknownConn(9));
    }
}
