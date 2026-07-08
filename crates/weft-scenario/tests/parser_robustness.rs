//! Parser robustness sweep: 10,000 mutated / truncated / garbage inputs
//! must never panic the scenario parser — errors are fine, aborts are not.
//!
//! This is the stable-toolchain stand-in for a `cargo fuzz` target: the
//! mutation engine is a seeded SplitMix64, so every run covers the same
//! deterministic corpus and a failure is reproducible by iteration index.

use weft_scenario::Scenario;

const VALID: &str = r#"{
  "seed": 7,
  "processes": [
    {"id": 0, "cmd": ["node", "a"]},
    {"id": 1, "cmd": ["node", "b"]}
  ],
  "net": {"latency": "uniform:1000-2000", "loss": 0.1, "partitions": "0|1"},
  "events": [
    {"at_ms": 10, "kind": "crash", "node": 1},
    {"at_ms": 20, "kind": "restart", "node": 1}
  ]
}"#;

fn splitmix64(s: &mut u64) -> u64 {
    *s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *s;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[test]
fn ten_thousand_mutated_inputs_never_panic() {
    let mut rng = 0x5EED_u64;
    let base = VALID.as_bytes();
    for i in 0..10_000 {
        let mut bytes = base.to_vec();
        match splitmix64(&mut rng) % 4 {
            // Byte flips at random positions.
            0 => {
                for _ in 0..1 + splitmix64(&mut rng) % 8 {
                    let pos = (splitmix64(&mut rng) as usize) % bytes.len();
                    bytes[pos] = (splitmix64(&mut rng) & 0xFF) as u8;
                }
            }
            // Truncation.
            1 => {
                let len = (splitmix64(&mut rng) as usize) % bytes.len();
                bytes.truncate(len);
            }
            // Random garbage of random length (incl. invalid UTF-8).
            2 => {
                let len = (splitmix64(&mut rng) as usize) % 512;
                bytes = (0..len)
                    .map(|_| (splitmix64(&mut rng) & 0xFF) as u8)
                    .collect();
            }
            // Duplication: splice a random slice into a random position.
            _ => {
                let start = (splitmix64(&mut rng) as usize) % bytes.len();
                let end = start + (splitmix64(&mut rng) as usize) % (bytes.len() - start);
                let at = (splitmix64(&mut rng) as usize) % bytes.len();
                let slice = bytes[start..end].to_vec();
                for (offset, byte) in slice.into_iter().enumerate() {
                    bytes.insert(at + offset, byte);
                }
            }
        }
        // A panic here fails the test with the reproducing iteration index.
        let text = String::from_utf8_lossy(&bytes);
        let parses = std::panic::catch_unwind(|| {
            let _ = Scenario::from_json(&text);
            let _ = Scenario::from_yaml(&text);
        });
        assert!(
            parses.is_ok(),
            "parser panicked on mutated input, iteration {i}"
        );
    }
}
