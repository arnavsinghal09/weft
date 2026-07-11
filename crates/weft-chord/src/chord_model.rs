//! Explicit-state model of the Chord ring-maintenance protocol under
//! **synchronous, perfect failure detection** — the formal oracle for
//! cross-validating the dynamic engine's findings.
//!
//! This is a stateright port of the semantics in
//! docs/case-study/chord-spec.md (Zave's shared-state Alloy model of the
//! 2001 [SIGCOMM] protocol plus [PODC] recovery), with the *same* fix-level
//! gating as `examples/chord/chord_node.c` and the *same* four correctness
//! invariants as the dynamic checker in `lib.rs`. The two deliberate
//! divergences from the dynamic harness, both inherent to the oracle's
//! purpose:
//!
//! 1. **Perfect failure detection**: `live()` is ground truth, visible to
//!    every operation instantly. The harness's DEAD-broadcast detection
//!    latency does not exist here. A violation that *requires* in-flight
//!    death notices is therefore unreachable in this model — that is
//!    exactly the discriminating power the cross-validation uses.
//! 2. **Synchronous atomic operations**: each protocol operation reads and
//!    writes global state atomically (Zave's shared-state abstraction).
//!    `stabilize` folds the notify side-effect into the same atomic step
//!    (per the operation list in chord-spec.md). This granularity is
//!    coarser than the harness's real message passing; the cross-validation
//!    doc states it, and the level-0 confirmations empirically bound how
//!    much it costs.
//!
//! Fault schedules are **not** modeled nondeterministically by default:
//! each oracle run replays the join/fail sequence extracted from one
//! recorded violation, exploring all interleavings of protocol steps
//! around that fixed schedule. `Schedule::exhaustive()` lifts that
//! restriction for the MODEL_ONLY sweep.

use stateright::{Model, Property};

use crate::between;

pub const NONE: i8 = -1;

/// Node lifecycle in the model (ground truth, globally visible).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Status {
    /// Not yet joined (appendage waiting on its schedule slot).
    Out,
    /// Live member.
    Member,
    /// Failed (permanent).
    Dead,
}

/// One node's pointers. `i8` keeps the state fingerprint small; idents in
/// the case study are < 64 (m = 6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NState {
    pub status: Status,
    pub succ: i8,
    pub succ2: i8,
    pub prdc: i8,
}

/// A scheduled fault event, in recording order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SchedEvent {
    Join(i8),
    Fail(i8),
}

/// The fault schedule the model replays.
#[derive(Clone, Debug)]
pub enum Schedule {
    /// The join/fail sequence extracted from one recording, in order.
    /// Protocol steps interleave freely; schedule order is fixed.
    Fixed(Vec<SchedEvent>),
    /// MODEL_ONLY sweep: every appendage may join at any time and fail at
    /// any later time (failure-assumption-gated), in any order.
    Exhaustive,
}

/// Global model state: pointers per ident (sorted order fixed by the
/// model's ident list) plus the schedule cursor.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChordState {
    pub nodes: Vec<NState>,
    /// Next unapplied index into a `Schedule::Fixed` (unused for
    /// `Exhaustive`, where per-node `status` carries the information).
    pub sched_pos: u8,
    /// Exhaustive mode only: how many fails have been injected.
    pub fails_done: u8,
}

/// One atomic protocol or schedule step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChordAction {
    /// Apply the next scheduled join, bootstrapping via member index `via`
    /// (Zave's JoinEvent: nondeterministic over valid insertion points).
    Join { node: u8, via: u8 },
    /// Apply the next scheduled failure.
    Fail { node: u8 },
    /// stabilize(n) + the notified() effect on the notify target, atomic.
    Stabilize { node: u8 },
    /// reconcile(n): adopt target's reported successor as succ2.
    Reconcile { node: u8 },
    /// update(n): promote succ2 past a dead succ.
    Update { node: u8 },
    /// flush(n): drop a dead predecessor.
    Flush { node: u8 },
}

/// The model: fixed ident universe, base ring, fix level, schedule.
pub struct ChordModel {
    pub m: u32,
    /// All member idents, sorted ascending; indices into `ChordState.nodes`.
    pub idents: Vec<i8>,
    /// Idents that start as the ideal base ring.
    pub base: Vec<i8>,
    /// Liveness-discipline level, identical to `CHORD_FIX` in chord_node.c.
    pub fix: u8,
    pub schedule: Schedule,
    /// Exhaustive mode: how many appendage failures to inject.
    pub max_fails: u8,
}

