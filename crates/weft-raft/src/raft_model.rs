//! Explicit-state model of Raft leader election for the formal oracle —
//! the synchronous counterpart of `examples/raft/raft_node.c`, checking
//! the same **ElectionSafety** invariant as `raft-check` (lib.rs).
//!
//! Semantics ported 1:1 from raft_node.c:
//! - Becoming candidate: `term += 1; voted_for = self; votes = {self}`.
//! - Vote grant (RV handling): a higher-term RV first steps the voter down
//!   (`term = rv.term, role = Follower, voted_for = None`), then the grant
//!   test is `rv.term == term && (voted_for is None || voted_for == cand)`.
//! - Majority (`votes*2 > members`) makes the candidate leader.
//! - Crash-restart drops role/tally, keeps `current_term`, and drops
//!   `voted_for` **iff fix = 0** — Figure 3.2's persistence requirement is
//!   exactly the fix-1/fix-0 difference, as in the C.
//!
//! Abstractions (stated, load-bearing for interpretation):
//! - **Timing → nondeterminism.** Election timeouts and message delays do
//!   not exist; any non-leader may start an election at any step, any
//!   candidate's RV may reach any voter at any step. The model asks
//!   ∃-interleaving questions; the dynamic engine samples real timing.
//! - **Atomic grant.** RV delivery, the grant, and the candidate counting
//!   the vote are one atomic action (no in-flight replies).
//! - **Term bound.** Elections stop at `term_bound` (taken from the
//!   recording being replayed, plus headroom), keeping the state space
//!   finite. Reported results say "within term bound B", never more.
//! - Heartbeats are omitted; they only suppress elections, and the model's
//!   nondeterminism already includes every suppression pattern (a node
//!   simply not starting an election).

use stateright::{Model, Property};

