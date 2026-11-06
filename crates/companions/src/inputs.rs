//! The closed input types that generated templates are built from (plan
//! 2026-11-06-v06-companions §4.3, CMP-0 and CMP-7).
//!
//! A compose service or an Unraid template is run by a root-equivalent
//! `docker compose` or by Unraid itself, so nothing the user types reaches
//! one as a free-form string. Each value is a validated newtype, built only
//! through its parser, that refuses the values which would widen a
//! container's reach: root's id, the host network, the Docker socket and
//! Docker's state, the host's system folders, Lodestar's own data folder,
//! and parents of any of those.
//!
//! **Safe once quoted, not bare.** No value can hold a quote, a backslash,
//! a control character, `$`, `:`, `#`, `{`, `}`, `,` or a newline, so none
//! can end a quoted YAML or XML string, start a compose interpolation or
//! split a volume spec. But a [`HostPath`] may hold a space, and a name may
//! read as a YAML 1.1 keyword or number (`null`, `true`, `yes`, `0x10`).
//! So CMP-7 quotes every key and value it writes (`serde_json`, and
//! `quick-xml` for XML), passes values to a program only as separate
//! arguments, never in a shell line, and golden-tests those keywords. See
//! the crate README's CMP-7 contract.
//!
//! It also holds the two identifiers every CMP slice shares: a
//! companion's [`CompanionId`] and a release [`Version`], parsed by strict
//! grammars because versions are also read from companions' own answers.

use std::fmt;

/// Why an input was refused. Carries no part of the refused value, so it
/// can be shown or logged as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputError {
    /// Empty, or longer than the type allows.
    Length,
    /// A character outside the type's set.
    Character,
    /// A path component that is empty, `.` or `..`.
    Component,
    /// Not an absolute path.
    NotAbsolute,
    /// A path the companion must never be given (the Docker socket, `/`,
    /// the host's system folders).
    Forbidden,
    /// A number outside the allowed range.
    Range,
    /// A name with a reserved meaning (`host`, `none`, `default`).
    Reserved,
}

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            InputError::Length => "empty or too long",
            InputError::Character => "contains a character that isn't allowed",
            InputError::Component => "has an empty, `.` or `..` component",
            InputError::NotAbsolute => "isn't an absolute path",
            InputError::Forbidden => "is a path a companion may never be given",
            InputError::Range => "is out of range",
            InputError::Reserved => "is a reserved name",
        })
    }
}

impl std::error::Error for InputError {}

/// The longest host path accepted, in bytes.
pub const HOST_PATH_MAX: usize = 255;

/// Top-level host folders a companion is never given: the host's system
/// and runtime folders (the Docker socket lives under `/run` and
/// `/var/run`).
const FORBIDDEN_ROOTS: &[&str] = &[
    "bin", "boot", "dev", "etc", "lib", "lib32", "lib64", "libx32", "opt", "proc", "root", "run",
    "sbin", "snap", "sys", "usr", "var",
];

/// Folders under `/var` a companion may be given, and only strictly below
/// them: each companion's own folder under Lodestar's companions root (the
/// plan's default save path), and spool space. The rest of `/var` holds
/// Lodestar's own data and secrets (`/var/lib/lodestar`), Docker's and
/// containerd's state (`/var/lib/docker`) and, where it isn't a link,
/// `/var/run`.
const VAR_ALLOWED: &[&[&str]] = &[&["lib", "lodestar", "companions"], &["spool", "lodestar"]];

/// Lodestar's own folders in Unraid's `appdata` share
/// (`/mnt/<pool>/appdata/lodestar`, `packaging/unraid/lodestar.xml`), which
/// hold its database and `secrets.key`. Only their `companions/` subtree,
/// strictly below it, may be given to a companion.
const LODESTAR_APPDATA: &[&str] = &["lodestar", "lodestar-worker"];

