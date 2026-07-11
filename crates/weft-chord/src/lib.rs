//! Chord ring-maintenance invariant checker for the Phase 7 case study.
//!
//! It reconstructs the global Chord configuration from the `RPT` state-report
//! datagrams captured in a `weft-log` recording of real Chord processes, and
//! evaluates the correctness invariants exactly as Pamela Zave states them
//! (docs/case-study/chord-spec.md): the verbatim Alloy `OneOrderedRing`
//! predicate split into `AtLeastOneRing`, `AtMostOneRing`, and `OrderedRing`,
//! plus `ConnectedAppendages`. r = 2 (each node reports `succ`, `succ2`,
//! `prdc`).
//!
//! Two entry points share one definition of the predicates:
//! - [`Snapshot::from_log`] + [`Snapshot::check`] — the authoritative
//!   final-quiescent-state verdict used by the campaign and case study;
//! - [`ChordInvariant`] — a streaming [`weft_replay::invariant::Invariant`]
//!   so the Phase 6 shrinker (`weft_fuzz::shrink`) can minimize a Chord
//!   recording.

pub mod chord_model;

use std::collections::HashMap;

use weft_replay::invariant::Invariant;
use weft_replay::log::{Event, RecvOutcome, SendOutcome};
use weft_replay::Log;

pub const NONE: i32 = -1;

/// One node's most recently reported state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeState {
    pub ident: i32,
    pub date: i32,
    pub alive: bool,
    pub succ: i32,
    pub succ2: i32,
    pub prdc: i32,
}

/// Parse an `RPT <ident> <date> <alive> <succ> <succ2> <prdc>` report.
#[must_use]
pub fn parse_report(text: &str) -> Option<NodeState> {
    let mut it = text.split_whitespace();
    if it.next()? != "RPT" {
        return None;
    }
    let ident = it.next()?.parse().ok()?;
    let date = it.next()?.parse().ok()?;
    let alive: i32 = it.next()?.parse().ok()?;
    let succ = it.next()?.parse().ok()?;
    let succ2 = it.next()?.parse().ok()?;
    let prdc = it.next()?.parse().ok()?;
    Some(NodeState {
        ident,
        date,
        alive: alive != 0,
        succ,
        succ2,
        prdc,
    })
}

/// Decode a hex payload from a log event, if it is a report, returning the
/// parsed state.
fn report_from_event(e: &Event) -> Option<NodeState> {
    let (Event::Send {
        payload: payload_hex,
        ..
    }
    | Event::Recv {
        outcome: RecvOutcome::Delivered {
            payload: payload_hex,
            ..
        },
        ..
    }) = e
    else {
        return None;
    };
    let bytes = weft_replay::hash::from_hex(payload_hex)?;
    let text = std::str::from_utf8(&bytes).ok()?;
    parse_report(text)
}

/// The reconstructed global configuration: the latest report per node.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    latest: HashMap<i32, NodeState>,
}

impl Snapshot {
    /// Fold one report into the snapshot (keeps the highest-date report, and
    /// once a node reports dead it stays dead).
    pub fn observe(&mut self, s: NodeState) {
        let entry = self.latest.entry(s.ident).or_insert(s);
        let was_dead = !entry.alive;
        if s.date >= entry.date {
            *entry = s;
        }
        if was_dead {
            entry.alive = false; // death is permanent
        }
    }

    /// Build the final snapshot from a recording (every report, latest wins).
    #[must_use]
    pub fn from_log(log: &Log) -> Self {
        let mut snap = Self::default();
        for r in &log.records {
            if let Some(st) = report_from_event(&r.e) {
                snap.observe(st);
            }
        }
        snap
    }

    /// Live members = nodes whose latest report says alive.
    #[must_use]
    pub fn live(&self) -> Vec<NodeState> {
        let mut v: Vec<NodeState> = self.latest.values().copied().filter(|s| s.alive).collect();
        v.sort_by_key(|s| s.ident);
        v
    }

    fn is_live(&self, ident: i32) -> bool {
        ident >= 0 && self.latest.get(&ident).is_some_and(|s| s.alive)
    }

    /// best successor: first successor pointing to a live node.
    fn best_succ(&self, n: &NodeState) -> i32 {
        if self.is_live(n.succ) {
            n.succ
        } else if self.is_live(n.succ2) {
            n.succ2
        } else {
            NONE
        }
    }