pub const NOBODY: i8 = -1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    Follower,
    Candidate,
    Leader,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RNode {
    pub term: u8,
    pub role: Role,
    pub voted_for: i8,
    /// Bitmask of granted votes (only meaningful while Candidate).
    pub votes: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RaftState {
    pub nodes: Vec<RNode>,
    /// leader_of[t] = first node to win term t (NOBODY if none yet).
    pub leader_of: Vec<i8>,
    /// Sticky: some term elected two distinct leaders.
    pub violated: bool,
    /// Next unapplied index into the fixed restart schedule.
    pub sched_pos: u8,
    /// Exhaustive mode: restarts injected so far.
    pub restarts_done: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RaftAction {
    /// A non-leader times out and becomes candidate for term+1.
    StartElection { node: u8 },
    /// Candidate `cand`'s RequestVote reaches `voter` (atomic grant+count).
    Grant { cand: u8, voter: u8 },
    /// `node` observes `peer`'s higher term and steps down (RVR/HB path).
    StepDown { node: u8, peer: u8 },
    /// The next scheduled crash-restart hits `node`.
    Restart { node: u8 },
}

#[derive(Clone, Debug)]
pub enum RestartSchedule {
    /// Restart sequence extracted from one recording, in order.
    Fixed(Vec<u8>),
    /// Any node may restart at any time, up to `max_restarts` total.
    Exhaustive,
}

pub struct RaftModel {
    pub members: u8,
    pub fix: u8,
    pub term_bound: u8,
    pub schedule: RestartSchedule,
    pub max_restarts: u8,
}

impl RaftModel {
    fn majority(&self, votes: u8) -> bool {
        votes.count_ones() * 2 > u32::from(self.members)
    }
}

impl Model for RaftModel {
    type State = RaftState;
    type Action = RaftAction;

    fn init_states(&self) -> Vec<Self::State> {
        vec![RaftState {
            nodes: vec![
                RNode {
                    term: 0,
                    role: Role::Follower,
                    voted_for: NOBODY,
                    votes: 0,
                };
                usize::from(self.members)
            ],
            leader_of: vec![NOBODY; usize::from(self.term_bound) + 1],
            violated: false,
            sched_pos: 0,
            restarts_done: 0,
        }]
    }

    #[allow(clippy::cast_possible_truncation)]
    fn actions(&self, s: &Self::State, actions: &mut Vec<Self::Action>) {
        if s.violated {
            return; // property reached; no need to expand further
        }
        // Schedule.
        match &self.schedule {
            RestartSchedule::Fixed(seq) => {
                if let Some(&n) = seq.get(usize::from(s.sched_pos)) {
                    actions.push(RaftAction::Restart { node: n });
                }
            }
            RestartSchedule::Exhaustive => {
                if s.restarts_done < self.max_restarts {
                    for n in 0..self.members {
                        actions.push(RaftAction::Restart { node: n });
                    }
                }
            }
        }
        // Protocol.
        for (i, ni) in s.nodes.iter().enumerate() {
            if ni.role != Role::Leader && ni.term < self.term_bound {
                actions.push(RaftAction::StartElection { node: i as u8 });
            }
            if ni.role == Role::Candidate {
                for (j, nj) in s.nodes.iter().enumerate() {
                    if i == j {
                        continue;
                    }
                    // Emit only if delivery would change state: either the
                    // voter steps down (higher term) or grants.
                    let would_grant = ni.term == nj.term
                        && (nj.voted_for == NOBODY || nj.voted_for == i as i8)
                        && s.nodes[i].votes & (1 << j) == 0;
                    if ni.term > nj.term || would_grant {
                        actions.push(RaftAction::Grant {
                            cand: i as u8,
                            voter: j as u8,
                        });
                    }
                }
            }
            // Step-down on observing any higher-term peer.
            for (j, nj) in s.nodes.iter().enumerate() {
                if i != j && nj.term > ni.term {
                    actions.push(RaftAction::StepDown {
                        node: i as u8,
                        peer: j as u8,
                    });
                }
            }
        }
    }

    // node ids are < 8 in every scenario; u8 -> i8 cannot wrap.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    fn next_state(&self, s: &Self::State, a: Self::Action) -> Option<Self::State> {
        let mut ns = s.clone();
        match a {
            RaftAction::StartElection { node } => {
                let k = usize::from(node);
                let n = &mut ns.nodes[k];
                n.term += 1;
                n.role = Role::Candidate;
                n.voted_for = node as i8;
                n.votes = 1 << node;
                // A 1-member cluster would elect immediately; members >= 3
                // in every scenario, so majority needs at least one grant.
            }
            RaftAction::Grant { cand, voter } => {
                let (ci, vi) = (usize::from(cand), usize::from(voter));
                let cand_term = s.nodes[ci].term;
                {
                    let v = &mut ns.nodes[vi];
                    if cand_term > v.term {
                        // step_down(rv.term) exactly as raft_node.c: adopt
                        // term, clear vote, drop candidacy.
                        v.term = cand_term;
                        v.role = Role::Follower;
                        v.voted_for = NOBODY;
                        v.votes = 0;
                    }
                    if cand_term == v.term && (v.voted_for == NOBODY || v.voted_for == cand as i8) {
                        v.voted_for = cand as i8;
                    } else {
                        return None; // delivery changed nothing grantable
                    }
                }
                let c = &mut ns.nodes[ci];
                if c.role == Role::Candidate && c.term == cand_term {
                    c.votes |= 1 << voter;
                    if self.majority(c.votes) {
                        c.role = Role::Leader;
                        let slot = &mut ns.leader_of[usize::from(cand_term)];
                        if *slot == NOBODY {
                            *slot = cand as i8;
                        } else if *slot != cand as i8 {
                            ns.violated = true;
                        }
                    }
                }
            }
            RaftAction::StepDown { node, peer } => {
                let higher = s.nodes[usize::from(peer)].term;
                let n = &mut ns.nodes[usize::from(node)];
                n.term = higher;
                n.role = Role::Follower;
                n.voted_for = NOBODY;
                n.votes = 0;
            }
            RaftAction::Restart { node } => {
                let n = &mut ns.nodes[usize::from(node)];
                n.role = Role::Follower;
                n.votes = 0;
                if self.fix == 0 {
                    n.voted_for = NOBODY; // Figure 3.2 broken: vote lost
                }
                match &self.schedule {
                    RestartSchedule::Fixed(_) => ns.sched_pos += 1,
                    RestartSchedule::Exhaustive => ns.restarts_done += 1,
                }
            }
        }
        (ns != *s).then_some(ns)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![Property::<Self>::sometimes(
            "election-safety-violated",
            |_, s| s.violated,
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stateright::Checker;

    fn model(fix: u8, schedule: RestartSchedule) -> RaftModel {
        RaftModel {
            members: 5,
            fix,
            term_bound: 2,
            schedule,
            max_restarts: 3,
        }
    }

    #[test]
    fn volatile_vote_with_one_restart_breaks_election_safety() {
        // One voter restarting mid-term is enough at fix 0: it can grant
        // its term-1 vote twice.
        let m = model(0, RestartSchedule::Fixed(vec![1]));
        let checker = m.checker().spawn_bfs().join();
        assert!(
            checker.discovery("election-safety-violated").is_some(),
            "fix 0 must reach two leaders in one term with a restart"
        );
    }

    #[test]
    fn persistent_vote_is_exhaustively_safe_under_same_restarts() {
        let m = model(1, RestartSchedule::Fixed(vec![1]));
        let checker = m.checker().spawn_bfs().join();
        assert!(
            checker.discovery("election-safety-violated").is_none(),
            "fix 1 must be exhaustively safe for the same schedule"
        );
    }

    #[test]
    fn no_restarts_is_safe_even_with_volatile_votes() {
        // Without a restart, fix 0 and fix 1 coincide; safety must hold.
        let m = model(0, RestartSchedule::Fixed(vec![]));
        let checker = m.checker().spawn_bfs().join();
        assert!(checker.discovery("election-safety-violated").is_none());
    }
}
