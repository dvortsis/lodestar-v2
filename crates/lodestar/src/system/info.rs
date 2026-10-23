//! What `GET /system` reports about the build, the host and the data
//! directory (architecture §10.10.13). A value this platform or build
//! can't measure is `None` (null), never 0.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use lodestar_api::system::{BuildInfo, Channel, DataInfo, Deployment, DeploymentKind, HostInfo};

use crate::fsdetect;

/// The build details `build.rs` recorded (empty: unknown).
pub fn build() -> BuildInfo {
    let known = |v: &str| (!v.is_empty()).then(|| v.to_string());
    BuildInfo {
        commit: known(env!("LODESTAR_BUILD_COMMIT")),
        date: env!("LODESTAR_BUILD_DATE").parse().ok(),
        rustc: known(env!("LODESTAR_BUILD_RUSTC")),
        target: env!("LODESTAR_BUILD_TARGET").to_string(),
        ui_build_id: known(env!("LODESTAR_BUILD_UI")),
    }
}

pub fn channel() -> Channel {
    match env!("LODESTAR_BUILD_CHANNEL") {
        "stable" => Channel::Stable,
        "beta" => Channel::Beta,
        "nightly" => Channel::Nightly,
        _ => Channel::Dev,
    }
}

/// How Lodestar is installed: `LODESTAR_DEPLOYMENT` wins (packaging sets
/// it); otherwise Docker by `/.dockerenv` or the cgroup, systemd by
/// `INVOCATION_ID`, else a tarball on Linux and unknown elsewhere. Like
/// `LODESTAR_PROXY_AUTH`, it isn't a setting, so it isn't in
/// `config::ENV_VARS`. The image tag and digest come from
/// `LODESTAR_IMAGE_TAG` and `LODESTAR_IMAGE_DIGEST` when the image sets
/// them.
pub fn deployment(
    env: impl Fn(&str) -> Option<String>,
    fs: impl Fn(&str) -> Option<String>,
) -> Deployment {
    let hint = env("LODESTAR_DEPLOYMENT").and_then(|h| match h.trim() {
        "docker" => Some(DeploymentKind::Docker),
        "unraid" => Some(DeploymentKind::Unraid),
        "windows_service" => Some(DeploymentKind::WindowsService),
        "windows_portable" => Some(DeploymentKind::WindowsPortable),
        "systemd" => Some(DeploymentKind::Systemd),
        "tarball" => Some(DeploymentKind::Tarball),
        _ => None,
    });
    let in_container = || {
        fs("/.dockerenv").is_some()
            || fs("/proc/1/cgroup").is_some_and(|c| {
                c.contains("/docker") || c.contains("/kubepods") || c.contains("/containerd")
            })
    };
    let kind = hint.unwrap_or_else(|| {
        if in_container() {
            DeploymentKind::Docker
        } else if env("INVOCATION_ID").is_some() {
            DeploymentKind::Systemd
        } else if cfg!(target_os = "linux") {
            DeploymentKind::Tarball
        } else {
            DeploymentKind::Unknown
        }
    });
    let image = |var: &str| {
        matches!(kind, DeploymentKind::Docker | DeploymentKind::Unraid)
            .then(|| env(var))
            .flatten()
            .filter(|v| !v.trim().is_empty())
    };
    Deployment {
        image_digest: image("LODESTAR_IMAGE_DIGEST"),
        image_tag: image("LODESTAR_IMAGE_TAG"),
        kind,
    }
}

/// The real environment and filesystem, for [`deployment`].
pub fn deployment_here() -> Deployment {
    deployment(
        |k| std::env::var(k).ok(),
        |p| {
            let path = Path::new(p);
            if p == "/.dockerenv" {
                path.exists().then(String::new)
            } else {
                std::fs::read_to_string(path).ok()
            }
        },
    )
}

pub fn host() -> HostInfo {
    HostInfo {
        arch: std::env::consts::ARCH.to_string(),
        cpus: std::thread::available_parallelism()
            .ok()
            .map(|n| n.get() as u64),
        memory_bytes: memory_bytes(),
        os: std::env::consts::OS.to_string(),
        time_zone: time_zone(|k| std::env::var(k).ok(), read_link_or_file),
    }
}

