//! References, not identifiers (§12.1): with info hashes off, a line
//! names a torrent as `ref:7f3a2c9e`, the first 32 bits of an HMAC-SHA256
//! of its info hash under a key drawn from the OS at each start. The key
//! is never written down, so a reference can't be turned back into a hash
//! or matched against a public hash list, and a restart starts new ones.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

pub struct Refs {
    key: [u8; 32],
}

// Never the key.
impl std::fmt::Debug for Refs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Refs([hidden])")
    }
}

impl Refs {
    /// A new run key. Without the OS RNG, torrents show as `[hidden]`
    /// rather than as references anyone could compute.
    pub fn new() -> Option<Refs> {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).ok()?;
        Some(Refs { key })
    }

    pub fn of(&self, hash: &[u8; 20]) -> String {
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&self.key).expect("any key length");
        mac.update(hash);
        let out = mac.finalize().into_bytes();
        format!(
            "ref:{:02x}{:02x}{:02x}{:02x}",
            out[0], out[1], out[2], out[3]
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_within_a_run_and_new_across_runs() {
        let h = [7u8; 20];
        let a = Refs::new().unwrap();
        assert_eq!(a.of(&h), a.of(&h));
        assert_ne!(a.of(&h), a.of(&[8u8; 20]));
        let b = Refs::new().unwrap();
        assert_ne!(a.of(&h), b.of(&h), "a new key at each start");
        let r = a.of(&h);
        assert!(r.starts_with("ref:") && r.len() == 12, "{r}");
        assert!(!r.contains("0707"));
        assert!(!format!("{a:?}").contains('7'));
    }
}
