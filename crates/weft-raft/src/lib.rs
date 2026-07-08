//! Raft **ElectionSafety** checker over Weft recordings.
//!
//! Property (Ongaro, "Consensus: Bridging Theory and Practice", Fig. 3.2):
//! *at most one leader can be elected in a given term*. Leadership is
//! transient, so unlike the Chord checker this scans EVERY state report in
//! the recording, not just the final configuration: a term with two
//! distinct nodes ever reporting `role == LEADER` is a violation no matter
//! how briefly either held it.
//!
//! Report wire format (from `examples/raft/raft_node.c`):
//! `RPT <node> <date> <alive> <term> <role> <votedFor>` with role
//! 0=follower, 1=candidate, 2=leader; `alive == 0` marks a crash-restart
//! tick (the node resumes next tick).

use std::collections::BTreeMap;

use weft_replay::log::Event;
use weft_replay::Log;

pub const LEADER: i32 = 2;

/// One parsed `RPT` state report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RaftReport {
    pub node: i32,
    pub date: i32,
    pub alive: bool,
    pub term: i32,
    pub role: i32,
    pub voted_for: i32,
}

/// Parse the text of one datagram; `None` if it is not an RPT.
#[must_use]
pub fn parse_report(text: &str) -> Option<RaftReport> {
    let mut it = text.split_ascii_whitespace();
    if it.next()? != "RPT" {
        return None;
    }
    let mut next = || it.next()?.parse::<i32>().ok();
    Some(RaftReport {
        node: next()?,
        date: next()?,
        alive: next()? != 0,
        term: next()?,
        role: next()?,
        voted_for: next()?,
    })
}

/// An ElectionSafety violation: two distinct nodes led the same term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub term: i32,
    pub leaders: Vec<i32>,
    /// op index of the report that completed the violation (zero-archaeology
    /// anchor into the recording's logical timeline).
    pub at_op: u64,
}

/// Outcome of scanning one recording.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Verdict {
    /// term -> every node that ever reported itself leader in that term.
    pub leaders_by_term: BTreeMap<i32, Vec<i32>>,
    pub violations: Vec<Violation>,
    pub restarts: u32,
    pub reports: u64,
}

impl Verdict {
    /// True when no term ever had a leader — the seed exercised nothing.
    #[must_use]
    pub fn uninformative(&self) -> bool {
        self.leaders_by_term.is_empty()
    }
    /// Fold one report into the verdict (broker-linearization order).
    pub fn observe(&mut self, op: u64, rep: RaftReport) {
        self.reports += 1;
        if !rep.alive {
            self.restarts += 1;
            return;
        }
        if rep.role == LEADER {
            let leaders = self.leaders_by_term.entry(rep.term).or_default();
            if !leaders.contains(&rep.node) {
                leaders.push(rep.node);
                if leaders.len() == 2 {
                    self.violations.push(Violation {
                        term: rep.term,
                        leaders: leaders.clone(),
                        at_op: op,
                    });
                }
            }
        }
    }
}

/// Scan every Send record's payload for RPT reports and collect leaders per
/// term in broker-linearization order.
#[must_use]
pub fn check(log: &Log) -> Verdict {
    let mut v = Verdict::default();
    for r in &log.records {
        let Event::Send { payload, .. } = &r.e else {
            continue;
        };
        let Some(bytes) = weft_replay::hash::from_hex(payload) else {
            continue;
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        let Some(rep) = parse_report(text) else {
            continue;
        };
        v.observe(r.op, rep);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_reports() {
        let r = parse_report("RPT 3 17 1 5 2 3").unwrap();
        assert_eq!(
            r,
            RaftReport {
                node: 3,
                date: 17,
                alive: true,
                term: 5,
                role: 2,
                voted_for: 3
            }
        );
        assert!(parse_report("RV 5 2").is_none());
        assert!(parse_report("RPT 3 17").is_none());
    }

    fn verdict_of(reports: &[(i32, i32, i32, bool)]) -> Verdict {
        // (node, term, role, alive) — fed through the same parse + observe
        // path check() uses on real recordings.
        let mut v = Verdict::default();
        for (op, (node, term, role, alive)) in reports.iter().enumerate() {
            let text = format!("RPT {node} 0 {} {term} {role} -1", i32::from(*alive));
            v.observe(op as u64, parse_report(&text).unwrap());
        }
        v
    }

    #[test]
    fn one_leader_per_term_is_safe() {
        let v = verdict_of(&[(0, 1, 2, true), (0, 1, 2, true), (1, 2, 2, true)]);
        assert!(v.violations.is_empty());
        assert!(!v.uninformative());
        assert_eq!(v.leaders_by_term[&1], vec![0]);
        assert_eq!(v.leaders_by_term[&2], vec![1]);
    }

    #[test]
    fn two_leaders_same_term_is_violation() {
        let v = verdict_of(&[(0, 3, 2, true), (2, 3, 2, true)]);
        assert_eq!(v.violations.len(), 1);
        assert_eq!(v.violations[0].term, 3);
        assert_eq!(v.violations[0].leaders, vec![0, 2]);
    }

    #[test]
    fn no_leader_at_all_is_uninformative() {
        let v = verdict_of(&[(0, 1, 1, true), (1, 1, 0, true), (2, 1, 0, false)]);
        assert!(v.uninformative());
        assert_eq!(v.restarts, 1);
    }
}
