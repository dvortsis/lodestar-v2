//! Exit when the network this process shares has gone
//! (X-gluetun-restart-recover).
//!
//! Lodestar can run inside another container's network namespace
//! (`--network=container:gluetun`, `network_mode: service:gluetun`). When
//! that container restarts or is recreated, Lodestar is left in the old
//! namespace with nothing but `lo`. Its own container still reads
//! "running", so no restart policy fires, and the web UI and the DHT stay
//! dead until someone restarts it by hand.
//!
//! With `LODESTAR_EXIT_ON_NETWORK_LOSS=true` (off by default; only our
//! gluetun recipes set it), Linux only: every [`SAMPLE`] it reads the
//! interfaces (`/proc/net/dev`) and the default routes (`/proc/net/route`,
//! `/proc/net/ipv6_route`) of its own namespace. "Lost" means no interface
//! but `lo` **and** no default route. Once that has held for [`HOLD`]
//! without a break, it logs one line and stops the way a signal does (the
//! store is flushed), then exits [`EXIT_CODE`], and the restart policy
//! starts it again inside the other container's current namespace.
//!
//! - It arms only once it has seen another interface, so a container with
//!   no network at all (`--network=none`) never exits in a loop.
//! - gluetun's own VPN restart takes `wg0` down but leaves `eth0` and its
//!   default route: that never counts.
//! - It stays disarmed while an update's self-test hasn't decided, so its
//!   exit can't count as a failed attempt.
//! - A read that fails counts as "not lost": it never exits on doubt.
//! - Anywhere but Linux the setting is ignored, with one log line.

use std::time::{Duration, Instant};

/// How often the namespace is read.
pub const SAMPLE: Duration = Duration::from_secs(5);
/// How long "lost" must hold, without a break, before Lodestar exits.
pub const HOLD: Duration = Duration::from_secs(30);
/// The exit status after a network loss: `EX_TEMPFAIL`, a temporary
/// failure, so the restart policy tries again.
pub const EXIT_CODE: u8 = 75;

/// One reading of this process's network namespace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sample {
    /// Interfaces other than `lo`, up or down.
    pub interfaces: Vec<String>,
    /// An IPv4 or IPv6 default route through an interface other than `lo`.
    pub default_route: bool,
}

impl Sample {
    /// Nothing but `lo`, and no default route.
    pub fn lost(&self) -> bool {
        self.interfaces.is_empty() && !self.default_route
    }
}

/// What the rule makes of a reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Not armed yet: no interface but `lo` seen so far.
    Unarmed,
    /// Disarmed for now (an update's self-test is running).
    Held,
    /// The network is there.
    Fine,
    /// Lost, for this long so far (less than [`HOLD`]).
    Losing(Duration),
    /// Lost for [`HOLD`]: exit.
    Exit,
}

/// The detection rule, fed readings and the time they were taken.
#[derive(Debug, Default)]
pub struct Rule {
    armed: bool,
    lost_since: Option<Instant>,
}

impl Rule {
    /// `reading` is `None` when the namespace couldn't be read;
    /// `held` is true while an update's self-test hasn't decided.
    pub fn observe(&mut self, reading: Option<&Sample>, now: Instant, held: bool) -> Verdict {
        let Some(sample) = reading else {
            // Never exit on doubt.
            self.lost_since = None;
            return if self.armed {
                Verdict::Fine
            } else {
                Verdict::Unarmed
            };
        };
        if !sample.interfaces.is_empty() {
            self.armed = true;
        }
        if !self.armed {
            return Verdict::Unarmed;
        }
        if held {
            self.lost_since = None;
            return Verdict::Held;
        }
        if !sample.lost() {
            self.lost_since = None;
            return Verdict::Fine;
        }
        let since = *self.lost_since.get_or_insert(now);
        let lost_for = now.saturating_duration_since(since);
        if lost_for >= HOLD {
            Verdict::Exit
        } else {
            Verdict::Losing(lost_for)
        }
    }
}

