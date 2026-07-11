//! Parsing the compact network-condition spec (from `--net` / `WEFT_NET`).
//!
//! Grammar: comma-separated `key=value` clauses. Within a `partition`, groups
//! are separated by `|` and nodes within a group by `+` (so `,` stays free as
//! the clause separator). Examples:
//! - `latency=uniform:1000-5000,loss=0.1`
//! - `latency=exp:3000,bw=1000000`
//! - `partition=0+1|2` (nodes 0 and 1 on one side, node 2 on the other)

use std::fmt;

use crate::fault::{FaultModel, Latency, Partition};

/// Why a network-condition spec failed to parse. Hand-rolled (no `thiserror`)
/// to keep this crate's dependency tree minimal; `Display` gives the same
/// human-readable message callers previously got as a bare `String`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// A clause is not `key=value`.
    NotKeyValue(String),
    /// The clause key is not part of the spec grammar.
    UnknownKey(String),
    /// The `latency` value is malformed (offending value, reason).
    InvalidLatency(String, String),
    /// The `loss` value is not a probability in `[0, 1]`.
    InvalidLoss(String),
    /// The `bw` value is not an integer.
    InvalidBandwidth(String),
    /// The `partition` value contains a bad node id.
    InvalidPartition(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotKeyValue(clause) => {
                write!(f, "network clause {clause:?} is not key=value")
            }
            Self::UnknownKey(key) => write!(f, "unknown network key {key:?}"),
            Self::InvalidLatency(value, why) => {
                write!(f, "latency {value:?}: {why}")
            }
            Self::InvalidLoss(value) => {
                write!(f, "loss {value:?} is not a probability in [0,1]")
            }
            Self::InvalidBandwidth(value) => {
                write!(f, "bw {value:?} is not an integer")
            }
            Self::InvalidPartition(value) => {
                write!(f, "partition: bad node id {value:?}")
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// Parse a spec into a [`FaultModel`] bound to `seed`. An empty spec yields a
/// perfectly reliable network.
///
/// # Errors
/// Returns a [`ParseError`] describing the first malformed clause.
pub fn parse(seed: u64, spec: &str) -> Result<FaultModel, ParseError> {
    let mut m = FaultModel::reliable(seed);
    for clause in spec.split(',') {
        let clause = clause.trim();
        if clause.is_empty() {
            continue;
        }
        let (k, v) = clause
            .split_once('=')
            .ok_or_else(|| ParseError::NotKeyValue(clause.to_string()))?;
        match k.trim() {
            "latency" | "lat" => m.latency = parse_latency(v.trim())?,
            "loss" => {
                m.loss = v
                    .trim()
                    .parse()
                    .map_err(|_| ParseError::InvalidLoss(v.to_string()))?;
                if !(0.0..=1.0).contains(&m.loss) {
                    return Err(ParseError::InvalidLoss(v.to_string()));
                }
            }
            "bw" | "bandwidth" => {
                m.bandwidth_bps = v
                    .trim()
                    .parse()
                    .map_err(|_| ParseError::InvalidBandwidth(v.to_string()))?;
            }
            "partition" | "part" => m.partition = parse_partition(v.trim())?,
            other => return Err(ParseError::UnknownKey(other.to_string())),
        }
    }
    Ok(m)
}

fn parse_latency(v: &str) -> Result<Latency, ParseError> {
    let bad = |why: &str| ParseError::InvalidLatency(v.to_string(), why.to_string());
    let (kind, arg) = v.split_once(':').unwrap_or((v, ""));
    match kind {
        "fixed" | "const" => Ok(Latency::Fixed(parse_ns(v, arg)?)),
        "uniform" | "unif" => {
            let (lo, hi) = arg.split_once('-').ok_or_else(|| bad("needs lo-hi"))?;
            let (lo, hi) = (parse_ns(v, lo)?, parse_ns(v, hi)?);
            if hi < lo {
                return Err(bad(&format!("hi<lo ({hi}<{lo})")));
            }
            Ok(Latency::Uniform { lo, hi })
        }
        "exp" | "exponential" => Ok(Latency::Exponential {
            mean: parse_ns(v, arg)?,
        }),
        other => Err(bad(&format!("unknown kind {other:?}"))),
    }
}

fn parse_ns(spec: &str, s: &str) -> Result<u64, ParseError> {
    s.trim().parse().map_err(|_| {
        ParseError::InvalidLatency(
            spec.to_string(),
            format!("{s:?} is not an integer (nanoseconds)"),
        )
    })
}

fn parse_partition(v: &str) -> Result<Partition, ParseError> {
    let mut groups = Vec::new();
    for side in v.split('|') {
        let mut nodes = Vec::new();
        for n in side.split('+') {
            let n = n.trim();
            if n.is_empty() {
                continue;
            }
            nodes.push(
                n.parse::<u32>()
                    .map_err(|_| ParseError::InvalidPartition(n.to_string()))?,
            );
        }
        if !nodes.is_empty() {
            groups.push(nodes);
        }
    }
    Ok(Partition::from_groups(groups))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_spec() {
        let m = parse(
            9,
            "latency=uniform:1000-5000,loss=0.1,bw=2000000,partition=0+1|2",
        )
        .unwrap();
        assert_eq!(m.latency, Latency::Uniform { lo: 1000, hi: 5000 });
        assert!((m.loss - 0.1).abs() < 1e-9);
        assert_eq!(m.bandwidth_bps, 2_000_000);
        assert!(m.partition.blocked(0, 2));
        assert!(!m.partition.blocked(0, 1));
    }

    #[test]
    fn empty_spec_is_reliable() {
        let m = parse(1, "").unwrap();
        assert_eq!(m.latency, Latency::Fixed(0));
        assert!((m.loss).abs() < 1e-9);
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(
            parse(1, "loss=2.0"),
            Err(ParseError::InvalidLoss(_))
        ));
        assert!(matches!(
            parse(1, "latency=weird:5"),
            Err(ParseError::InvalidLatency(..))
        ));
        assert!(matches!(parse(1, "nope=1"), Err(ParseError::UnknownKey(_))));
        assert!(matches!(
            parse(1, "latency=uniform:5"),
            Err(ParseError::InvalidLatency(..))
        ));
    }
}
