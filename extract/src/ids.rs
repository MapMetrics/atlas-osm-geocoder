use blake2::{digest::{Update, VariableOutput}, Blake2bVar};

pub fn hid(s: &str) -> u64 {
    let mut h = Blake2bVar::new(7).expect("7-byte blake2b");
    h.update(s.as_bytes());
    let mut out = [0u8; 7];
    h.finalize_variable(&mut out).expect("finalize");
    let mut v: u64 = 0;
    for b in out { v = (v << 8) | b as u64; }
    v
}

pub fn osm_sid(kind: char, id: i64) -> String {
    format!("{kind}{id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hid_matches_python_blake2b7() {
        // values from Task 1 Step 1 — Python is the source of truth
        assert_eq!(hid("n1"), 52385471604144979);
        assert_eq!(hid("w42"), 54027233708865090);
        assert_eq!(hid("r7444"), 15141391783958377);
        assert_eq!(hid("n123456789"), 52076732293777007);
        assert_eq!(hid("abc"), 60012850110631570);
        assert!(hid("n1") < (1u64 << 56)); // 7-byte digest ⇒ /details shard rule holds
    }

    #[test]
    fn sid_format() {
        assert_eq!(osm_sid('n', 123), "n123");
    }
}