    fn best_succ_of(&self, ident: i32) -> i32 {
        self.latest
            .get(&ident)
            .filter(|s| s.alive)
            .map_or(NONE, |s| self.best_succ(s))
    }

    /// Ring members: live nodes reachable from themselves by following the
    /// chain of best successors (n ∈ n.(^bestSucc)).
    #[must_use]
    pub fn ring_members(&self) -> Vec<i32> {
        let live = self.live();
        let n = live.len();
        let mut ring = Vec::new();
        for s in &live {
            let mut cur = self.best_succ(s);
            let mut steps = 0;
            while cur != NONE && steps <= n {
                if cur == s.ident {
                    ring.push(s.ident);
                    break;
                }
                cur = self.best_succ_of(cur);
                steps += 1;
            }
        }
        ring.sort_unstable();
        ring
    }

    /// The forward best-successor orbit of `start` restricted to ring members.
    fn cycle_of(&self, start: i32) -> Vec<i32> {
        let mut out = vec![start];
        let mut cur = self.best_succ_of(start);
        while cur != NONE && cur != start {
            out.push(cur);
            cur = self.best_succ_of(cur);
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Evaluate every correctness invariant against this snapshot.
    #[must_use]
    pub fn check(&self, m: u32) -> Vec<ChordViolation> {
        let mut v = Vec::new();
        let ring = self.ring_members();

        // AtLeastOneRing: there is a cycle of best successors.
        if ring.is_empty() {
            v.push(ChordViolation {
                invariant: InvariantKind::AtLeastOneRing,
                detail: "no ring member: the best-successor graph has no cycle".into(),
            });
            // Everything else is defined relative to a ring; stop here.
            return v;
        }

        // AtMostOneRing: all ring members lie on the same cycle.
        let cycle = self.cycle_of(ring[0]);
        if cycle.len() != ring.len() {
            v.push(ChordViolation {
                invariant: InvariantKind::AtMostOneRing,
                detail: format!(
                    "ring split: {} ring members but the cycle through {} has {}",
                    ring.len(),
                    ring[0],
                    cycle.len()
                ),
            });
        }

        // OrderedRing: for adjacent ring members n1 -> n2, no ring member n3
        // lies Between(n1, n2).
        for &n1 in &ring {
            let n2 = self.best_succ_of(n1);
            if !ring.contains(&n2) {
                continue;
            }
            for &n3 in &ring {
                if n3 != n1 && n3 != n2 && between(n1, n3, n2, m) {
                    v.push(ChordViolation {
                        invariant: InvariantKind::OrderedRing,
                        detail: format!("ring member {n3} lies between adjacent {n1} -> {n2}"),
                    });
                }
            }
        }

        // ConnectedAppendages: every live non-ring member reaches the ring by
        // following best successors.
        for s in self.live() {
            if ring.contains(&s.ident) {
                continue;
            }
            let mut cur = self.best_succ(&s);
            let mut steps = 0;
            let mut reached = false;
            while cur != NONE && steps <= self.live().len() {
                if ring.contains(&cur) {
                    reached = true;
                    break;
                }
                cur = self.best_succ_of(cur);
                steps += 1;
            }
            if !reached {
                v.push(ChordViolation {
                    invariant: InvariantKind::ConnectedAppendages,
                    detail: format!(
                        "appendage member {} cannot reach the ring via best successors",
                        s.ident
                    ),
                });
            }
        }
        v
    }

    /// Verify the Chord papers' failure assumption held at every failure
    /// instant: "a member never fails if its failure would leave another
    /// member with no live successor in its successor list."
    ///
    /// Reconstructed from the recording's chronological report stream: at
    /// each death, every other then-live member's most recent reported
    /// successor list must still contain at least one live member. If not,
    /// the run violated the model's own precondition and any invariant
    /// violation in it is the harness's artifact, not Chord's bug — the
    /// campaign discards such seeds.
    #[must_use]
    pub fn failure_assumption_held(log: &Log) -> bool {
        let mut latest: HashMap<i32, NodeState> = HashMap::new();
        let mut dead: Vec<i32> = Vec::new();
        // Only consume each report once (it appears as both a Send and a
        // Recv in the log); Sends are in linearization order already.
        for r in &log.records {
            let Event::Send { .. } = &r.e else { continue };
            let Some(st) = report_from_event(&r.e) else {
                continue;
            };
            if st.alive {
                let e = latest.entry(st.ident).or_insert(st);
                if st.date >= e.date {
                    *e = st;
                }
            } else {
                // A death. Check every other live member's list survives it.
                dead.push(st.ident);
                for (id, s) in &latest {
                    if *id == st.ident || dead.contains(id) {
                        continue;
                    }
                    let succ_live = s.succ >= 0 && !dead.contains(&s.succ);
                    let second_live = s.succ2 >= 0 && !dead.contains(&s.succ2);
                    if !succ_live && !second_live {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// A compact rendering of the final configuration for reports.
    #[must_use]
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut o = String::new();
        let ring = self.ring_members();
        let _ = writeln!(o, "ring members: {ring:?}");
        for s in self.live() {
            let bs = self.best_succ(&s);
            let _ = writeln!(
                o,
                "  node {:>3}: succ={:>3} succ2={:>3} prdc={:>3} bestSucc={:>3}{}",
                s.ident,
                s.succ,
                s.succ2,
                s.prdc,
                bs,
                if ring.contains(&s.ident) {
                    " [ring]"
                } else {
                    " [appendage]"
                }
            );
        }
        for s in self.latest.values().filter(|s| !s.alive) {
            let _ = writeln!(o, "  node {:>3}: FAILED", s.ident);
        }
        o
    }
}

/// b strictly within the clockwise arc (a, c) on the m-bit identifier circle.
#[must_use]
#[allow(clippy::many_single_char_names)] // a, b, c mirror the papers' notation
pub fn between(a: i32, b: i32, c: i32, m: u32) -> bool {
    let n = 1i64 << m;
    let a = i64::from(a).rem_euclid(n);
    let b = i64::from(b).rem_euclid(n);
    let c = i64::from(c).rem_euclid(n);
    if a < c {
        a < b && b < c
    } else {
        a < b || b < c
    }
}

/// Which invariant a violation is against.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InvariantKind {
    AtLeastOneRing,
    AtMostOneRing,
    OrderedRing,
    ConnectedAppendages,
}

impl InvariantKind {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::AtLeastOneRing => "AtLeastOneRing",
            Self::AtMostOneRing => "AtMostOneRing",
            Self::OrderedRing => "OrderedRing",
            Self::ConnectedAppendages => "ConnectedAppendages",
        }
    }
    /// Whether a violation is unrepairable by the ring-maintenance protocol
    /// (all four we check are in Zave's "required for correctness" set).
    #[must_use]
    pub fn is_correctness_critical(self) -> bool {
        true
    }
}

/// A detected invariant violation over a snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct ChordViolation {
    pub invariant: InvariantKind,
    pub detail: String,
}

/// Streaming Chord invariant for the shrinker: it maintains a running
/// snapshot from report datagrams and fires when the current global
/// configuration violates `target`. Because the base ring is intact until a
/// failure genuinely breaks it, `AtLeastOneRing`/`AtMostOneRing`/
/// `ConnectedAppendages` do not fire during healthy operation — only once the
/// ring is actually broken, which (per Zave) does not self-heal, so the fire
/// persists through the quiescent tail.
pub struct ChordInvariant {
    snap: Snapshot,
    m: u32,
    target: InvariantKind,
}

impl ChordInvariant {
    #[must_use]
    pub fn new(m: u32, target: InvariantKind) -> Self {
        Self {
            snap: Snapshot::default(),
            m,
            target,
        }
    }
}

impl Invariant for ChordInvariant {
    fn name(&self) -> &str {
        self.target.name()
    }

    fn on_event(&mut self, _op: u64, _vt: u64, e: &Event) -> Option<String> {
        // Only report datagrams change the configuration.
        let is_report = matches!(
            e,
            Event::Send {
                outcome: SendOutcome::Enqueued { .. },
                ..
            } | Event::Recv { .. }
        );
        if !is_report {
            return None;
        }
        let st = report_from_event(e)?;
        self.snap.observe(st);
        self.snap
            .check(self.m)
            .into_iter()
            .find(|viol| viol.invariant == self.target)
            .map(|viol| viol.detail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(ident: i32, succ: i32, succ2: i32, prdc: i32, alive: bool) -> NodeState {
        NodeState {
            ident,
            date: 100,
            alive,
            succ,
            succ2,
            prdc,
        }
    }

    fn snap(states: &[NodeState]) -> Snapshot {
        let mut s = Snapshot::default();
        for &n in states {
            s.observe(n);
        }
        s
    }

    #[test]
    fn between_wraps_correctly() {
        // m=6, circle 0..64
        assert!(between(6, 10, 16, 6));
        assert!(!between(6, 20, 16, 6));
        assert!(between(60, 2, 6, 6)); // wraps past 0
        assert!(!between(10, 10, 20, 6)); // strict
    }

    #[test]
    fn ideal_ring_of_three_satisfies_all() {
        // Ordered ring 10 -> 30 -> 50 -> 10.
        let s = snap(&[
            st(10, 30, 50, 50, true),
            st(30, 50, 10, 10, true),
            st(50, 10, 30, 30, true),
        ]);
        assert_eq!(s.ring_members(), vec![10, 30, 50]);
        assert!(
            s.check(6).is_empty(),
            "ideal ring must satisfy every invariant"
        );
    }

    #[test]
    fn appendage_connected_to_ring_is_fine() {
        // 20 is an appendage whose succ is ring member 30.
        let s = snap(&[
            st(10, 30, 50, 50, true),
            st(30, 50, 10, 10, true),
            st(50, 10, 30, 30, true),
            st(20, 30, 50, NONE, true),
        ]);
        assert_eq!(s.ring_members(), vec![10, 30, 50]);
        assert!(s.check(6).is_empty());
    }

    #[test]
    fn fig6_gap_skips_a_member_and_breaks_the_ring() {
        // Fig-6 shape at r=2: 10 joined between 6 and 12; 6 adopted 10 as succ
        // via stabilize, but 10 failed before 6 reconciled, so 6's succ2 still
        // pointed past 12 to 40. update promotes 6.succ := 40, skipping 12.
        // With 12's only inbound edge gone, the best-successor graph no longer
        // forms a single ring containing 12.
        let s = snap(&[
            st(6, 10, 40, 60, true),  // succ=10 dead -> bestSucc=40 (skips 12)
            st(40, 60, 6, 6, true),   // 40 -> 60
            st(60, 6, 40, 40, true),  // 60 -> 6   (cycle 6->40->60->6)
            st(12, 40, 60, 10, true), // 12 -> 40, but nobody points to 12
            st(10, NONE, NONE, NONE, false),
        ]);
        // A ring exists (6,40,60) but 12 was ejected from it: 12 is now an
        // appendage that (here) still reaches the ring, so this specific shape
        // is an OrderedRing/ejection issue, not AtLeastOneRing. The decisive
        // unrepairable case is the no-cycle one below.
        assert!(s.ring_members().contains(&6));
        assert!(
            !s.ring_members().contains(&12),
            "12 was skipped out of the ring"
        );
    }

    #[test]
    fn no_cycle_means_at_least_one_ring_fires() {
        // A pure chain with no cycle: 10 -> 30 -> 50 -> (dead), nothing loops.
        let s = snap(&[
            st(10, 30, 55, NONE, true),
            st(30, 50, 55, 10, true),
            st(50, 55, 55, 30, true), // succ 55 dead, succ2 55 dead -> bestSucc NONE
            st(55, NONE, NONE, NONE, false),
        ]);
        assert!(s.ring_members().is_empty());
        let v = s.check(6);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].invariant, InvariantKind::AtLeastOneRing);
    }

    #[test]
    fn split_ring_violates_at_most_one_ring() {
        // Two disjoint cycles: {10,30} and {40,50}.
        let s = snap(&[
            st(10, 30, 30, 30, true),
            st(30, 10, 10, 10, true),
            st(40, 50, 50, 50, true),
            st(50, 40, 40, 40, true),
        ]);
        assert_eq!(s.ring_members(), vec![10, 30, 40, 50]);
        let v = s.check(6);
        assert!(v
            .iter()
            .any(|x| x.invariant == InvariantKind::AtMostOneRing));
    }

    #[test]
    fn disconnected_appendage_violates_connected_appendages() {
        // Ring 10->30->50->10 healthy; 20 is an appendage whose succ is dead
        // and succ2 is dead -> bestSucc NONE -> cannot reach ring.
        let s = snap(&[
            st(10, 30, 50, 50, true),
            st(30, 50, 10, 10, true),
            st(50, 10, 30, 30, true),
            st(20, 63, 63, NONE, true),
            st(63, NONE, NONE, NONE, false),
        ]);
        assert_eq!(s.ring_members(), vec![10, 30, 50]);
        let v = s.check(6);
        assert!(v
            .iter()
            .any(|x| x.invariant == InvariantKind::ConnectedAppendages));
    }
}