impl ChordModel {
    fn idx(&self, ident: i8) -> usize {
        self.idents.iter().position(|&i| i == ident).expect("ident")
    }

    fn live(&self, s: &ChordState, ident: i8) -> bool {
        ident >= 0
            && self
                .idents
                .iter()
                .position(|&i| i == ident)
                .is_some_and(|k| s.nodes[k].status == Status::Member)
    }

    /// bestSucc: first successor pointing to a live node (ground truth).
    fn best_succ(&self, s: &ChordState, k: usize) -> i8 {
        let n = &s.nodes[k];
        if self.live(s, n.succ) {
            n.succ
        } else if self.live(s, n.succ2) {
            n.succ2
        } else {
            NONE
        }
    }

    fn btw(&self, a: i8, b: i8, c: i8) -> bool {
        between(i32::from(a), i32::from(b), i32::from(c), self.m)
    }

    /// Ring members: live nodes on a bestSucc cycle (n ∈ n.(^bestSucc)).
    fn ring_members(&self, s: &ChordState) -> Vec<i8> {
        let n_live = s
            .nodes
            .iter()
            .filter(|n| n.status == Status::Member)
            .count();
        let mut ring = Vec::new();
        for (k, ns) in s.nodes.iter().enumerate() {
            if ns.status != Status::Member {
                continue;
            }
            let me = self.idents[k];
            let mut cur = self.best_succ(s, k);
            let mut steps = 0;
            while cur != NONE && steps <= n_live {
                if cur == me {
                    ring.push(me);
                    break;
                }
                cur = self.best_succ(s, self.idx(cur));
                steps += 1;
            }
        }
        ring
    }

    /// Evaluate the four correctness invariants; true = all hold.
    /// Mirrors `Snapshot::check` in lib.rs, over ground-truth liveness.
    #[must_use]
    pub fn invariants_hold(&self, s: &ChordState) -> bool {
        self.violated_invariants(s).is_empty()
    }

    /// Names of violated invariants (same set the dynamic checker reports).
    #[must_use]
    pub fn violated_invariants(&self, s: &ChordState) -> Vec<&'static str> {
        let mut out = Vec::new();
        let ring = self.ring_members(s);

        if ring.is_empty() {
            // Defined relative to a ring; match the dynamic checker: stop.
            out.push("AtLeastOneRing");
            return out;
        }

        // AtMostOneRing: the cycle through ring[0] covers every ring member.
        let mut cycle = vec![ring[0]];
        let mut cur = self.best_succ(s, self.idx(ring[0]));
        while cur != NONE && cur != ring[0] {
            cycle.push(cur);
            cur = self.best_succ(s, self.idx(cur));
        }
        if cycle.len() != ring.len() {
            out.push("AtMostOneRing");
        }

        // OrderedRing: no ring member strictly between adjacent ring members.
        'ordered: for &n1 in &ring {
            let n2 = self.best_succ(s, self.idx(n1));
            if !ring.contains(&n2) {
                continue;
            }
            for &n3 in &ring {
                if n3 != n1 && n3 != n2 && self.btw(n1, n3, n2) {
                    out.push("OrderedRing");
                    break 'ordered;
                }
            }
        }

        // ConnectedAppendages: every live non-ring member reaches the ring.
        let n_live = s
            .nodes
            .iter()
            .filter(|n| n.status == Status::Member)
            .count();
        for (k, ns) in s.nodes.iter().enumerate() {
            if ns.status != Status::Member || ring.contains(&self.idents[k]) {
                continue;
            }
            let mut cur = self.best_succ(s, k);
            let mut steps = 0;
            let mut reached = false;
            while cur != NONE && steps <= n_live {
                if ring.contains(&cur) {
                    reached = true;
                    break;
                }
                cur = self.best_succ(s, self.idx(cur));
                steps += 1;
            }
            if !reached {
                out.push("ConnectedAppendages");
                break;
            }
        }
        out
    }

    /// The papers' failure assumption, evaluated on ground truth: failing
    /// `victim` must not leave any *other* live member with an all-dead
    /// successor list. (Same rule `Snapshot::failure_assumption_held`
    /// enforces over recordings; Zave's models fail nodes only under it.)
    fn failure_assumption_ok(&self, s: &ChordState, victim: i8) -> bool {
        for (k, ns) in s.nodes.iter().enumerate() {
            if ns.status != Status::Member || self.idents[k] == victim {
                continue;
            }
            let live_after = |p: i8| p != NONE && p != victim && self.live(s, p);
            if !live_after(ns.succ) && !live_after(ns.succ2) {
                return false;
            }
        }
        true
    }

    /// Whether the fault schedule has fully played out in `s`.
    #[must_use]
    pub fn schedule_done(&self, s: &ChordState) -> bool {
        match &self.schedule {
            Schedule::Fixed(ev) => usize::from(s.sched_pos) >= ev.len(),
            Schedule::Exhaustive => {
                s.fails_done >= self.max_fails && s.nodes.iter().all(|n| n.status != Status::Out)
            }
        }
    }

    fn push_join_actions(&self, s: &ChordState, joiner: i8, actions: &mut Vec<ChordAction>) {
        // Zave JoinEvent: any member m with Between(m, joiner, m.succ) and
        // Member(m.succ) is a valid insertion point.
        let jk = self.idx(joiner);
        for (k, ns) in s.nodes.iter().enumerate() {
            if ns.status != Status::Member {
                continue;
            }
            if ns.succ != NONE && self.live(s, ns.succ) && self.btw(self.idents[k], joiner, ns.succ)
            {
                #[allow(clippy::cast_possible_truncation)]
                actions.push(ChordAction::Join {
                    node: jk as u8,
                    via: k as u8,
                });
            }
        }
    }
}

