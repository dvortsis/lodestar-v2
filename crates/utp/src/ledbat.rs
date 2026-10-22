//! LEDBAT congestion control (BEP 29, RFC 6817) and the retransmission
//! timer (RFC 6298 smoothing with Karn's rule, applied by the caller).
//!
//! The window grows while the one-way queueing delay we cause stays under
//! the 100 ms target and shrinks as it rises above it, so uTP yields to
//! other traffic on the user's link instead of filling router buffers.

use std::time::{Duration, Instant};

/// The queueing delay LEDBAT aims for.
const TARGET_US: i64 = 100_000;
/// Window growth per round trip at zero queueing delay.
const MAX_CWND_INCREASE_PER_RTT: f64 = 3000.0;
/// Base delay is the minimum over this many one-minute slots (≈ 2 min).
const BASE_DELAY_SLOTS: usize = 3;
const SLOT: Duration = Duration::from_secs(60);

pub(crate) const INITIAL_RTO: Duration = Duration::from_secs(1);
const MIN_RTO: Duration = Duration::from_millis(500);
pub(crate) const MAX_RTO: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub(crate) struct Ledbat {
    /// Congestion window in bytes.
    cwnd: f64,
    min_cwnd: f64,
    max_cwnd: f64,
    slow_start: bool,
    ssthresh: f64,
    /// Minimum delay sample per slot, newest last.
    base: [u32; BASE_DELAY_SLOTS],
    slot_started: Option<Instant>,
    last_decrease: Option<Instant>,
}

impl Ledbat {
    pub fn new(mss: usize, max_cwnd: usize) -> Ledbat {
        let mss = mss as f64;
        Ledbat {
            cwnd: 3.0 * mss,
            min_cwnd: mss,
            max_cwnd: (max_cwnd as f64).max(mss),
            slow_start: true,
            ssthresh: max_cwnd as f64,
            base: [u32::MAX; BASE_DELAY_SLOTS],
            slot_started: None,
            last_decrease: None,
        }
    }

    pub fn window(&self) -> usize {
        self.cwnd as usize
    }

    fn base_delay(&mut self, sample: u32, now: Instant) -> u32 {
        match self.slot_started {
            None => self.slot_started = Some(now),
            Some(t) if now.duration_since(t) >= SLOT => {
                self.base.rotate_left(1);
                self.base[BASE_DELAY_SLOTS - 1] = u32::MAX;
                self.slot_started = Some(now);
            }
            Some(_) => {}
        }
        let last = &mut self.base[BASE_DELAY_SLOTS - 1];
        *last = (*last).min(sample);
        self.base.iter().copied().min().unwrap_or(sample)
    }

    /// New bytes were acknowledged by a packet whose one-way delay sample
    /// (the peer's `timestamp_diff`) is `delay_us`; 0 means "no sample".
    pub fn on_ack(&mut self, bytes_acked: usize, delay_us: u32, now: Instant) {
        if bytes_acked == 0 {
            return;
        }
        let our_delay = if delay_us == 0 {
            0
        } else {
            let base = self.base_delay(delay_us, now);
            // Clocks wrap and peers lie; a delay beyond 10 s is noise.
            i64::from(delay_us.wrapping_sub(base)).min(10_000_000)
        };
        let delay_factor = ((TARGET_US - our_delay) as f64 / TARGET_US as f64).clamp(-1.0, 1.0);
        let acked = bytes_acked as f64;
        let window_factor = acked / self.cwnd.max(acked);
        let gain = MAX_CWND_INCREASE_PER_RTT * delay_factor * window_factor;
        if self.slow_start {
            if our_delay * 10 > TARGET_US * 9 || self.cwnd + acked > self.ssthresh {
                self.slow_start = false;
            } else {
                self.cwnd = (self.cwnd + acked).max(self.cwnd + gain);
            }
        }
        if !self.slow_start {
            self.cwnd += gain;
        }
        self.cwnd = self.cwnd.clamp(self.min_cwnd, self.max_cwnd);
    }

    /// A packet was lost (selective ACKs or duplicate ACKs): halve, at most
    /// once per round trip.
    pub fn on_loss(&mut self, now: Instant, rtt: Duration) {
        if self
            .last_decrease
            .is_some_and(|t| now.duration_since(t) < rtt)
        {
            return;
        }
        self.last_decrease = Some(now);
        self.slow_start = false;
        self.ssthresh = (self.cwnd / 2.0).max(self.min_cwnd);
        self.cwnd = self.ssthresh;
    }

    /// The retransmission timer fired: back to one packet, then slow start
    /// up to half the old window.
    pub fn on_timeout(&mut self) {
        self.ssthresh = (self.cwnd / 2.0).max(self.min_cwnd);
        self.cwnd = self.min_cwnd;
        self.slow_start = true;
    }
}

