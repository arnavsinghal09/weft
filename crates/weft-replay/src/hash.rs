//! FNV-1a hashing and hex encoding for the log format.
//!
//! FNV-1a is used (rather than a cryptographic hash) because the chain exists
//! to catch truncation, reordering, and accidental edits — not adversaries —
//! and keeping the format implementable from the spec alone matters more than
//! collision resistance. See docs/recording-format.md.

/// FNV-1a 64-bit offset basis.
pub const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
pub const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a over `bytes`, continuing from `state` (pass [`FNV_OFFSET`] to start).
#[must_use]
pub fn fnv1a(mut state: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        state ^= u64::from(b);
        state = state.wrapping_mul(FNV_PRIME);
    }
    state
}

/// FNV-1a of a whole buffer from the standard offset basis.
#[must_use]
pub fn fnv1a_once(bytes: &[u8]) -> u64 {
    fnv1a(FNV_OFFSET, bytes)
}

/// Lowercase hex encoding.
#[must_use]
pub fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('?'));
        s.push(char::from_digit(u32::from(b & 0xf), 16).unwrap_or('?'));
    }
    s
}

/// Decode lowercase/uppercase hex; `None` on odd length or non-hex bytes.
#[must_use]
pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let digits = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in digits.chunks_exact(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        #[allow(clippy::cast_possible_truncation)] // both nibbles < 16
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_reference_vector() {
        // Published FNV-1a test vector: "a" → 0xaf63dc4c8601ec8c.
        assert_eq!(fnv1a_once(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a_once(b""), FNV_OFFSET);
    }

    #[test]
    fn hex_round_trip() {
        let data = [0u8, 1, 15, 16, 127, 128, 255];
        assert_eq!(from_hex(&to_hex(&data)).unwrap(), data);
        assert_eq!(from_hex("0"), None); // odd length
        assert_eq!(from_hex("zz"), None); // not hex
        assert_eq!(from_hex(""), Some(Vec::new()));
    }
}