/// An absolute host path to mount into a companion's container
/// (`/mnt/user/appdata/qbittorrent`).
///
/// Linux paths only: Docker and Unraid hosts are Linux. Components are
/// ASCII letters, digits, `-`, `_`, `.` and an inner space; no `.` or `..`
/// component, no trailing `/`, at most [`HOST_PATH_MAX`] bytes.
///
/// **Refused, with every folder that contains one of them** (a parent
/// exposes what it holds):
/// - `/`, and the system folders (`/etc`, `/proc`, `/run`, `/usr`, `/opt`,
///   `/var`, …);
/// - under `/var`, everything except paths strictly below
///   `/var/lib/lodestar/companions` and `/var/spool/lodestar`: that keeps out
///   Lodestar's own data (`/var/lib/lodestar`, its `secrets.key` and
///   database) and Docker's state (`/var/lib/docker`);
/// - `/home` and `/home/<user>`, which hold `~/.docker`;
/// - under `/mnt`, anything shorter than `/mnt/<pool>/<share>`; Unraid's
///   `system` share (`docker.img`, `libvirt.img`) and the `appdata` share
///   itself, on any pool; and Lodestar's own `appdata/lodestar` and
///   `appdata/lodestar-worker`, except strictly below their `companions`
///   folder;
/// - any `*.sock` and any `.docker` folder.
///
/// **What this can't see:** it checks the path as typed, for another
/// machine. A symlink into one of these places passes, and so does a
/// custom `LODESTAR_DATA_DIR` elsewhere. CMP-7 also checks against the
/// running instance's real data folder, and its output and the guide say
/// to mount real folders only.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HostPath(String);

impl HostPath {
    pub fn parse(s: &str) -> Result<HostPath, InputError> {
        if s.is_empty() || s.len() > HOST_PATH_MAX {
            return Err(InputError::Length);
        }
        let Some(rest) = s.strip_prefix('/') else {
            return Err(InputError::NotAbsolute);
        };
        if rest.is_empty() {
            return Err(InputError::Forbidden);
        }
        let mut components = Vec::new();
        for c in rest.split('/') {
            if c.is_empty() || c == "." || c == ".." {
                return Err(InputError::Component);
            }
            if !c
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b' '))
            {
                return Err(InputError::Character);
            }
            // A leading or trailing space is invisible in a form and easy
            // to get wrong; refuse it rather than guess.
            if c.starts_with(' ') || c.ends_with(' ') {
                return Err(InputError::Character);
            }
            components.push(c);
        }
        if forbidden(&components) {
            return Err(InputError::Forbidden);
        }
        Ok(HostPath(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HostPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether `c` (a path split into components, at least one) names a
/// forbidden folder or one that contains one (see [`HostPath`]).
///
/// Every comparison ignores case: a differently cased spelling is refused
/// rather than let through, which costs nothing on a case-sensitive host
/// and matters on a case-insensitive share.
fn forbidden(original: &[&str]) -> bool {
    let lower: Vec<String> = original.iter().map(|x| x.to_ascii_lowercase()).collect();
    let c: Vec<&str> = lower.iter().map(String::as_str).collect();
    // A socket or a `.docker` folder at any depth.
    if c.iter().any(|x| x.ends_with(".sock") || *x == ".docker") {
        return true;
    }
    // Strictly below `prefix`.
    let below = |prefix: &[&str]| c.len() > prefix.len() && c[..prefix.len()] == *prefix;
    match c[0] {
        "var" => !VAR_ALLOWED.iter().any(|a| {
            let mut p = vec!["var"];
            p.extend_from_slice(a);
            below(&p)
        }),
        // `/home` and `/home/<user>` hold every user's `~/.docker`; a dot
        // folder directly in a home (`~/.ssh`, `~/.local`, `~/.config`)
        // holds keys and per-user state.
        "home" => c.len() <= 2 || c[2].starts_with('.'),
        "mnt" => {
            if c.len() < 3 {
                return true;
            }
            match c[2] {
                "system" => true,
                "appdata" => {
                    c.len() == 3
                        || (LODESTAR_APPDATA.contains(&c[3])
                            && !(c.len() > 5 && c[4] == "companions"))
                }
                _ => false,
            }
        }
        first => FORBIDDEN_ROOTS.contains(&first),
    }
}

/// The largest user or group id accepted (`i32::MAX`: the kernel's
/// `uid_t` is 32 bits, and `(uid_t)-1` means "no change").
pub const ID_MAX: u32 = i32::MAX as u32;

macro_rules! linux_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name(u32);

        impl $name {
            /// Refuses `0` (root): a companion never runs as root.
            pub fn new(id: u32) -> Result<$name, InputError> {
                if id == 0 || id > ID_MAX {
                    return Err(InputError::Range);
                }
                Ok($name(id))
            }

            /// Parses decimal digits only (no sign, no spaces, at most ten
            /// digits).
            pub fn parse(s: &str) -> Result<$name, InputError> {
                $name::new(parse_decimal(s)?)
            }

            pub fn get(self) -> u32 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

linux_id!(
    /// The user id a linuxserver.io image runs its program as (`PUID`).
    Puid
);
linux_id!(
    /// The group id a linuxserver.io image runs its program as (`PGID`).
    Pgid
);

fn parse_decimal(s: &str) -> Result<u32, InputError> {
    if s.is_empty() || s.len() > 10 {
        return Err(InputError::Length);
    }
    if !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(InputError::Character);
    }
    s.parse::<u32>().map_err(|_| InputError::Range)
}

/// The lowest port accepted: companions never need a privileged port, and
/// never run with the right to bind one.
pub const PORT_MIN: u16 = 1024;

/// A TCP or UDP port number, `1024..=65535`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PortNumber(u16);

impl PortNumber {
    pub fn new(port: u16) -> Result<PortNumber, InputError> {
        if port < PORT_MIN {
            return Err(InputError::Range);
        }
        Ok(PortNumber(port))
    }

    /// Parses decimal digits only.
    pub fn parse(s: &str) -> Result<PortNumber, InputError> {
        if s.len() > 5 {
            return Err(InputError::Length);
        }
        let n = parse_decimal(s)?;
        PortNumber::new(u16::try_from(n).map_err(|_| InputError::Range)?)
    }

    pub fn get(self) -> u16 {
        self.0
    }
}

impl fmt::Display for PortNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The longest container or network name accepted.
pub const NAME_MAX: usize = 63;

/// Docker's names with a meaning of their own: `host` shares the host's
/// network stack, `none` and `default` aren't user networks, and `bridge`
/// is written as the default rather than named.
const RESERVED_NETWORKS: &[&str] = &["host", "none", "default", "bridge"];

/// Lower-case ASCII letters and digits, then letters, digits, `_`, `.` and
/// `-`; at most [`NAME_MAX`] bytes. Docker's own rule, narrowed to lower
/// case so that a name compares the same everywhere.
fn check_name(s: &str) -> Result<(), InputError> {
    if s.is_empty() || s.len() > NAME_MAX {
        return Err(InputError::Length);
    }
    let b = s.as_bytes();
    if !(b[0].is_ascii_lowercase() || b[0].is_ascii_digit()) {
        return Err(InputError::Character);
    }
    if !b
        .iter()
        .all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'.' | b'-'))
    {
        return Err(InputError::Character);
    }
    Ok(())
}

