//! A worker's own budget and intensity (architecture §18.4, §19.5;
//! workers plan WK-10c), and the process figures its heartbeat reports.
//!
//! §18.4's scheduler v1 sets the global budget from the intensity preset:
//! outbound DHT queries (the DHT's own preset) and TCP sockets (the fetch
//! pool). A worker does the same on its own NIC and router, from its
//! `Assign`. The budget's fields (WK-5) narrow it; 0 means no cap beyond
//! the defaults:
//!
//! - **`ram_mb`** caps the fetch pool's keys, the one part of a worker's
//!   memory that grows with its settings (plan Q5: at most about 0.95 KB a
//!   key), after [`FIXED_MIB`] for everything else. It never goes below
//!   [`MIN_POOL`] keys, so a budget too small to work in still fetches.
//! - **`cpu_permille`** lowers the intensity: at most `Balanced` at half
//!   the machine or less, `Low` at a quarter or less. The DHT and the
//!   fetches are what a worker spends its CPU on.
//! - **`up_kbps` / `down_kbps`** aren't applied yet: neither the DHT nor
//!   the pool has a byte-rate limiter. The intensity is what bounds them.

use lodestar_cluster::assign::{Assignment, Intensity};

/// The worker's socket ceiling (WK-10b: 200, within its 4 MiB held).
const SOCKETS: usize = super::runtime::MAX_SOCKETS;
/// MiB a worker uses besides its fetch pool, at defaults and 10M stored:
/// the measured worst case, about 212 MiB during a have-filter swap,
/// less the pool's 44 (`docs/decisions/2026-11-08-WK-10c.md`).
pub const FIXED_MIB: u64 = 168;
/// Bytes the pool takes per key (plan Q5, the observed upper bound).
const BYTES_PER_KEY: u64 = 973;
pub const MIN_POOL: u32 = 1_000;

/// What a worker runs with, from its assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub intensity: Intensity,
    pub max_sockets: usize,
    pub pool_cap: u32,
}

impl Limits {
    pub fn of(a: &Assignment) -> Limits {
        let b = a.budget;
        let intensity = match b.cpu_permille {
            0 | 501.. => a.intensity,
            251..=500 => a.intensity.min_with(Intensity::Balanced),
            _ => Intensity::Low,
        };
        // §6's presets are 100 / 400 / 1000 sockets; a worker stops at
        // its own ceiling.
        let max_sockets = match intensity {
            Intensity::Low => 100,
            Intensity::Balanced | Intensity::Aggressive => SOCKETS,
        }
        .min(SOCKETS);
        let pool_cap = match b.ram_mb {
            0 => a.pool_cap,
            mb => {
                // `ram_mb` is in MiB, as `Hello` and the heartbeat report.
                let room = (u64::from(mb) << 20).saturating_sub(FIXED_MIB << 20);
                let keys = u32::try_from(room / BYTES_PER_KEY).unwrap_or(u32::MAX);
                a.pool_cap.min(keys.max(MIN_POOL))
            }
        };
        Limits {
            intensity,
            max_sockets,
            pool_cap,
        }
    }
}

trait MinWith {
    fn min_with(self, other: Self) -> Self;
}

impl MinWith for Intensity {
    fn min_with(self, other: Intensity) -> Intensity {
        let rank = |i| match i {
            Intensity::Low => 0,
            Intensity::Balanced => 1,
            Intensity::Aggressive => 2,
        };
        if rank(self) <= rank(other) {
            self
        } else {
            other
        }
    }
}

/// The process's CPU time and resident memory, for the heartbeat. Read
/// from `/proc` on Linux; elsewhere both read as 0 (not reported yet).
#[derive(Debug, Default)]
pub struct Process {
    last: Option<(std::time::Instant, u64)>,
}

impl Process {
    /// CPU use since the last call, in ‰ of the CPUs this process may use
    /// (`available_parallelism`: a container's or affinity's limit), and the
    /// resident memory in MB. The first call reports 0 ‰.
    pub fn sample(&mut self) -> (u16, u32) {
        let now = std::time::Instant::now();
        let rss_mb = rss_kib().map_or(0, |k| u32::try_from(k >> 10).unwrap_or(u32::MAX));
        let Some(cpu) = cpu_ticks() else {
            return (0, rss_mb);
        };
        let permille = match self.last.replace((now, cpu)) {
            Some((then, before)) => {
                let wall = now.duration_since(then).as_secs_f64();
                let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f64;
                // `/proc` counts in USER_HZ, which Linux fixes at 100.
                let used = cpu.saturating_sub(before) as f64 / 100.0;
                if wall > 0.0 {
                    (used / (wall * cores) * 1000.0).clamp(0.0, 1000.0) as u16
                } else {
                    0
                }
            }
            None => 0,
        };
        (permille, rss_mb)
    }
}

/// The memory this process may use, in MiB, for `Hello`: the cgroup's
/// limit when one is set (a container's), else the machine's. 0 where
/// neither reads (not Linux).
pub fn ram_mb() -> u32 {
    crate::system::info::memory_bytes().map_or(0, |b| u32::try_from(b >> 20).unwrap_or(u32::MAX))
}