#[derive(Debug)]
pub(crate) struct Rtt {
    srtt: Option<Duration>,
    rttvar: Duration,
    /// Doubled on every timeout until a fresh sample arrives.
    backoff: u32,
}

impl Rtt {
    pub fn new() -> Rtt {
        Rtt {
            srtt: None,
            rttvar: Duration::ZERO,
            backoff: 0,
        }
    }

    pub fn sample(&mut self, r: Duration) {
        match self.srtt {
            None => {
                self.srtt = Some(r);
                self.rttvar = r / 2;
            }
            Some(s) => {
                let diff = s.abs_diff(r);
                self.rttvar = self.rttvar * 3 / 4 + diff / 4;
                self.srtt = Some(s * 7 / 8 + r / 8);
            }
        }
        self.backoff = 0;
    }

    /// The smoothed round trip, or the initial timeout before any sample.
    pub fn srtt(&self) -> Duration {
        self.srtt.unwrap_or(INITIAL_RTO)
    }

    pub fn rto(&self) -> Duration {
        let base = match self.srtt {
            None => INITIAL_RTO,
            Some(s) => (s + 4 * self.rttvar).max(MIN_RTO),
        };
        base.saturating_mul(1 << self.backoff.min(16)).min(MAX_RTO)
    }

    pub fn back_off(&mut self) {
        self.backoff = self.backoff.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MSS: usize = 1000;

    #[test]
    fn grows_under_target_and_shrinks_above_it() {
        let t = Instant::now();
        let mut c = Ledbat::new(MSS, 1 << 20);
        c.slow_start = false;
        let start = c.window();
        // Base 10 ms, no queueing: grows by up to 3000 bytes a window.
        for _ in 0..10 {
            c.on_ack(MSS, 10_000, t);
        }
        let grown = c.window();
        assert!(grown > start);
        // 300 ms above base: 200 ms over target, so it shrinks.
        for _ in 0..10 {
            c.on_ack(MSS, 310_000, t);
        }
        assert!(c.window() < grown);
        // Never below one packet.
        for _ in 0..10_000 {
            c.on_ack(MSS, 2_000_000, t);
        }
        assert_eq!(c.window(), MSS);
    }

    #[test]
    fn slow_start_doubles_until_delay_rises() {
        let t = Instant::now();
        let mut c = Ledbat::new(MSS, 1 << 20);
        let w = c.window();
        for _ in 0..3 {
            c.on_ack(MSS, 5_000, t);
        }
        assert_eq!(c.window(), 2 * w, "one window of acks doubles it");
        c.on_ack(MSS, 5_000 + 95_000, t);
        assert!(!c.slow_start, "delay near target ends slow start");
    }

    #[test]
    fn base_delay_is_a_windowed_minimum() {
        let t = Instant::now();
        let mut c = Ledbat::new(MSS, 1 << 20);
        assert_eq!(c.base_delay(500, t), 500);
        assert_eq!(c.base_delay(900, t), 500);
        // The old minimum ages out after the slots rotate past it.
        let mut now = t;
        for _ in 0..BASE_DELAY_SLOTS {
            now += SLOT;
            c.base_delay(900, now);
        }
        assert_eq!(c.base_delay(900, now), 900);
    }

    #[test]
    fn loss_halves_once_per_rtt_and_timeout_resets() {
        let t = Instant::now();
        let mut c = Ledbat::new(MSS, 1 << 20);
        c.cwnd = 40.0 * MSS as f64;
        c.on_loss(t, Duration::from_millis(100));
        assert_eq!(c.window(), 20 * MSS);
        c.on_loss(t + Duration::from_millis(10), Duration::from_millis(100));
        assert_eq!(c.window(), 20 * MSS, "same round trip");
        c.on_loss(t + Duration::from_millis(200), Duration::from_millis(100));
        assert_eq!(c.window(), 10 * MSS);
        c.on_timeout();
        assert_eq!(c.window(), MSS);
        assert!(c.slow_start);
    }

    #[test]
    fn rto_smooths_backs_off_and_clamps() {
        let mut r = Rtt::new();
        assert_eq!(r.rto(), INITIAL_RTO);
        r.sample(Duration::from_millis(10));
        assert_eq!(r.rto(), MIN_RTO, "floored at 500 ms");
        r.sample(Duration::from_millis(2000));
        let rto = r.rto();
        assert!(rto > MIN_RTO);
        r.back_off();
        assert_eq!(r.rto(), (rto * 2).min(MAX_RTO));
        for _ in 0..40 {
            r.back_off();
        }
        assert_eq!(r.rto(), MAX_RTO);
        r.sample(Duration::from_millis(400));
        assert!(r.rto() < MAX_RTO, "a fresh sample resets the backoff");
    }
}
