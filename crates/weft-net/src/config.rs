//! Parsing the compact network-condition spec (from `--net` / `WEFT_NET`).
//!
//! Grammar: comma-separated `key=value` clauses. Within a `partition`, groups
//! are separated by `|` and nodes within a group by `+` (so `,` stays free as
//! the clause separator). Examples:
//! - `latency=uniform:1000-5000,loss=0.1`
//! - `latency=exp:3000,bw=1000000`
//! - `partition=0+1|2` (nodes 0 and 1 on one side, node 2 on the other)

use crate::fault::{FaultModel, Latency, Partition};

/// Parse a spec into a [`FaultModel`] bound to `seed`. An empty spec yields a
/// perfectly reliable network.
///
/// # Errors
/// Returns a human-readable message for any malformed clause.
pub fn parse(seed: u64, spec: &str) -> Result<FaultModel, String> {
    let mut m = FaultModel::reliable(seed);
    for clause in spec.split(',') {
        let clause = clause.trim();
        if clause.is_empty() {
            continue;
        }
        let (k, v) = clause
            .split_once('=')
            .ok_or_else(|| format!("network clause {clause:?} is not key=value"))?;
        match k.trim() {
            "latency" | "lat" => m.latency = parse_latency(v.trim())?,
            "loss" => {
                m.loss = v
                    .trim()
                    .parse()
                    .map_err(|_| format!("loss {v:?} is not a number"))?;
                if !(0.0..=1.0).contains(&m.loss) {
                    return Err(format!("loss {} out of [0,1]", m.loss));
                }
            }
            "bw" | "bandwidth" => {
                m.bandwidth_bps = v
                    .trim()
                    .parse()
                    .map_err(|_| format!("bw {v:?} is not an integer"))?;
            }
            "partition" | "part" => m.partition = parse_partition(v.trim())?,
            other => return Err(format!("unknown network key {other:?}")),
        }
    }
    Ok(m)
}

fn parse_latency(v: &str) -> Result<Latency, String> {
    let (kind, arg) = v.split_once(':').unwrap_or((v, ""));
    match kind {
        "fixed" | "const" => Ok(Latency::Fixed(parse_ns(arg)?)),
        "uniform" | "unif" => {
            let (lo, hi) = arg
                .split_once('-')
                .ok_or_else(|| format!("uniform latency needs lo-hi, got {arg:?}"))?;
            let (lo, hi) = (parse_ns(lo)?, parse_ns(hi)?);
            if hi < lo {
                return Err(format!("uniform latency hi<lo ({hi}<{lo})"));
            }
            Ok(Latency::Uniform { lo, hi })
        }
        "exp" | "exponential" => Ok(Latency::Exponential { mean: parse_ns(arg)? }),
        other => Err(format!("unknown latency kind {other:?}")),
    }
}

fn parse_ns(s: &str) -> Result<u64, String> {
    s.trim().parse().map_err(|_| format!("{s:?} is not an integer (nanoseconds)"))
}

fn parse_partition(v: &str) -> Result<Partition, String> {
    let mut groups = Vec::new();
    for side in v.split('|') {
        let mut nodes = Vec::new();
        for n in side.split('+') {
            let n = n.trim();
            if n.is_empty() {
                continue;
            }
            nodes.push(n.parse::<u32>().map_err(|_| format!("bad node id {n:?}"))?);
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
        let m = parse(9, "latency=uniform:1000-5000,loss=0.1,bw=2000000,partition=0+1|2").unwrap();
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
        assert!(parse(1, "loss=2.0").is_err());
        assert!(parse(1, "latency=weird:5").is_err());
        assert!(parse(1, "nope=1").is_err());
        assert!(parse(1, "latency=uniform:5").is_err());
    }
}