/// Linux: `MemTotal`, or a cgroup v2 limit when lower. Elsewhere unknown
/// until a platform slice measures it.
fn memory_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let total = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|m| mem_total(&m));
        let limit = std::fs::read_to_string("/sys/fs/cgroup/memory.max")
            .ok()
            .and_then(|l| l.trim().parse::<u64>().ok());
        match (total, limit) {
            (Some(t), Some(l)) => Some(t.min(l)),
            (t, l) => t.or(l),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// `MemTotal:  16314252 kB` in bytes.
fn mem_total(meminfo: &str) -> Option<u64> {
    let line = meminfo.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    kib.checked_mul(1024)
}

/// `TZ` when it names a zone, else `/etc/timezone`, else the zone
/// `/etc/localtime` links to.
fn time_zone(
    env: impl Fn(&str) -> Option<String>,
    fs: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let zone = |s: &str| {
        let s = s.trim().trim_start_matches(':');
        (!s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "/_-+".contains(c)))
        .then(|| s.to_string())
    };
    if let Some(tz) = env("TZ").as_deref().and_then(zone) {
        return Some(tz);
    }
    if let Some(tz) = fs("/etc/timezone").as_deref().and_then(zone) {
        return Some(tz);
    }
    let target = fs("@/etc/localtime")?;
    let (_, name) = target.split_once("zoneinfo/")?;
    zone(name)
}

/// A file's contents, or with a leading `@` where a link points.
fn read_link_or_file(p: &str) -> Option<String> {
    match p.strip_prefix('@') {
        Some(link) => std::fs::read_link(link)
            .ok()
            .map(|t| t.to_string_lossy().into_owned()),
        None => std::fs::read_to_string(p).ok(),
    }
}

/// The data directory's sizes, measured at most once a minute (they walk
/// the directory).
#[derive(Debug)]
pub struct Sizes {
    dir: PathBuf,
    cache: Mutex<Option<(Instant, DataInfo)>>,
    ttl: Duration,
    /// How many times it measured, for the tests.
    #[cfg(test)]
    pub measured: std::sync::atomic::AtomicU32,
}

/// How long measured sizes are reused.
pub const SIZE_TTL: Duration = Duration::from_secs(60);

impl Sizes {
    pub fn new(dir: PathBuf) -> Sizes {
        Sizes {
            dir,
            cache: Mutex::new(None),
            ttl: SIZE_TTL,
            #[cfg(test)]
            measured: Default::default(),
        }
    }

    /// Blocking: call it off the async runtime.
    pub fn get(&self) -> DataInfo {
        let mut cache = self.cache.lock().expect("sizes lock");
        if let Some((at, info)) = &*cache
            && at.elapsed() < self.ttl
        {
            return info.clone();
        }
        let info = self.measure();
        *cache = Some((Instant::now(), info.clone()));
        info
    }

    fn measure(&self) -> DataInfo {
        #[cfg(test)]
        self.measured
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = &self.dir;
        let file = |name: &str| std::fs::metadata(dir.join(name)).ok().map(|m| m.len());
        let database = file("lodestar.db").map(|db| {
            db + file("lodestar.db-wal").unwrap_or(0) + file("lodestar.db-shm").unwrap_or(0)
        });
        let sub = |name: &str| {
            let path = dir.join(name);
            path.is_dir().then(|| crate::ops::dir_size(&path)).flatten()
        };
        let report = fsdetect::detect(dir);
        DataInfo {
            cold_bytes: sub("cold"),
            database_bytes: database,
            dir: dir.display().to_string(),
            filesystem_warning: if report.is_risky() {
                report.warning()
            } else {
                None
            },
            free_bytes: free_bytes(dir),
            index_bytes: sub("index"),
            logs_bytes: sub("logs"),
        }
    }
}