impl Model for ChordModel {
    type State = ChordState;
    type Action = ChordAction;

    fn init_states(&self) -> Vec<Self::State> {
        // Base idents form the ideal ordered ring (init_base_pointers in
        // chord_node.c); appendages are Out.
        let nb = self.base.len();
        let nodes = self
            .idents
            .iter()
            .map(|&id| {
                if let Some(i) = self.base.iter().position(|&b| b == id) {
                    NState {
                        status: Status::Member,
                        succ: self.base[(i + 1) % nb],
                        succ2: self.base[(i + 2) % nb],
                        prdc: self.base[(i + nb - 1) % nb],
                    }
                } else {
                    NState {
                        status: Status::Out,
                        succ: NONE,
                        succ2: NONE,
                        prdc: NONE,
                    }
                }
            })
            .collect();
        vec![ChordState {
            nodes,
            sched_pos: 0,
            fails_done: 0,
        }]
    }

    #[allow(clippy::too_many_lines, clippy::cast_possible_truncation)]
    fn actions(&self, s: &Self::State, actions: &mut Vec<Self::Action>) {
        // Schedule actions.
        match &self.schedule {
            Schedule::Fixed(ev) => {
                if let Some(e) = ev.get(usize::from(s.sched_pos)) {
                    match *e {
                        SchedEvent::Join(j) => {
                            if s.nodes[self.idx(j)].status == Status::Out {
                                self.push_join_actions(s, j, actions);
                            }
                        }
                        SchedEvent::Fail(f) => {
                            if s.nodes[self.idx(f)].status == Status::Member
                                && self.failure_assumption_ok(s, f)
                            {
                                actions.push(ChordAction::Fail {
                                    node: self.idx(f) as u8,
                                });
                            }
                        }
                    }
                }
            }
            Schedule::Exhaustive => {
                for (k, ns) in s.nodes.iter().enumerate() {
                    match ns.status {
                        Status::Out => self.push_join_actions(s, self.idents[k], actions),
                        Status::Member
                            if !self.base.contains(&self.idents[k])
                                && s.fails_done < self.max_fails
                                && self.failure_assumption_ok(s, self.idents[k]) =>
                        {
                            actions.push(ChordAction::Fail { node: k as u8 });
                        }
                        _ => {}
                    }
                }
            }
        }

        // Protocol actions for every live member — emitted only when they
        // would change state, so quiescent configurations terminate BFS.
        for (k, ns) in s.nodes.iter().enumerate() {
            if ns.status != Status::Member {
                continue;
            }
            let me = self.idents[k];
            let bs = self.best_succ(s, k);

            // stabilize: target = live(succ) ? succ : bestSucc (as in the C).
            let target = if self.live(s, ns.succ) { ns.succ } else { bs };
            if target != NONE && self.live(s, target) {
                // Would the adopt or the notify change anything?
                let p = s.nodes[self.idx(target)].prdc;
                let live_ok = self.fix < 1 || self.live(s, p);
                let adopts = p != NONE && live_ok && bs != NONE && self.btw(me, p, bs);
                let new_succ = if adopts { p } else { ns.succ };
                let notify_to = if new_succ == NONE { bs } else { new_succ };
                let notify_changes = notify_to != NONE
                    && self.live(s, notify_to)
                    && {
                        let t = &s.nodes[self.idx(notify_to)];
                        t.prdc == NONE
                            || !self.live(s, t.prdc)
                            || self.btw(t.prdc, me, self.idents[self.idx(notify_to)])
                    }
                    && s.nodes[self.idx(notify_to)].prdc != me;
                if (adopts && new_succ != ns.succ) || notify_changes {
                    actions.push(ChordAction::Stabilize { node: k as u8 });
                }
            }

            // reconcile: target = live(succ) ? succ : bestSucc; reply is
            // target.succ at fix<2, target.bestSucc at fix>=2.
            if target != NONE && self.live(s, target) {
                let tk = self.idx(target);
                let a2 = if self.fix >= 2 {
                    self.best_succ(s, tk)
                } else {
                    s.nodes[tk].succ
                };
                if a2 != NONE && (self.fix < 2 || self.live(s, a2)) && ns.succ2 != a2 {
                    actions.push(ChordAction::Reconcile { node: k as u8 });
                }
            }

            // update: promote succ2 past a dead succ.
            if !self.live(s, ns.succ)
                && ns.succ2 != NONE
                && (self.fix < 2 || self.live(s, ns.succ2))
                && ns.succ != ns.succ2
            {
                actions.push(ChordAction::Update { node: k as u8 });
            }

            // flush: drop a dead predecessor.
            if ns.prdc != NONE && !self.live(s, ns.prdc) {
                actions.push(ChordAction::Flush { node: k as u8 });
            }
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    // `s`/`a` (state/action) are the naming idiom of stateright's Model
    // trait, used consistently across this file's other trait methods.
    #[allow(clippy::many_single_char_names)]
    fn next_state(&self, s: &Self::State, a: Self::Action) -> Option<Self::State> {
        let mut ns = s.clone();
        match a {
            ChordAction::Join { node, via } => {
                let succ_of_via = s.nodes[usize::from(via)].succ;
                let n = &mut ns.nodes[usize::from(node)];
                n.status = Status::Member;
                n.succ = succ_of_via;
                n.succ2 = NONE;
                n.prdc = NONE;
                if matches!(self.schedule, Schedule::Fixed(_)) {
                    ns.sched_pos += 1;
                }
            }
            ChordAction::Fail { node } => {
                ns.nodes[usize::from(node)].status = Status::Dead;
                if matches!(self.schedule, Schedule::Fixed(_)) {
                    ns.sched_pos += 1;
                } else {
                    ns.fails_done += 1;
                }
            }
            ChordAction::Stabilize { node } => {
                let k = usize::from(node);
                let me = self.idents[k];
                let bs = self.best_succ(s, k);
                let target = if self.live(s, s.nodes[k].succ) {
                    s.nodes[k].succ
                } else {
                    bs
                };
                let p = s.nodes[self.idx(target)].prdc;
                let live_ok = self.fix < 1 || self.live(s, p);
                if p != NONE && live_ok && bs != NONE && self.btw(me, p, bs) {
                    ns.nodes[k].succ = p;
                }
                // notified() at the (possibly new) successor, atomically.
                let cur_succ = ns.nodes[k].succ;
                let notify_to = if cur_succ == NONE { bs } else { cur_succ };
                if notify_to != NONE && self.live(s, notify_to) {
                    let tk = self.idx(notify_to);
                    let t = &mut ns.nodes[tk];
                    if t.prdc == NONE || !self.live(s, t.prdc) || self.btw(t.prdc, me, notify_to) {
                        t.prdc = me;
                    }
                }
            }
            ChordAction::Reconcile { node } => {
                let k = usize::from(node);
                let bs = self.best_succ(s, k);
                let target = if self.live(s, s.nodes[k].succ) {
                    s.nodes[k].succ
                } else {
                    bs
                };
                let tk = self.idx(target);
                let a2 = if self.fix >= 2 {
                    self.best_succ(s, tk)
                } else {
                    s.nodes[tk].succ
                };
                if a2 != NONE && (self.fix < 2 || self.live(s, a2)) {
                    ns.nodes[k].succ2 = a2;
                }
            }
            ChordAction::Update { node } => {
                let k = usize::from(node);
                ns.nodes[k].succ = s.nodes[k].succ2;
            }
            ChordAction::Flush { node } => {
                ns.nodes[usize::from(node)].prdc = NONE;
            }
        }
        (ns != *s).then_some(ns)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            // Reachability of a post-schedule invariant violation: the
            // question the cross-validation asks. "sometimes" = the checker
            // hunts for one example.
            Property::<Self>::sometimes("violation-after-schedule", |m, s| {
                m.schedule_done(s) && !m.invariants_hold(s)
            }),
            Property::<Self>::sometimes("alor-after-schedule", |m, s| {
                m.schedule_done(s) && m.violated_invariants(s).contains(&"AtLeastOneRing")
            }),
            Property::<Self>::sometimes("connapp-after-schedule", |m, s| {
                m.schedule_done(s) && m.violated_invariants(s).contains(&"ConnectedAppendages")
            }),
            Property::<Self>::sometimes("ordered-or-split-after-schedule", |m, s| {
                m.schedule_done(s) && {
                    let v = m.violated_invariants(s);
                    v.contains(&"OrderedRing") || v.contains(&"AtMostOneRing")
                }
            }),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stateright::Checker;

    fn model(fix: u8, schedule: Schedule) -> ChordModel {
        // The case-study universe: base {1,22,43}, appendages {4,25,46}, m=6.
        ChordModel {
            m: 6,
            idents: vec![1, 4, 22, 25, 43, 46],
            base: vec![1, 22, 43],
            fix,
            schedule,
            max_fails: 3,
        }
    }

    #[test]
    fn base_ring_alone_is_quiescent_and_correct() {
        let m = model(0, Schedule::Fixed(vec![]));
        let init = &m.init_states()[0];
        assert!(m.invariants_hold(init));
        // Ideal ring: only prdc-notify churn at most; BFS must terminate
        // quickly with no violation discovered.
        let checker = m.checker().spawn_bfs().join();
        assert!(checker.discovery("violation-after-schedule").is_none());
    }

    #[test]
    fn level0_join_fail_schedule_can_break_the_ring() {
        // A Zave-style schedule: appendages join, then fail; at fix level 0
        // some interleaving must reach a post-schedule violation (Figure 6's
        // mechanism exists under perfect detection).
        let m = model(
            0,
            Schedule::Fixed(vec![
                SchedEvent::Join(4),
                SchedEvent::Join(25),
                SchedEvent::Join(46),
                SchedEvent::Fail(25),
                SchedEvent::Fail(4),
                SchedEvent::Fail(46),
            ]),
        );
        let checker = m.checker().spawn_bfs().join();
        assert!(
            checker.discovery("violation-after-schedule").is_some(),
            "level 0 must be breakable under perfect detection (Zave)"
        );
    }

    #[test]
    fn level2_perfect_detection_excludes_ring_loss_but_not_splits() {
        // Full liveness discipline with ground-truth liveness. Two distinct
        // facts, both pinned:
        //
        // (a) AtLeastOneRing / ConnectedAppendages violations are
        //     EXHAUSTIVELY unreachable — losing every live pointer requires
        //     adopting a node that is already dead, impossible with
        //     ground-truth liveness checks. This is the theorem the dynamic
        //     level-2 residual is measured against.
        //
        // (b) AtMostOneRing / OrderedRing violations REMAIN reachable:
        //     liveness discipline alone does not prevent a node from
        //     holding itself as succ2 (legal in a 2-ring) and degenerating
        //     into a disjoint self-ring when its first successor dies.
        //     Zave's actually-proven-correct protocol (TSE 2017) changes
        //     more than liveness checks; this model shows why. The dynamic
        //     campaign never sampled this class (0/106 hits) — recorded as
        //     a MODEL_ONLY finding in FORMAL_CROSS_VALIDATION.md.
        let m = model(
            2,
            Schedule::Fixed(vec![
                SchedEvent::Join(4),
                SchedEvent::Join(25),
                SchedEvent::Join(46),
                SchedEvent::Fail(25),
                SchedEvent::Fail(4),
                SchedEvent::Fail(46),
            ]),
        );
        let checker = m.checker().spawn_bfs().join();
        assert!(
            checker.discovery("alor-after-schedule").is_none(),
            "(a) AtLeastOneRing must be exhaustively safe at level 2 under \
             perfect detection"
        );
        assert!(
            checker.discovery("connapp-after-schedule").is_none(),
            "(a) ConnectedAppendages must be exhaustively safe at level 2 \
             under perfect detection"
        );
        assert!(
            checker
                .discovery("ordered-or-split-after-schedule")
                .is_some(),
            "(b) the split/misorder class must remain reachable at level 2 \
             (liveness discipline alone does not close it)"
        );
    }
}