/// The interface names in `/proc/net/dev` (two header lines, then
/// `name: counters…`), without `lo`.
pub fn parse_dev(text: &str) -> Vec<String> {
    text.lines()
        .skip(2)
        .filter_map(|l| l.split_once(':'))
        .map(|(name, _)| name.trim())
        .filter(|n| !n.is_empty() && *n != "lo")
        .map(str::to_owned)
        .collect()
}

/// `RTF_UP`.
const RTF_UP: u32 = 0x1;
/// `RTF_REJECT`: an unreachable route, never a way out.
const RTF_REJECT: u32 = 0x200;

/// Whether `/proc/net/route` has a usable IPv4 default route: destination
/// and mask 0, up, not a reject route, not through `lo`.
pub fn parse_route_v4(text: &str) -> bool {
    text.lines().skip(1).any(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        // Iface Destination Gateway Flags RefCnt Use Metric Mask …
        if f.len() < 8 || f[0] == "lo" {
            return false;
        }
        let flags = u32::from_str_radix(f[3], 16).unwrap_or(0);
        f[1] == "00000000" && f[7] == "00000000" && flags & RTF_UP != 0 && flags & RTF_REJECT == 0
    })
}

/// Whether `/proc/net/ipv6_route` has a usable IPv6 default route:
/// `::/0`, up, not a reject route, not through `lo` (the kernel always
/// lists an unreachable `::/0` on `lo`).
pub fn parse_route_v6(text: &str) -> bool {
    text.lines().any(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        // dest plen src splen next-hop metric refcnt use flags iface
        if f.len() < 10 || f[9] == "lo" {
            return false;
        }
        let flags = u32::from_str_radix(f[8], 16).unwrap_or(0);
        f[0].bytes().all(|b| b == b'0')
            && f[0].len() == 32
            && f[1] == "00"
            && flags & RTF_UP != 0
            && flags & RTF_REJECT == 0
    })
}

/// Builds a [`Sample`] from the three files' text. `None` when the
/// interface list couldn't be read; a missing route file counts as no
/// route of that family (IPv6 can be off).
pub fn sample_from(dev: Option<&str>, v4: Option<&str>, v6: Option<&str>) -> Option<Sample> {
    let interfaces = parse_dev(dev?);
    Some(Sample {
        interfaces,
        default_route: v4.is_some_and(parse_route_v4) || v6.is_some_and(parse_route_v6),
    })
}

/// Reads this process's namespace (Linux).
#[cfg(target_os = "linux")]
fn read() -> Option<Sample> {
    let r = |p: &str| std::fs::read_to_string(p).ok();
    sample_from(
        r("/proc/self/net/dev").as_deref(),
        r("/proc/self/net/route").as_deref(),
        r("/proc/self/net/ipv6_route").as_deref(),
    )
}

/// Whether the watch can run here.
pub const SUPPORTED: bool = cfg!(target_os = "linux");

/// Resolves once the network has been lost for [`HOLD`], after logging
/// it; never resolves when `enabled` is false or off Linux (where an
/// enabled setting is logged once and ignored).
pub async fn lost(enabled: bool) {
    if !enabled {
        return std::future::pending().await;
    }
    #[cfg(target_os = "linux")]
    {
        let mut rule = Rule::default();
        let mut every = tokio::time::interval(SAMPLE);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            let held = crate::update::probation::current().is_some_and(|c| c.undecided());
            let reading = tokio::task::spawn_blocking(read).await.ok().flatten();
            if rule.observe(reading.as_ref(), Instant::now(), held) == Verdict::Exit {
                lodestar_log::log_event!(
                    crate::logging::events::SYSTEM_NETWORK_LOST,
                    seconds = lodestar_log::Count(HOLD.as_secs())
                );
                return;
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        lodestar_log::log_event!(crate::logging::events::SYSTEM_NETWORK_WATCH_UNSUPPORTED);
        std::future::pending().await
    }
}

#[cfg(test)]
#[path = "net_watch_tests.rs"]
mod tests;
