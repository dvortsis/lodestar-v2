//! The closed input types that generated templates are built from (plan
//! 2026-11-06-v06-companions §4.3, CMP-0 and CMP-7).
//!
//! A compose service or an Unraid template is run by a root-equivalent
//! `docker compose` or by Unraid itself, so nothing the user types reaches
//! one as a free-form string. Each value is a validated newtype: it can be
//! built only through its parser, holds only characters that mean nothing
//! special to YAML, XML, compose interpolation (`$`), volume syntax (`:`)
//! or a shell, and refuses the values that would widen a container's reach
//! (root's id, the host network, the Docker socket, the host's system
//! folders). CMP-7 still quotes every scalar with `serde_json` and writes
//! XML through `quick-xml`; these types are the first of the two layers.

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
    "bin", "boot", "dev", "etc", "lib", "lib64", "proc", "root", "run", "sbin", "sys", "usr",
];

/// An absolute host path to mount into a companion's container
/// (`/mnt/user/appdata/qbittorrent`).
///
/// Linux paths only: Docker and Unraid hosts are Linux. Components are
/// ASCII letters, digits, `-`, `_`, `.` and space; no `.` or `..`
/// component, no trailing `/`, at most [`HOST_PATH_MAX`] bytes. `/`, the
/// folders in the system list (`/etc`, `/proc`, `/run`, …), `/var/run` and
/// any path naming `docker.sock` are refused.
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
        let first = components[0];
        if FORBIDDEN_ROOTS.contains(&first)
            || (first == "var" && components.get(1) == Some(&"run"))
            || components
                .iter()
                .any(|c| c.eq_ignore_ascii_case("docker.sock") || c.ends_with(".sock"))
        {
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

/// A container name (`qbittorrent`): what detection looks for on Docker's
/// network, and the compose service's name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContainerName(String);

impl ContainerName {
    pub fn parse(s: &str) -> Result<ContainerName, InputError> {
        check_name(s)?;
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
    /// The compose value: the network's name, or `service:gluetun`.
    pub fn compose_value(&self) -> &str {
        match self {
            NetworkMode::Network(n) => n.as_str(),
            NetworkMode::ServiceGluetun => "service:gluetun",
        }
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
            "/home/me/.config",
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
        assert!(ContainerName::parse("lodestar-prowlarr_1.x").is_ok());
        assert!(ContainerName::parse("9lives").is_ok());
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

    proptest! {
        #[test]
        fn accepted_values_hold_no_special_characters(s in "\\PC{0,80}") {
            if let Ok(p) = HostPath::parse(&s) {
                prop_assert!(!p.as_str().contains(SPECIAL));
                prop_assert!(!p.as_str().split('/').any(|c| c == ".."));
            }
            if let Ok(n) = ContainerName::parse(&s) {
                prop_assert!(!n.as_str().contains(SPECIAL));
            }
            if let Ok(n) = NetworkName::parse(&s) {
                prop_assert!(!n.as_str().contains(SPECIAL));
            }
            if let Ok(p) = PortNumber::parse(&s) {
                prop_assert!(p.get() >= PORT_MIN);
            }
            if let Ok(id) = Puid::parse(&s) {
                prop_assert!(id.get() != 0);
            }
        }

        #[test]
        fn any_path_with_a_special_character_is_refused(
            a in "[a-z]{1,8}", c in proptest::sample::select(SPECIAL), b in "[a-z]{0,8}"
        ) {
            let s = format!("/mnt/{a}{c}{b}");
            prop_assert!(HostPath::parse(&s).is_err());
        }
    }
}
