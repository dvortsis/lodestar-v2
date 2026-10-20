//! `announce_peer` tokens (BEP 5): a hash of a rotating secret and the
//! querier's IP. A token is valid for the current and the previous secret,
//! i.e. for 5 to 10 minutes after the `get_peers` that issued it.

use std::net::IpAddr;
use std::time::{Duration, Instant};

use rand::RngExt;
use sha1::{Digest, Sha1};

pub(crate) const ROTATE_EVERY: Duration = Duration::from_secs(5 * 60);
const TOKEN_LEN: usize = 8;

pub(crate) struct Tokens {
    current: [u8; 16],
    previous: [u8; 16],
    rotated_at: Instant,
}

impl Tokens {
    pub fn new(now: Instant, rng: &mut impl RngExt) -> Tokens {
        Tokens {
            current: rng.random(),
            previous: rng.random(),
            rotated_at: now,
        }
    }

    fn compute(secret: &[u8; 16], ip: &IpAddr) -> [u8; TOKEN_LEN] {
        let mut h = Sha1::new();
        h.update(secret);
        match ip {
            IpAddr::V4(v4) => h.update(v4.octets()),
            IpAddr::V6(v6) => h.update(v6.octets()),
        }
        let digest = h.finalize();
        digest[..TOKEN_LEN].try_into().expect("digest is 20 bytes")
    }

    pub fn issue(&self, ip: &IpAddr) -> [u8; TOKEN_LEN] {
        Tokens::compute(&self.current, ip)
    }

    pub fn is_valid(&self, ip: &IpAddr, token: &[u8]) -> bool {
        token == Tokens::compute(&self.current, ip) || token == Tokens::compute(&self.previous, ip)
    }

    pub fn maybe_rotate(&mut self, now: Instant, rng: &mut impl RngExt) {
        if now.duration_since(self.rotated_at) >= ROTATE_EVERY {
            self.previous = self.current;
            self.current = rng.random();
            self.rotated_at = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    #[test]
    fn tokens_bind_to_ip_and_survive_one_rotation() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(9);
        let t0 = Instant::now();
        let mut tokens = Tokens::new(t0, &mut rng);
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let other: IpAddr = "1.2.3.5".parse().unwrap();
        let tok = tokens.issue(&ip);
        assert!(tokens.is_valid(&ip, &tok));
        assert!(!tokens.is_valid(&other, &tok));
        assert!(!tokens.is_valid(&ip, &tok[..4]));
        tokens.maybe_rotate(t0 + ROTATE_EVERY, &mut rng);
        assert!(tokens.is_valid(&ip, &tok), "previous secret still accepted");
        tokens.maybe_rotate(t0 + ROTATE_EVERY * 2, &mut rng);
        assert!(!tokens.is_valid(&ip, &tok), "expired after two rotations");
    }
}
