//! Latency distribution parsing: "fixed:N", "uniform:LO-HI", "exp:MEAN".

use crate::ScenarioError;

/// Latency distribution for network messages.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LatencyDistribution {
    Fixed { ns: u64 },
    Uniform { lo_ns: u64, hi_ns: u64 },
    Exponential { mean_ns: u64 },
}

impl LatencyDistribution {
    /// Parse latency spec: "fixed:1000" or "uniform:100-5000" or "exp:1000".
    pub fn parse(spec: &str) -> Result<Self, ScenarioError> {
        let spec = spec.trim();

        if let Some(rest) = spec.strip_prefix("fixed:") {
            let ns = rest
                .parse::<u64>()
                .map_err(|e| ScenarioError::InvalidLatency(spec.to_string(), e.to_string()))?;
            Ok(Self::Fixed { ns })
        } else if let Some(rest) = spec.strip_prefix("uniform:") {
            let (lo, hi) = rest.split_once('-').ok_or_else(|| {
                ScenarioError::InvalidLatency(
                    spec.to_string(),
                    "uniform format is 'uniform:LO-HI' (e.g., 'uniform:100-5000')".to_string(),
                )
            })?;
            let lo_ns = lo.parse::<u64>().map_err(|e| {
                ScenarioError::InvalidLatency(spec.to_string(), format!("lo: {}", e))
            })?;
            let hi_ns = hi.parse::<u64>().map_err(|e| {
                ScenarioError::InvalidLatency(spec.to_string(), format!("hi: {}", e))
            })?;
            if lo_ns > hi_ns {
                return Err(ScenarioError::InvalidLatency(
                    spec.to_string(),
                    format!("lo ({}) must be <= hi ({})", lo_ns, hi_ns),
                ));
            }
            Ok(Self::Uniform { lo_ns, hi_ns })
        } else if let Some(rest) = spec.strip_prefix("exp:") {
            let mean_ns = rest
                .parse::<u64>()
                .map_err(|e| ScenarioError::InvalidLatency(spec.to_string(), e.to_string()))?;
            if mean_ns == 0 {
                return Err(ScenarioError::InvalidLatency(
                    spec.to_string(),
                    "mean must be > 0".to_string(),
                ));
            }
            Ok(Self::Exponential { mean_ns })
        } else {
            Err(ScenarioError::InvalidLatency(
                spec.to_string(),
                "must be 'fixed:N', 'uniform:LO-HI', or 'exp:MEAN'".to_string(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fixed() {
        assert_eq!(
            LatencyDistribution::parse("fixed:1000").unwrap(),
            LatencyDistribution::Fixed { ns: 1000 }
        );
    }

    #[test]
    fn parse_uniform() {
        assert_eq!(
            LatencyDistribution::parse("uniform:100-5000").unwrap(),
            LatencyDistribution::Uniform {
                lo_ns: 100,
                hi_ns: 5000
            }
        );
    }

    #[test]
    fn parse_exponential() {
        assert_eq!(
            LatencyDistribution::parse("exp:1000").unwrap(),
            LatencyDistribution::Exponential { mean_ns: 1000 }
        );
    }

    #[test]
    fn invalid_formats() {
        assert!(LatencyDistribution::parse("uniform:5000-100").is_err()); // lo > hi
        assert!(LatencyDistribution::parse("exp:0").is_err()); // mean = 0
        assert!(LatencyDistribution::parse("foo:123").is_err()); // unknown type
    }
}