/// `utime + stime` from `/proc/self/stat`, in clock ticks.
fn cpu_ticks() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    parse_stat(&stat)
}

fn parse_stat(stat: &str) -> Option<u64> {
    // The command name (field 2) is in parentheses and may hold spaces;
    // the fields after its closing one start at field 3.
    let rest = &stat[stat.rfind(')')? + 1..];
    let mut f = rest.split_ascii_whitespace();
    let utime: u64 = f.nth(11)?.parse().ok()?;
    let stime: u64 = f.next()?.parse().ok()?;
    Some(utime + stime)
}

/// `VmRSS` from `/proc/self/status`, in KiB.
pub fn rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    parse_status(&status)
}

fn parse_status(status: &str) -> Option<u64> {
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    line["VmRSS:".len()..]
        .split_ascii_whitespace()
        .next()?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_cluster::assign::Budget;

    fn assignment(intensity: Intensity, budget: Budget, pool_cap: u32) -> Assignment {
        Assignment {
            revision: 1,
            dht: None,
            fetch: true,
            prepare: true,
            intensity,
            budget,
            pool_cap,
            dict_version: 0,
            filter_version: 0,
            politeness: lodestar_cluster::assign::Politeness::ALONE,
        }
    }

    #[test]
    fn no_budget_keeps_the_assignment() {
        let l = Limits::of(&assignment(
            Intensity::Aggressive,
            Budget::default(),
            50_000,
        ));
        assert_eq!(
            l,
            Limits {
                intensity: Intensity::Aggressive,
                max_sockets: SOCKETS,
                pool_cap: 50_000
            }
        );
        let low = Limits::of(&assignment(Intensity::Low, Budget::default(), 50_000));
        assert_eq!(low.max_sockets, 100);
    }

    #[test]
    fn a_cpu_share_lowers_the_intensity() {
        let at = |cpu_permille| {
            let b = Budget {
                cpu_permille,
                ..Budget::default()
            };
            Limits::of(&assignment(Intensity::Aggressive, b, 50_000)).intensity
        };
        assert_eq!(at(1000), Intensity::Aggressive);
        assert_eq!(at(501), Intensity::Aggressive);
        assert_eq!(at(500), Intensity::Balanced);
        assert_eq!(at(251), Intensity::Balanced);
        assert_eq!(at(250), Intensity::Low);
        assert_eq!(at(1), Intensity::Low);
        // Never raised: a Low worker stays Low at half the machine.
        let b = Budget {
            cpu_permille: 400,
            ..Budget::default()
        };
        assert_eq!(
            Limits::of(&assignment(Intensity::Low, b, 50_000)).intensity,
            Intensity::Low
        );
    }

    #[test]
    fn a_memory_cap_shrinks_the_pool_but_never_below_its_floor() {
        let at = |ram_mb, cap| {
            let b = Budget {
                ram_mb,
                ..Budget::default()
            };
            Limits::of(&assignment(Intensity::Balanced, b, cap)).pool_cap
        };
        // 256 MiB: 88 MiB for the pool, about 94k keys, so 50k stands,
        // and an operator's 1M is cut to what fits.
        assert_eq!(at(256, 50_000), 50_000);
        let fits = ((256 - FIXED_MIB) << 20) / BYTES_PER_KEY;
        assert_eq!(u64::from(at(256, 1_000_000)), fits);
        assert!(fits * BYTES_PER_KEY + (FIXED_MIB << 20) <= 256 << 20);
        // 200 MiB: 32 MiB, about 34k keys.
        let fits = ((200 - FIXED_MIB) << 20) / BYTES_PER_KEY;
        assert_eq!(u64::from(at(200, 50_000)), fits);
        // Less than the fixed part: the floor.
        assert_eq!(at(100, 50_000), MIN_POOL);
        // Never raised above what the coordinator set.
        assert_eq!(at(4096, 500), 500);
    }

    #[test]
    fn proc_files_parse() {
        let stat = "1234 (lode star) S 1 2 3 4 5 6 7 8 9 10 250 50 0 0 20 0 9 0 100";
        assert_eq!(parse_stat(stat), Some(300));
        assert_eq!(parse_stat("garbage"), None);
        let status = "Name:\tlodestar\nVmPeak:\t  9000 kB\nVmRSS:\t  204800 kB\n";
        assert_eq!(parse_status(status), Some(204_800));
        assert_eq!(parse_status("Name: x\n"), None);
    }

    #[test]
    fn sampling_reads_this_process() {
        let mut p = Process::default();
        let (cpu, rss) = p.sample();
        assert_eq!(cpu, 0, "the first sample has nothing to compare with");
        if cfg!(target_os = "linux") {
            assert!(rss > 0, "a running test has resident memory");
        }
        let (cpu, _) = p.sample();
        assert!(cpu <= 1000);
        if cfg!(target_os = "linux") {
            assert!(ram_mb() > 0);
        }
    }
}