/// Names a container resolves through its own `/etc/hosts`, or that are
/// Lodestar's own.
const RESERVED_HOSTS: &[&str] = &["localhost", "lodestar", "lodestar-worker"];

/// A container name (`qbittorrent`): what detection looks up as a host on
/// Docker's network, and the compose service's name.
///
/// Because it's used as a host name, it's narrower than Docker's rule: a
/// lower-case letter first, then lower-case letters, digits, `_` and `-`;
/// no `.`. So it can never be an IP address (`169.254.169.254`,
/// `2130706433`) or a domain name (`qbit.example.com`).
///
/// Inside a container a name is looked up in `/etc/hosts` before Docker's
/// DNS, so the names Docker writes there (`localhost`, `ip6-localhost`,
/// `ip6-loopback` and the other `ip6-*` names) are refused, and so are
/// Lodestar's own container names (`lodestar`, `lodestar-worker`), and
/// names shaped like a container id (12 or 64 hex digits), which is
/// Docker's default hostname.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContainerName(String);

impl ContainerName {
    pub fn parse(s: &str) -> Result<ContainerName, InputError> {
        check_name(s)?;
        let b = s.as_bytes();
        if !b[0].is_ascii_lowercase() || b.contains(&b'.') {
            return Err(InputError::Character);
        }
        if RESERVED_HOSTS.contains(&s) || s.starts_with("ip6-") || s.starts_with("localhost") {
            return Err(InputError::Reserved);
        }
        // A container's `/etc/hosts` also maps its own hostname, by default
        // its id: 12 or 64 hex digits.
        if (s.len() == 12 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(InputError::Reserved);
        }
        Ok(ContainerName(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ContainerName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A user-defined Docker network's name (`lodestar`). `host`, `none`,
/// `default` and `bridge` are refused.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NetworkName(String);

impl NetworkName {
    pub fn parse(s: &str) -> Result<NetworkName, InputError> {
        check_name(s)?;
        if RESERVED_NETWORKS.contains(&s) {
            return Err(InputError::Reserved);
        }
        Ok(NetworkName(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NetworkName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// How a generated companion service is networked. There is no way to
/// ask for the host's network or another container's stack, except
/// gluetun's, which is how companion traffic follows the VPN (§18.10).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NetworkMode {
    /// Joins a user-defined network.
    Network(NetworkName),
    /// `network_mode: service:gluetun`: all traffic through gluetun.
    ServiceGluetun,
}

impl NetworkMode {
    /// The value as written in compose: the network's name, which goes
    /// under the service's `networks:`, or `service:gluetun`, which goes
    /// under `network_mode:`. CMP-7 picks the key by the variant, never
    /// by this text.
    pub fn compose_value(&self) -> &str {
        match self {
            NetworkMode::Network(n) => n.as_str(),
            NetworkMode::ServiceGluetun => "service:gluetun",
        }
    }
}

/// A companion's id: `[a-z][a-z0-9-]{0,31}`, not ending in `-`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CompanionId(String);

impl CompanionId {
    pub fn parse(s: &str) -> Result<CompanionId, InputError> {
        let b = s.as_bytes();
        if b.is_empty() || b.len() > 32 {
            return Err(InputError::Length);
        }
        if !b[0].is_ascii_lowercase()
            || b[b.len() - 1] == b'-'
            || !b
                .iter()
                .all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        {
            return Err(InputError::Character);
        }
        Ok(CompanionId(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CompanionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A release version: one to four dot-separated decimal numbers, each at
/// most nine digits with no leading zero (`5.0.3`, `1.28.2.4885`).
/// Compared number by number; a missing part counts as `0`.
#[derive(Debug, Clone)]
pub struct Version {
    parts: [u32; 4],
    text: String,
}

impl Version {
    pub fn parse(s: &str) -> Result<Version, InputError> {
        if s.is_empty() || s.len() > 39 {
            return Err(InputError::Length);
        }
        let mut parts = [0u32; 4];
        for (n, p) in s.split('.').enumerate() {
            if n == 4
                || p.is_empty()
                || p.len() > 9
                || !p.bytes().all(|b| b.is_ascii_digit())
                || (p.len() > 1 && p.starts_with('0'))
            {
                return Err(InputError::Character);
            }
            parts[n] = p.parse().map_err(|_| InputError::Range)?;
        }
        Ok(Version {
            parts,
            text: s.to_owned(),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Version) -> bool {
        self.parts == other.parts
    }
}

impl Eq for Version {}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Version) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Version) -> std::cmp::Ordering {
        self.parts.cmp(&other.parts)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_paths() {
        for ok in [
            "/mnt/user/appdata/qbittorrent",
            "/srv/downloads",
            "/mnt/user/My Media/tv",
            "/var/lib/lodestar/companions/qbittorrent/downloads",
            "/var/lib/lodestar/companions/qbittorrent",
            "/var/spool/lodestar/x",
            "/home/me/downloads",
            "/home/me/media/.cache",
            "/mnt/cache/appdata/prowlarr",
            "/mnt/user/appdata/lodestar/companions/qbittorrent",
            "/mnt/disk1/media",
        ] {
            assert_eq!(HostPath::parse(ok).unwrap().as_str(), ok);
        }
        let cases: &[(&str, InputError)] = &[
            ("", InputError::Length),
            ("relative/path", InputError::NotAbsolute),
            ("/", InputError::Forbidden),
            ("/mnt//user", InputError::Component),
            ("/mnt/user/", InputError::Component),
            ("/mnt/../etc", InputError::Component),
            ("/mnt/./x", InputError::Component),
            ("/mnt/a:b", InputError::Character),
            ("/mnt/$HOME", InputError::Character),
            ("/mnt/a\"b", InputError::Character),
            ("/mnt/a\nb", InputError::Character),
            ("/mnt/a\\b", InputError::Character),
            ("/mnt/a,b", InputError::Character),
            ("/mnt/é", InputError::Character),
            ("/mnt/ lead", InputError::Character),
            ("/mnt/trail ", InputError::Character),
            ("/etc", InputError::Forbidden),
            ("/etc/qbittorrent", InputError::Forbidden),
            ("/proc/self", InputError::Forbidden),
            ("/run/docker.sock", InputError::Forbidden),
            ("/var/run/docker.sock", InputError::Forbidden),
            ("/var/run", InputError::Forbidden),
            ("/mnt/docker.sock", InputError::Forbidden),
            ("/mnt/DOCKER.SOCK", InputError::Forbidden),
            ("/srv/containerd.sock", InputError::Forbidden),
            ("/root", InputError::Forbidden),
            ("/sys/fs/cgroup", InputError::Forbidden),
            ("/var", InputError::Forbidden),
            ("/var/lib", InputError::Forbidden),
            ("/var/lib/docker", InputError::Forbidden),
            ("/var/lib/docker/volumes/x", InputError::Forbidden),
            ("/var/lib/containerd", InputError::Forbidden),
            ("/var/lib/lodestarx", InputError::Forbidden),
            ("/var/spool", InputError::Forbidden),
            ("/opt", InputError::Forbidden),
            ("/snap/x", InputError::Forbidden),
            ("/lib32", InputError::Forbidden),
            ("/mnt/x.SOCK", InputError::Forbidden),
            ("/home/me/.docker", InputError::Forbidden),
            ("/home/me/.docker/run", InputError::Forbidden),
            ("/home/me/.DOCKER", InputError::Forbidden),
            ("/mnt/user/system", InputError::Forbidden),
            ("/mnt/cache/system/docker", InputError::Forbidden),
            ("/mnt/user/System", InputError::Forbidden),
            // Lodestar's own data folder and secrets (systemd and Unraid
            // defaults), and their parents.
            ("/var/lib/lodestar", InputError::Forbidden),
            ("/var/lib/lodestar/secrets.key", InputError::Forbidden),
            ("/var/lib/lodestar/lodestar.db", InputError::Forbidden),
            ("/var/lib/lodestar/companions", InputError::Forbidden),
            ("/var/spool/lodestar", InputError::Forbidden),
            ("/mnt/cache/appdata/lodestar", InputError::Forbidden),
            (
                "/mnt/user/appdata/lodestar/secrets.key",
                InputError::Forbidden,
            ),
            (
                "/mnt/user/appdata/lodestar/lodestar.db",
                InputError::Forbidden,
            ),
            ("/mnt/user/appdata/Lodestar", InputError::Forbidden),
            (
                "/mnt/user/appdata/lodestar/companions",
                InputError::Forbidden,
            ),
            (
                "/mnt/user/appdata/lodestar/companionsx/a",
                InputError::Forbidden,
            ),
            ("/mnt/user/appdata/lodestar-worker", InputError::Forbidden),
            (
                "/mnt/user/appdata/lodestar-worker/certs",
                InputError::Forbidden,
            ),
            // Parents of the system share, of every container's config, and
            // of `~/.docker`.
            ("/mnt", InputError::Forbidden),
            ("/mnt/cache", InputError::Forbidden),
            ("/mnt/user", InputError::Forbidden),
            ("/mnt/user/appdata", InputError::Forbidden),
            ("/mnt/cache/AppData", InputError::Forbidden),
            ("/home", InputError::Forbidden),
            ("/home/me", InputError::Forbidden),
            ("/home/me/.ssh", InputError::Forbidden),
            ("/home/me/.ssh/id_ed25519", InputError::Forbidden),
            ("/home/me/.local/share/docker", InputError::Forbidden),
            ("/home/me/.config/qbittorrent", InputError::Forbidden),
            ("/HOME/me", InputError::Forbidden),
            ("/Var/Lib/Lodestar", InputError::Forbidden),
            ("/MNT/user", InputError::Forbidden),
            (
                "/mnt/user/appdata/lodestar/Companions",
                InputError::Forbidden,
            ),
        ];
        for (s, e) in cases {
            assert_eq!(HostPath::parse(s), Err(*e), "{s:?}");
        }
        let long = format!("/{}", "a".repeat(HOST_PATH_MAX));
        assert_eq!(HostPath::parse(&long), Err(InputError::Length));
        let max = format!("/{}", "a".repeat(HOST_PATH_MAX - 1));
        assert!(HostPath::parse(&max).is_ok());
    }

    #[test]
    fn ids() {
        assert_eq!(Puid::parse("99").unwrap().get(), 99);
        assert_eq!(Pgid::parse("100").unwrap().get(), 100);
        assert_eq!(Puid::parse("0"), Err(InputError::Range));
        assert_eq!(Pgid::new(0), Err(InputError::Range));
        assert_eq!(Puid::parse("-1"), Err(InputError::Character));
        assert_eq!(Puid::parse("+1"), Err(InputError::Character));
        assert_eq!(Puid::parse(" 1"), Err(InputError::Character));
        assert_eq!(Puid::parse(""), Err(InputError::Length));
        assert_eq!(Puid::parse("12345678901"), Err(InputError::Length));
        assert_eq!(Puid::parse("4294967295"), Err(InputError::Range));
        assert_eq!(Puid::parse("4294967296"), Err(InputError::Range));
        assert_eq!(Puid::new(ID_MAX).unwrap().get(), ID_MAX);
        assert_eq!(Puid::new(ID_MAX + 1), Err(InputError::Range));
        assert_eq!(Puid::new(1000).unwrap().to_string(), "1000");
    }

    #[test]
    fn ports() {
        assert_eq!(PortNumber::parse("8080").unwrap().get(), 8080);
        assert_eq!(PortNumber::parse("65535").unwrap().get(), 65535);
        assert_eq!(PortNumber::parse("1024").unwrap().get(), 1024);
        assert_eq!(PortNumber::parse("1023"), Err(InputError::Range));
        assert_eq!(PortNumber::parse("0"), Err(InputError::Range));
        assert_eq!(PortNumber::parse("65536"), Err(InputError::Range));
        assert_eq!(PortNumber::parse("99999"), Err(InputError::Range));
        assert_eq!(PortNumber::parse("123456"), Err(InputError::Length));
        assert_eq!(PortNumber::parse("80a"), Err(InputError::Character));
        assert_eq!(PortNumber::new(80), Err(InputError::Range));
    }

    #[test]
    fn names() {
        assert!(ContainerName::parse("qbittorrent").is_ok());
        assert!(ContainerName::parse("lodestar-prowlarr_1").is_ok());
        // Never an address or a domain: it's looked up as a host.
        for host in [
            "169.254.169.254",
            "2130706433",
            "qbit.example.com",
            "9lives",
            "a.b",
        ] {
            assert_eq!(
                ContainerName::parse(host),
                Err(InputError::Character),
                "{host}"
            );
        }
        assert!(NetworkName::parse("9lives.net").is_ok());
        // Resolved through the container's own /etc/hosts, or Lodestar's own.
        for reserved in [
            "localhost",
            "ip6-localhost",
            "ip6-loopback",
            "ip6-allnodes",
            "ip6-allrouters",
            "ip6-localnet",
            "ip6-mcastprefix",
            "lodestar",
            "lodestar-worker",
            "localhost4",
        ] {
            assert_eq!(
                ContainerName::parse(reserved),
                Err(InputError::Reserved),
                "{reserved}"
            );
        }
        assert!(ContainerName::parse("lodestar-prowlarr").is_ok());
        assert!(ContainerName::parse("abcdef01234").is_ok());
        assert!(ContainerName::parse("abcdef0123456").is_ok());
        assert_eq!(ContainerName::parse(""), Err(InputError::Length));
        assert_eq!(
            ContainerName::parse(&"a".repeat(64)),
            Err(InputError::Length)
        );
        assert!(ContainerName::parse(&"a".repeat(63)).is_ok());
        for bad in ["-x", ".x", "_x", "Qbit", "a b", "a:b", "a/b", "a$b", "a\"b"] {
            assert_eq!(
                ContainerName::parse(bad),
                Err(InputError::Character),
                "{bad}"
            );
        }
        assert!(NetworkName::parse("lodestar").is_ok());
        for r in ["host", "none", "default", "bridge"] {
            assert_eq!(NetworkName::parse(r), Err(InputError::Reserved));
        }
        assert_eq!(
            NetworkName::parse("container:x"),
            Err(InputError::Character)
        );
        assert_eq!(
            NetworkMode::Network(NetworkName::parse("lodestar").unwrap()).compose_value(),
            "lodestar"
        );
        assert_eq!(
            NetworkMode::ServiceGluetun.compose_value(),
            "service:gluetun"
        );
    }

    #[test]
    fn companion_ids() {
        for ok in ["qbittorrent", "a", "flare-solverr", "x9"] {
            assert!(CompanionId::parse(ok).is_ok(), "{ok}");
        }
        let long = "a".repeat(33);
        for bad in [
            "",
            "Qbit",
            "9x",
            "-x",
            "x-",
            "a_b",
            "a.b",
            "a/b",
            long.as_str(),
        ] {
            assert!(CompanionId::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(
            CompanionId::parse("prowlarr").unwrap().to_string(),
            "prowlarr"
        );
    }

    #[test]
    fn versions() {
        let v = |s| Version::parse(s).unwrap();
        assert!(v("5.0.3") < v("5.0.10"));
        assert!(v("1.28.2.4885") > v("1.28.2"));
        assert_eq!(v("5.0"), v("5.0.0"));
        assert_eq!(v("1.28.2.4885").as_str(), "1.28.2.4885");
        assert_eq!(v("1.28.2.4885").to_string(), "1.28.2.4885");
        for bad in [
            "",
            "1..2",
            "1.2.3.4.5",
            "01.2",
            "v1",
            "1.2-beta",
            "1.2 ",
            "1234567890",
            "+1",
            ".1",
            "1.",
        ] {
            assert!(Version::parse(bad).is_err(), "{bad}");
        }
        assert!(Version::parse("0.0.0").is_ok());
        assert!(Version::parse("999999999.0").is_ok());
        assert!(Version::parse(&"1.".repeat(20)).is_err());
    }

    #[test]
    fn errors_never_echo_the_value() {
        let e = HostPath::parse("/etc/secret-thing").unwrap_err();
        assert!(!e.to_string().contains("secret"));
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    /// Characters that would mean something to YAML, compose, a volume
    /// spec, XML or a shell.
    const SPECIAL: &[char] = &[
        ':', '$', '"', '\'', '\\', '\n', '\r', '\t', '#', '{', '}', '[', ']', ',', '&', '*', '!',
        '|', '>', '<', '%', '@', '`', ';', '=', '\0',
    ];

    /// What a companion must never be able to reach: each is refused, and
    /// so is every folder above it.
    const SENSITIVE: &[&str] = &[
        "/var/lib/lodestar/secrets.key",
        "/var/lib/lodestar/lodestar.db",
        "/var/lib/docker/containers",
        "/var/run/docker.sock",
        "/run/docker.sock",
        "/etc/passwd",
        "/proc/1/environ",
        "/mnt/user/system/docker/docker.img",
        "/mnt/cache/system/docker/docker.img",
        "/mnt/user/appdata/lodestar/secrets.key",
        "/mnt/cache/appdata/lodestar/lodestar.db",
        "/mnt/user/appdata/lodestar-worker/certs",
        // Every container's config at once (a companion's own
        // `appdata/<name>` folder is a legitimate mount).
        "/mnt/user/appdata",
        "/home/me/.ssh/id_ed25519",
        "/home/me/.docker/desktop/docker.sock",
        "/home/me/.docker/run/docker.sock",
    ];

    /// Path components biased towards the interesting ones, so most
    /// generated paths are valid and many sit next to a refused folder.
    fn component() -> impl Strategy<Value = String> {
        prop_oneof![
            8 => proptest::sample::select(vec![
                "mnt", "user", "cache", "disk1", "appdata", "lodestar", "Lodestar",
                "lodestar-worker", "companions", "system", "System", "var", "lib", "spool",
                "home", "me", ".docker", "docker.sock", "x.sock", "etc", "srv", "media",
                "qbittorrent", "prowlarr", "run", "proc", "docker", "secrets.key", "a b",
            ])
            .prop_map(str::to_owned),
            2 => "[A-Za-z0-9._-]{1,8}",
            1 => proptest::sample::select(vec!["..", ".", " x", "x ", "a:b", "$x", "a\"b", "é"])
                .prop_map(str::to_owned),
        ]
    }

    fn path() -> impl Strategy<Value = String> {
        proptest::collection::vec(component(), 0..7).prop_map(|c| format!("/{}", c.join("/")))
    }

    /// Names biased towards hosts, addresses and Docker's alphabet.
    fn name() -> impl Strategy<Value = String> {
        prop_oneof![
            4 => "[a-z][a-z0-9_-]{0,12}",
            2 => "[a-z0-9._-]{1,12}",
            1 => "[0-9]{1,3}\\.[0-9]{1,3}\\.[0-9]{1,3}\\.[0-9]{1,3}",
            1 => "[0-9]{1,10}",
            1 => "[a-z]{1,6}\\.[a-z]{2,4}",
            1 => "\\PC{0,16}",
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2048))]

        #[test]
        fn accepted_paths_reach_nothing_sensitive(p in path()) {
            if let Ok(h) = HostPath::parse(&p) {
                let got = h.as_str().to_ascii_lowercase();
                prop_assert!(!got.contains(SPECIAL));
                prop_assert!(!got.split('/').any(|c| c == ".." || c == "."));
                for s in SENSITIVE {
                    let s = s.to_ascii_lowercase();
                    prop_assert!(
                        s != got && !s.starts_with(&format!("{got}/")),
                        "{} exposes {}", h.as_str(), s
                    );
                }
            }
        }

        #[test]
        fn accepted_names_are_never_addresses_or_domains(n in name()) {
            if let Ok(c) = ContainerName::parse(&n) {
                prop_assert!(!c.as_str().contains('.'));
                prop_assert!(c.as_str().parse::<std::net::IpAddr>().is_err());
                prop_assert!(c.as_str().parse::<u64>().is_err());
                prop_assert!(!c.as_str().contains(SPECIAL));
            }
            if let Ok(nw) = NetworkName::parse(&n) {
                prop_assert!(!nw.as_str().contains(SPECIAL));
                prop_assert!(!RESERVED_NETWORKS.contains(&nw.as_str()));
            }
        }

        #[test]
        fn accepted_numbers_stay_in_range(s in "[0-9+ -]{0,12}") {
            if let Ok(p) = PortNumber::parse(&s) {
                prop_assert!(p.get() >= PORT_MIN);
            }
            if let Ok(id) = Puid::parse(&s) {
                prop_assert!(id.get() != 0 && id.get() <= ID_MAX);
            }
            if let Ok(v) = Version::parse(&s) {
                prop_assert!(v.as_str().bytes().all(|b| b.is_ascii_digit()));
            }
        }

        #[test]
        fn any_path_with_a_special_character_is_refused(
            a in "[a-z]{1,8}", c in proptest::sample::select(SPECIAL), b in "[a-z]{0,8}"
        ) {
            let s = format!("/srv/{a}{c}{b}");
            prop_assert!(HostPath::parse(&s).is_err());
        }

        #[test]
        fn arbitrary_text_never_panics(s in "\\PC{0,80}") {
            let _ = HostPath::parse(&s);
            let _ = ContainerName::parse(&s);
            let _ = NetworkName::parse(&s);
            let _ = CompanionId::parse(&s);
            let _ = Version::parse(&s);
            if let Ok(id) = CompanionId::parse(&s) {
                prop_assert!(!id.as_str().contains(SPECIAL));
            }
        }
    }

    /// Every folder on the way to each sensitive path is refused, the path
    /// itself included, in any mix of case. The generator above rarely
    /// reaches depth three, so this walks them all.
    #[test]
    fn every_prefix_of_a_sensitive_path_is_refused_in_any_case() {
        // A fixed xorshift, so the case mixes are the same on every run.
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for s in SENSITIVE {
            let comps: Vec<&str> = s[1..].split('/').collect();
            for depth in 1..=comps.len() {
                let prefix = format!("/{}", comps[..depth].join("/"));
                for variant in 0..8 {
                    let p: String = if variant == 0 {
                        prefix.clone()
                    } else {
                        prefix
                            .chars()
                            .map(|ch| {
                                if next() & 1 == 1 {
                                    ch.to_ascii_uppercase()
                                } else {
                                    ch
                                }
                            })
                            .collect()
                    };
                    assert!(
                        HostPath::parse(&p).is_err(),
                        "{p} (towards {s}) was accepted"
                    );
                }
            }
        }
    }

    /// The generators aren't vacuous: a good share of what they produce is
    /// accepted.
    #[test]
    fn generators_produce_valid_values() {
        use proptest::strategy::ValueTree;
        use proptest::test_runner::TestRunner;
        let mut runner = TestRunner::deterministic();
        let (mut paths, mut names) = (0, 0);
        for _ in 0..1000 {
            let p = path().new_tree(&mut runner).unwrap().current();
            paths += usize::from(HostPath::parse(&p).is_ok());
            let n = name().new_tree(&mut runner).unwrap().current();
            names += usize::from(ContainerName::parse(&n).is_ok());
        }
        assert!(
            paths > 150,
            "only {paths} of 1000 generated paths were valid"
        );
        assert!(
            names > 100,
            "only {names} of 1000 generated names were valid"
        );
    }
}