/// Bytes free to an unprivileged user on `dir`'s volume (Unix); unknown
/// elsewhere until a platform slice measures it.
fn free_bytes(dir: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        let s = rustix::fs::statvfs(dir).ok()?;
        s.f_bavail.checked_mul(s.f_frsize)
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn deployment_hint_wins_then_detection() {
        let none = |_: &str| None;
        let d = deployment(env(&[("LODESTAR_DEPLOYMENT", "unraid")]), |p| {
            (p == "/.dockerenv").then(String::new)
        });
        assert_eq!(d.kind, DeploymentKind::Unraid);
        let d = deployment(
            env(&[("LODESTAR_IMAGE_TAG", "0.5.2"), ("INVOCATION_ID", "x")]),
            |p| (p == "/.dockerenv").then(String::new),
        );
        assert_eq!(d.kind, DeploymentKind::Docker);
        assert_eq!(d.image_tag.as_deref(), Some("0.5.2"));
        assert_eq!(d.image_digest, None);
        let d = deployment(env(&[]), |p| {
            (p == "/proc/1/cgroup").then(|| "0::/system.slice/docker-abc.scope".to_string())
        });
        assert_eq!(d.kind, DeploymentKind::Docker);
        let d = deployment(env(&[("INVOCATION_ID", "abc")]), none);
        assert_eq!(d.kind, DeploymentKind::Systemd);
        // Outside an image, an image tag isn't reported.
        let d = deployment(
            env(&[("INVOCATION_ID", "abc"), ("LODESTAR_IMAGE_TAG", "x")]),
            none,
        );
        assert_eq!(d.image_tag, None);
        // An unknown hint is ignored.
        let d = deployment(env(&[("LODESTAR_DEPLOYMENT", "toaster")]), none);
        let expected = if cfg!(target_os = "linux") {
            DeploymentKind::Tarball
        } else {
            DeploymentKind::Unknown
        };
        assert_eq!(d.kind, expected);
    }

    #[test]
    fn time_zone_sources_in_order() {
        let fs = |p: &str| match p {
            "/etc/timezone" => Some("Europe/Berlin\n".to_string()),
            "@/etc/localtime" => Some("/usr/share/zoneinfo/Asia/Tokyo".to_string()),
            _ => None,
        };
        assert_eq!(
            time_zone(env(&[("TZ", ":America/New_York")]), fs).as_deref(),
            Some("America/New_York")
        );
        assert_eq!(time_zone(env(&[]), fs).as_deref(), Some("Europe/Berlin"));
        let link_only =
            |p: &str| (p == "@/etc/localtime").then(|| "../usr/share/zoneinfo/Asia/Tokyo".into());
        assert_eq!(
            time_zone(env(&[]), link_only).as_deref(),
            Some("Asia/Tokyo")
        );
        assert_eq!(time_zone(env(&[("TZ", "")]), |_| None), None);
        // Not a zone name: ignored.
        assert_eq!(time_zone(env(&[("TZ", "a b; c")]), |_| None), None);
    }

    #[test]
    fn mem_total_in_bytes() {
        assert_eq!(
            mem_total("MemTotal:       16314252 kB\nMemFree: 1 kB\n"),
            Some(16_314_252 * 1024)
        );
        assert_eq!(mem_total("nothing"), None);
    }

    #[test]
    fn sizes_are_measured_and_cached() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lodestar.db"), vec![0u8; 1000]).unwrap();
        std::fs::write(dir.path().join("lodestar.db-wal"), vec![0u8; 24]).unwrap();
        std::fs::create_dir_all(dir.path().join("index/v1-t1")).unwrap();
        std::fs::write(dir.path().join("index/v1-t1/seg"), vec![0u8; 300]).unwrap();
        let sizes = Sizes::new(dir.path().to_path_buf());
        let info = sizes.get();
        assert_eq!(info.database_bytes, Some(1024));
        assert_eq!(info.index_bytes, Some(300));
        // No cold store or log files: unknown, not 0.
        assert_eq!(info.cold_bytes, None);
        assert_eq!(info.logs_bytes, None);
        #[cfg(unix)]
        assert!(info.free_bytes.is_some_and(|b| b > 0));
        // Within the minute it isn't measured again.
        std::fs::write(dir.path().join("index/v1-t1/seg2"), vec![0u8; 50]).unwrap();
        assert_eq!(sizes.get().index_bytes, Some(300));
        assert_eq!(sizes.measured.load(std::sync::atomic::Ordering::Relaxed), 1);
        let empty = Sizes::new(dir.path().join("missing"));
        assert_eq!(empty.get().database_bytes, None);
    }

    #[test]
    fn build_info_is_what_build_rs_recorded() {
        let b = build();
        assert!(!b.target.is_empty());
        assert!(b.rustc.as_deref().is_some_and(|r| r.starts_with("rustc ")));
        // Never the build time: unknown or a real date.
        assert!(b.date.is_none_or(|d| d > 1_600_000_000));
        let _ = channel();
    }
}
