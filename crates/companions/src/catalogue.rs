//! The companion catalogue (architecture §18.10; plan
//! 2026-11-06-v06-companions §4.3, CMP-0).
//!
//! **Trust root (owner §2.46, cq1 (a)):** the catalogue is
//! `data/catalogue.json`, compiled into the binary with `include_str!`, so
//! the release signature (§12.2) covers it and it changes only with a
//! Lodestar release. A first install is an unsigned download (owner
//! §2.23), so its catalogue is as trustworthy as where Lodestar came from.
//!
//! **Untrusted structure all the same.** It's parsed strictly
//! (`deny_unknown_fields`, no duplicate keys, closed enums) and every value
//! is validated before anything can read it: `https` only, every download
//! host in both the entry's allowlist and [`DOWNLOAD_HOSTS`], images only
//! from [`REGISTRIES`], hash and size formats, versions, archive kinds that
//! match the URL, executable paths that stay inside the archive, ports,
//! and only the platforms v0.6 supports. A malformed catalogue fails this
//! crate's tests; at run time [`builtin`] returns the error, which disables
//! installs (and logs `companions.catalogue_invalid`). Nothing here panics.
//!
//! **Pins (owner §2.46, cq2):** the SHA-256 and size of each artifact, and
//! each image's digest, are the run-time gate. An artifact or image
//! without a pin is in the catalogue but can't be installed or rendered:
//! [`Entry::installable`] and [`Container::pinned_digest`] return `None`.
//! Pins come from CMP-13's pin-refresh tool, which also checks upstream
//! OpenPGP signatures when the pin is made and records it as
//! [`UpstreamSig::VerifiedAtPin`].
//!
//! **No other catalogue in a release.** The only constructor a shipped
//! build has is [`builtin`]. [`Catalogue::from_json`] exists only under
//! `cfg(test)` or the `testing` feature; nothing reads one from the
//! configuration or the environment.

use std::collections::HashSet;
use std::fmt;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::inputs::{ContainerName, PortNumber};
use crate::log_events::CATALOGUE_INVALID;

/// The only catalogue format this build reads.
pub const FORMAT: u32 = 1;

/// The hard cap on one artifact's size (plan Q16): 512 MiB.
pub const MAX_ARTIFACT_SIZE: u64 = 512 * 1024 * 1024;

/// The largest catalogue document accepted, in bytes.
pub const MAX_JSON: usize = 256 * 1024;

/// Every host a companion download (or one of its redirects) may come
/// from. An entry names the subset it uses; a host outside this list is
/// refused even if an entry names it, so adding one is a code change that
/// gets its own review.
pub const DOWNLOAD_HOSTS: &[&str] = &[
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

/// The registries a companion image may come from (owner §2.47, cq6:
/// linuxserver.io's `lscr.io`, the projects' own `ghcr.io` and Docker Hub).
pub const REGISTRIES: &[&str] = &["lscr.io", "ghcr.io", "docker.io"];

/// The platforms and architectures v0.6 installs natively on. An artifact
/// for any other pair is refused.
pub const SUPPORTED: &[(Platform, Arch)] = &[
    (Platform::Linux, Arch::X86_64),
    (Platform::Linux, Arch::Aarch64),
    (Platform::Windows, Arch::X86_64),
];

const MAX_ENTRIES: usize = 64;
const MAX_ARTIFACTS: usize = 8;
const MAX_HOSTS: usize = 8;
const MAX_PORTS: usize = 16;
const MAX_TEMPLATES: usize = 8;
const MAX_URL: usize = 2048;
const MAX_REL_PATH: usize = 255;
const MAX_REL_COMPONENTS: usize = 16;

const BUILTIN_JSON: &str = include_str!("../data/catalogue.json");

static BUILTIN: OnceLock<Result<Catalogue, CatalogueError>> = OnceLock::new();

/// The compiled-in catalogue, parsed and validated once.
///
/// An error means the release shipped a catalogue this build can't read.
/// The caller disables installs and shows why; Lodestar keeps running. The
/// first call that finds the error logs `companions.catalogue_invalid`.
pub fn builtin() -> Result<&'static Catalogue, &'static CatalogueError> {
    BUILTIN
        .get_or_init(|| {
            let parsed = parse(BUILTIN_JSON);
            if let Err(e) = &parsed {
                lodestar_log::log_event!(CATALOGUE_INVALID, rule = e.rule().code());
            }
            parsed
        })
        .as_ref()
}

// ─── Closed vocabularies ────────────────────────────────────────────────

/// An operating system a native artifact runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Linux,
    Windows,
}

/// A CPU architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Arch {
    X86_64,
    Aarch64,
}

/// How a native artifact is packaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveKind {
    /// A `.zip`; `executable` names the program inside it.
    Zip,
    /// A `.tar.gz` or `.tgz`; `executable` names the program inside it.
    TarGz,
    /// The executable itself (the userdocs static `qbittorrent-nox`).
    Binary,
    /// A Windows installer (`.exe` or `.msi`), offered for you to run
    /// rather than run by Lodestar (plan Q4).
    Installer,
}

/// What a companion is needed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Need {
    /// Downloading results (qBittorrent).
    DownloadClient,
    /// The Prowlarr source plugin.
    IndexerManager,
    /// Routing companion traffic through a VPN (gluetun).
    VpnRoute,
    /// Getting past sites' bot challenges (FlareSolverr): always opt-in
    /// (owner §2.47, Q21).
    ChallengeSolver,
}

/// A configuration template the code knows how to write. Closed: an id
/// the code has no writer for is refused when the catalogue is parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateId {
    /// qBittorrent's `qBittorrent.conf` (CMP-3).
    QbittorrentConf,
    /// Prowlarr's `config.xml` (CMP-3).
    ProwlarrConfigXml,
    /// The generated compose service (CMP-7).
    ComposeService,
    /// The filled Unraid template (CMP-7).
    UnraidTemplate,
}

impl TemplateId {
    /// A template for the companion's own config file, so it needs a
    /// native install.
    pub fn is_native_config(self) -> bool {
        matches!(
            self,
            TemplateId::QbittorrentConf | TemplateId::ProwlarrConfigXml
        )
    }
}

/// A port's protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Proto {
    Tcp,
    Udp,
}

/// Who may reach a port (§18.10: "bound to localhost or the internal
/// network; only the P2P ports they need are exposed").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Exposure {
    /// Bound to `127.0.0.1` natively, or to the internal network in a
    /// container. Never published.
    Internal,
    /// A peer-to-peer port that is published. Only download clients have
    /// one.
    P2p,
}

// ─── Validated values ───────────────────────────────────────────────────

/// A companion's id: `[a-z][a-z0-9-]{0,31}`, not ending in `-`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CompanionId(String);

impl CompanionId {
    pub fn parse(s: &str) -> Result<CompanionId, Rule> {
        let b = s.as_bytes();
        if b.is_empty() || b.len() > 32 {
            return Err(Rule::Id);
        }
        if !b[0].is_ascii_lowercase()
            || b[b.len() - 1] == b'-'
            || !b
                .iter()
                .all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        {
            return Err(Rule::Id);
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
    pub fn parse(s: &str) -> Result<Version, Rule> {
        if s.is_empty() || s.len() > 39 {
            return Err(Rule::Version);
        }
        let mut parts = [0u32; 4];
        for (n, p) in s.split('.').enumerate() {
            if n == 4
                || p.is_empty()
                || p.len() > 9
                || !p.bytes().all(|b| b.is_ascii_digit())
                || (p.len() > 1 && p.starts_with('0'))
            {
                return Err(Rule::Version);
            }
            parts[n] = p.parse().map_err(|_| Rule::Version)?;
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

/// A SHA-256 digest, written as 64 lower-case hex digits.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha256(pub [u8; 32]);

impl Sha256 {
    /// Parses exactly 64 lower-case hex digits. A digest made of one digit
    /// repeated (all zeros, all `f`) is refused as a placeholder.
    pub fn parse_hex(s: &str) -> Result<Sha256, Rule> {
        let b = s.as_bytes();
        if b.len() != 64 || !b.iter().all(|&c| matches!(c, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(Rule::Sha256);
        }
        if b.iter().all(|&c| c == b[0]) {
            return Err(Rule::Sha256);
        }
        let mut out = [0u8; 32];
        for (i, pair) in b.chunks(2).enumerate() {
            out[i] = (hex_val(pair[0]) << 4) | hex_val(pair[1]);
        }
        Ok(Sha256(out))
    }

    pub fn to_hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut s = String::with_capacity(64);
        for byte in self.0 {
            s.push(HEX[(byte >> 4) as usize] as char);
            s.push(HEX[(byte & 15) as usize] as char);
        }
        s
    }
}

fn hex_val(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => 0,
    }
}

impl fmt::Debug for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sha256({})", self.to_hex())
    }
}

impl fmt::Display for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// A DNS host name: lower-case letters, digits and `-` in labels of 1–63,
/// at least two labels, at most 253 bytes, and not an IP address. No port,
/// no user information.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hostname(String);

impl Hostname {
    pub fn parse(s: &str) -> Result<Hostname, Rule> {
        if s.is_empty() || s.len() > 253 {
            return Err(Rule::Host);
        }
        let labels: Vec<&str> = s.split('.').collect();
        if labels.len() < 2 {
            return Err(Rule::Host);
        }
        for l in &labels {
            let b = l.as_bytes();
            if b.is_empty()
                || b.len() > 63
                || b[0] == b'-'
                || b[b.len() - 1] == b'-'
                || !b
                    .iter()
                    .all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            {
                return Err(Rule::Host);
            }
        }
        // A last label of digits only is an IPv4 address, not a name.
        if labels[labels.len() - 1].bytes().all(|c| c.is_ascii_digit()) {
            return Err(Rule::Host);
        }
        Ok(Hostname(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Hostname {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An `https` URL with a validated [`Hostname`], no port, no user
/// information, no query and no fragment. The path starts with `/` and
/// holds only ASCII letters, digits and `-._~/+`, with no empty, `.` or
/// `..` segment (a trailing `/` is allowed). No percent-encoding, so a
/// URL means exactly what it says.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HttpsUrl {
    text: String,
    host_end: usize,
}

impl HttpsUrl {
    pub fn parse(s: &str) -> Result<HttpsUrl, Rule> {
        if s.len() > MAX_URL {
            return Err(Rule::Url);
        }
        let Some(rest) = s.strip_prefix("https://") else {
            return Err(Rule::Url);
        };
        let slash = rest.find('/').ok_or(Rule::Url)?;
        Hostname::parse(&rest[..slash]).map_err(|_| Rule::Url)?;
        let path = &rest[slash..];
        if !path.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/' | b'+')
        }) {
            return Err(Rule::Url);
        }
        let segments: Vec<&str> = path[1..].split('/').collect();
        let last = segments.len() - 1;
        for (i, seg) in segments.iter().enumerate() {
            if *seg == "." || *seg == ".." || (seg.is_empty() && i != last) {
                return Err(Rule::Url);
            }
        }
        Ok(HttpsUrl {
            text: s.to_owned(),
            host_end: "https://".len() + slash,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn host(&self) -> &str {
        &self.text["https://".len()..self.host_end]
    }

    /// The path, starting with `/`.
    pub fn path(&self) -> &str {
        &self.text[self.host_end..]
    }

    /// The last path segment (the file name of a download).
    pub fn file_name(&self) -> &str {
        let p = self.path();
        &p[p.rfind('/').map_or(0, |i| i + 1)..]
    }
}

impl fmt::Display for HttpsUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// A path inside an archive: `/`-separated components of ASCII letters,
/// digits, `.`, `_` and `-`; no `.` or `..` component, nothing absolute,
/// at most 16 components and 255 bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelPath(String);

impl RelPath {
    pub fn parse(s: &str) -> Result<RelPath, Rule> {
        if s.is_empty() || s.len() > MAX_REL_PATH {
            return Err(Rule::Executable);
        }
        let comps: Vec<&str> = s.split('/').collect();
        if comps.len() > MAX_REL_COMPONENTS {
            return Err(Rule::Executable);
        }
        for c in comps {
            if c.is_empty()
                || c == "."
                || c == ".."
                || !c
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            {
                return Err(Rule::Executable);
            }
        }
        Ok(RelPath(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The components, in order.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }
}

/// An OpenPGP key fingerprint: 40 (v4) or 64 (v5, v6) upper-case hex
/// digits, no spaces.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Fingerprint(String);

impl Fingerprint {
    pub fn parse(s: &str) -> Result<Fingerprint, Rule> {
        let b = s.as_bytes();
        if !(b.len() == 40 || b.len() == 64)
            || !b.iter().all(|&c| matches!(c, b'0'..=b'9' | b'A'..=b'F'))
        {
            return Err(Rule::Fingerprint);
        }
        Ok(Fingerprint(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A container image without a tag: a registry from [`REGISTRIES`] and one
/// to three path components (`lscr.io/linuxserver/qbittorrent`). It's
/// pinned by [`Container::digest`], never by tag.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageRef {
    text: String,
    registry_end: usize,
}

impl ImageRef {
    pub fn parse(s: &str) -> Result<ImageRef, Rule> {
        if s.is_empty() || s.len() > 255 {
            return Err(Rule::Image);
        }
        let comps: Vec<&str> = s.split('/').collect();
        if comps.len() < 2 || comps.len() > 4 || !REGISTRIES.contains(&comps[0]) {
            return Err(Rule::Image);
        }
        for c in &comps[1..] {
            if !image_component(c) {
                return Err(Rule::Image);
            }
        }
        Ok(ImageRef {
            text: s.to_owned(),
            registry_end: comps[0].len(),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn registry(&self) -> &str {
        &self.text[..self.registry_end]
    }

    /// The repository path after the registry (`linuxserver/qbittorrent`).
    pub fn repository(&self) -> &str {
        &self.text[self.registry_end + 1..]
    }
}

/// `[a-z0-9]+([._-][a-z0-9]+)*`, at most 64 bytes (the OCI distribution
/// spec's component rule with a length cap; `__` isn't allowed).
fn image_component(c: &str) -> bool {
    let b = c.as_bytes();
    if b.is_empty() || b.len() > 64 {
        return false;
    }
    let alnum = |x: u8| x.is_ascii_lowercase() || x.is_ascii_digit();
    let mut prev_sep = true;
    for &x in b {
        if alnum(x) {
            prev_sep = false;
        } else if matches!(x, b'.' | b'_' | b'-') && !prev_sep {
            prev_sep = true;
        } else {
            return false;
        }
    }
    !prev_sep
}

/// A short machine name (`webui`, `p2p_udp`): `[a-z][a-z0-9_]{0,31}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PortName(String);

impl PortName {
    pub fn parse(s: &str) -> Result<PortName, Rule> {
        let b = s.as_bytes();
        if b.is_empty()
            || b.len() > 32
            || !b[0].is_ascii_lowercase()
            || !b
                .iter()
                .all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
        {
            return Err(Rule::PortName);
        }
        Ok(PortName(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ─── The catalogue ──────────────────────────────────────────────────────

/// The parsed, validated catalogue.
#[derive(Debug, Clone)]
pub struct Catalogue {
    entries: Vec<Entry>,
}

impl Catalogue {
    /// Parses and validates a catalogue other than the compiled-in one.
    /// Tests and the `testing` feature only: a shipped build has no way to
    /// load another catalogue.
    #[cfg(any(test, feature = "testing"))]
    pub fn from_json(json: &str) -> Result<Catalogue, CatalogueError> {
        parse(json)
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id.as_str() == id)
    }
}

/// One companion.
#[derive(Debug, Clone)]
pub struct Entry {
    pub id: CompanionId,
    /// Its display name (`qBittorrent`).
    pub name: String,
    pub licence: Licence,
    pub homepage: HttpsUrl,
    pub needed_for: Vec<Need>,
    /// Off and not installed unless you switch it on (FlareSolverr, Q21).
    pub opt_in: bool,
    /// The hosts its downloads and their redirects may use: a subset of
    /// [`DOWNLOAD_HOSTS`]. Empty when it has no native artifacts.
    pub hosts: Vec<Hostname>,
    pub native: Option<Native>,
    pub container: Option<Container>,
    pub ports: Vec<Port>,
    pub health: Option<HealthCheck>,
    pub templates: Vec<TemplateId>,
}

impl Entry {
    /// The artifact for a platform, pinned or not.
    pub fn artifact(&self, platform: Platform, arch: Arch) -> Option<&Artifact> {
        self.native
            .as_ref()?
            .artifacts
            .iter()
            .find(|a| a.platform == platform && a.arch == arch)
    }

    /// The artifact and its pin, only when it's pinned: the one thing an
    /// install may use.
    pub fn installable(&self, platform: Platform, arch: Arch) -> Option<(&Artifact, &Pin)> {
        let a = self.artifact(platform, arch)?;
        Some((a, a.pin.as_ref()?))
    }

    /// Whether `host` may serve this entry's downloads or redirects.
    pub fn allows_host(&self, host: &str) -> bool {
        self.hosts.iter().any(|h| h.as_str() == host)
    }

    pub fn port(&self, name: &str) -> Option<&Port> {
        self.ports.iter().find(|p| p.name.as_str() == name)
    }

    pub fn needs(&self, need: Need) -> bool {
        self.needed_for.contains(&need)
    }
}

/// A licence: its SPDX expression and where to read it.
#[derive(Debug, Clone)]
pub struct Licence {
    pub spdx: String,
    pub url: HttpsUrl,
}

/// The native builds.
#[derive(Debug, Clone)]
pub struct Native {
    pub artifacts: Vec<Artifact>,
}

/// One platform's build.
#[derive(Debug, Clone)]
pub struct Artifact {
    pub platform: Platform,
    pub arch: Arch,
    pub archive: ArchiveKind,
    /// The program inside a zip or tar.gz. `None` for a bare binary or an
    /// installer.
    pub executable: Option<RelPath>,
    /// `None` until CMP-13's tool pins it; an unpinned artifact is never
    /// installed.
    pub pin: Option<Pin>,
}

/// What a download must match, byte for byte.
#[derive(Debug, Clone)]
pub struct Pin {
    pub version: Version,
    /// On one of the entry's hosts.
    pub url: HttpsUrl,
    /// Exact size in bytes, at most [`MAX_ARTIFACT_SIZE`].
    pub size: u64,
    pub sha256: Sha256,
    pub upstream_sig: UpstreamSig,
}

/// What was checked of upstream's own signature when the pin was made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamSig {
    /// Upstream doesn't sign this artifact (Prowlarr, the userdocs
    /// builds). The SHA-256 pin is the only gate.
    None,
    /// CMP-13 checked upstream's OpenPGP signature against this pinned key
    /// before writing the pin.
    VerifiedAtPin { fingerprint: Fingerprint },
}

/// The container image for compose and Unraid.
#[derive(Debug, Clone)]
pub struct Container {
    pub image: ImageRef,
    /// `None` until CMP-13 pins it. Nothing is generated from an unpinned
    /// image.
    pub digest: Option<Sha256>,
    /// The container and compose service name detection looks for.
    pub name: ContainerName,
}

impl Container {
    /// The digest, only when pinned.
    pub fn pinned_digest(&self) -> Option<&Sha256> {
        self.digest.as_ref()
    }
}

/// A port the companion listens on.
#[derive(Debug, Clone)]
pub struct Port {
    pub name: PortName,
    pub number: PortNumber,
    pub proto: Proto,
    pub exposure: Exposure,
}

/// Where its health is read: an internal TCP port and an HTTP path.
#[derive(Debug, Clone)]
pub struct HealthCheck {
    pub port: PortName,
    /// Starts with `/`; letters, digits and `/._-` only.
    pub path: String,
}

// ─── Errors ─────────────────────────────────────────────────────────────

/// Which rule a catalogue broke. Each has a stable code for the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Rule {
    TooLarge,
    Syntax,
    Format,
    Count,
    Duplicate,
    Id,
    Name,
    Licence,
    Url,
    Host,
    HostNotAllowed,
    Platform,
    Archive,
    Executable,
    Version,
    Size,
    Sha256,
    Fingerprint,
    Image,
    PortName,
    Port,
    Exposure,
    Health,
    Template,
    Need,
    Shape,
}

impl Rule {
    /// The stable code, for `companions.catalogue_invalid`'s `rule` field.
    pub fn code(self) -> &'static str {
        match self {
            Rule::TooLarge => "too_large",
            Rule::Syntax => "syntax",
            Rule::Format => "format",
            Rule::Count => "count",
            Rule::Duplicate => "duplicate",
            Rule::Id => "id",
            Rule::Name => "name",
            Rule::Licence => "licence",
            Rule::Url => "url",
            Rule::Host => "host",
            Rule::HostNotAllowed => "host_not_allowed",
            Rule::Platform => "platform",
            Rule::Archive => "archive",
            Rule::Executable => "executable",
            Rule::Version => "version",
            Rule::Size => "size",
            Rule::Sha256 => "sha256",
            Rule::Fingerprint => "fingerprint",
            Rule::Image => "image",
            Rule::PortName => "port_name",
            Rule::Port => "port",
            Rule::Exposure => "exposure",
            Rule::Health => "health",
            Rule::Template => "template",
            Rule::Need => "need",
            Rule::Shape => "shape",
        }
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// Why a catalogue was refused: the rule and where (`entries[1].native.
/// artifacts[0].pin.url`, or a line and column for a syntax error). The
/// location is built from the code's own field names and indexes, never
/// from catalogue text, so it's safe to show and log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogueError {
    rule: Rule,
    at: String,
}

impl CatalogueError {
    fn new(rule: Rule, at: impl Into<String>) -> CatalogueError {
        CatalogueError {
            rule,
            at: at.into(),
        }
    }

    pub fn rule(&self) -> Rule {
        self.rule
    }

    pub fn at(&self) -> &str {
        &self.at
    }
}

impl fmt::Display for CatalogueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "companion catalogue: {} at {}", self.rule, self.at)
    }
}

impl std::error::Error for CatalogueError {}

// ─── Parsing ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCatalogue {
    format: u32,
    entries: Vec<RawEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    id: String,
    name: String,
    licence: RawLicence,
    homepage: String,
    needed_for: Vec<Need>,
    opt_in: bool,
    hosts: Vec<String>,
    native: Option<RawNative>,
    container: Option<RawContainer>,
    ports: Vec<RawPort>,
    health: Option<RawHealth>,
    templates: Vec<TemplateId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLicence {
    spdx: String,
    url: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNative {
    artifacts: Vec<RawArtifact>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawArtifact {
    platform: Platform,
    arch: Arch,
    archive: ArchiveKind,
    executable: Option<String>,
    pin: Option<RawPin>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPin {
    version: String,
    url: String,
    size: u64,
    sha256: String,
    upstream_sig: RawSig,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSig {
    kind: RawSigKind,
    fingerprint: Option<String>,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum RawSigKind {
    None,
    VerifiedAtPin,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawContainer {
    image: String,
    digest: Option<String>,
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPort {
    name: String,
    port: u16,
    proto: Proto,
    exposure: Exposure,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHealth {
    port: String,
    path: String,
}

fn parse(json: &str) -> Result<Catalogue, CatalogueError> {
    if json.len() > MAX_JSON {
        return Err(CatalogueError::new(Rule::TooLarge, "document"));
    }
    let raw: RawCatalogue = serde_json::from_str(json).map_err(|e| {
        CatalogueError::new(
            Rule::Syntax,
            format!("line {}, column {}", e.line(), e.column()),
        )
    })?;
    validate(raw)
}

/// Fails with `rule` at `at` unless `ok`.
fn check(ok: bool, rule: Rule, at: &str) -> Result<(), CatalogueError> {
    if ok {
        Ok(())
    } else {
        Err(CatalogueError::new(rule, at))
    }
}

fn field<T>(r: Result<T, Rule>, at: &str) -> Result<T, CatalogueError> {
    r.map_err(|rule| CatalogueError::new(rule, at))
}

fn unique<T: Eq + std::hash::Hash>(items: impl IntoIterator<Item = T>) -> bool {
    let mut seen = HashSet::new();
    items.into_iter().all(|x| seen.insert(x))
}

fn validate(raw: RawCatalogue) -> Result<Catalogue, CatalogueError> {
    check(raw.format == FORMAT, Rule::Format, "format")?;
    check(
        !raw.entries.is_empty() && raw.entries.len() <= MAX_ENTRIES,
        Rule::Count,
        "entries",
    )?;
    let mut entries = Vec::with_capacity(raw.entries.len());
    for (i, e) in raw.entries.into_iter().enumerate() {
        entries.push(validate_entry(e, &format!("entries[{i}]"))?);
    }
    check(
        unique(entries.iter().map(|e| e.id.as_str())),
        Rule::Duplicate,
        "entries[].id",
    )?;
    check(
        unique(
            entries
                .iter()
                .filter_map(|e| e.container.as_ref())
                .map(|c| c.name.as_str()),
        ),
        Rule::Duplicate,
        "entries[].container.name",
    )?;
    Ok(Catalogue { entries })
}

fn validate_entry(e: RawEntry, at: &str) -> Result<Entry, CatalogueError> {
    let id = field(CompanionId::parse(&e.id), &format!("{at}.id"))?;
    check(display_name(&e.name), Rule::Name, &format!("{at}.name"))?;
    check(
        spdx(&e.licence.spdx),
        Rule::Licence,
        &format!("{at}.licence.spdx"),
    )?;
    let licence = Licence {
        spdx: e.licence.spdx,
        url: field(
            HttpsUrl::parse(&e.licence.url),
            &format!("{at}.licence.url"),
        )?,
    };
    let homepage = field(HttpsUrl::parse(&e.homepage), &format!("{at}.homepage"))?;

    let at_need = format!("{at}.needed_for");
    check(
        !e.needed_for.is_empty() && e.needed_for.len() <= 4,
        Rule::Count,
        &at_need,
    )?;
    check(unique(e.needed_for.iter()), Rule::Duplicate, &at_need)?;
    // FlareSolverr and anything like it is opt-in only (owner §2.47, Q21).
    check(
        !e.needed_for.contains(&Need::ChallengeSolver) || e.opt_in,
        Rule::Need,
        &format!("{at}.opt_in"),
    )?;

    let at_hosts = format!("{at}.hosts");
    check(e.hosts.len() <= MAX_HOSTS, Rule::Count, &at_hosts)?;
    let mut hosts = Vec::with_capacity(e.hosts.len());
    for (j, h) in e.hosts.iter().enumerate() {
        let at_h = format!("{at_hosts}[{j}]");
        let h = field(Hostname::parse(h), &at_h)?;
        check(
            DOWNLOAD_HOSTS.contains(&h.as_str()),
            Rule::HostNotAllowed,
            &at_h,
        )?;
        hosts.push(h);
    }
    check(unique(hosts.iter()), Rule::Duplicate, &at_hosts)?;

    let native = match e.native {
        Some(n) => Some(validate_native(n, &hosts, &format!("{at}.native"))?),
        None => None,
    };
    // Hosts exist for downloads only: an entry without native builds names
    // none, and one with them names at least one.
    check(native.is_some() != hosts.is_empty(), Rule::Shape, &at_hosts)?;
    let container = match e.container {
        Some(c) => Some(validate_container(c, &format!("{at}.container"))?),
        None => None,
    };
    check(native.is_some() || container.is_some(), Rule::Shape, at)?;
    // The VPN route is gluetun in a container; the native VPN is v1.x
    // (plan §2).
    check(
        !e.needed_for.contains(&Need::VpnRoute) || (native.is_none() && container.is_some()),
        Rule::Shape,
        &format!("{at}.native"),
    )?;

    let at_ports = format!("{at}.ports");
    check(
        !e.ports.is_empty() && e.ports.len() <= MAX_PORTS,
        Rule::Count,
        &at_ports,
    )?;
    let mut ports = Vec::with_capacity(e.ports.len());
    for (j, p) in e.ports.into_iter().enumerate() {
        let at_p = format!("{at_ports}[{j}]");
        let port = Port {
            name: field(PortName::parse(&p.name), &format!("{at_p}.name"))?,
            number: PortNumber::new(p.port)
                .map_err(|_| CatalogueError::new(Rule::Port, format!("{at_p}.port")))?,
            proto: p.proto,
            exposure: p.exposure,
        };
        // Only a download client publishes a port, and only for peers.
        check(
            port.exposure == Exposure::Internal || e.needed_for.contains(&Need::DownloadClient),
            Rule::Exposure,
            &format!("{at_p}.exposure"),
        )?;
        ports.push(port);
    }
    check(
        unique(ports.iter().map(|p| p.name.as_str())),
        Rule::Duplicate,
        &at_ports,
    )?;
    check(
        unique(ports.iter().map(|p| (p.number, p.proto))),
        Rule::Duplicate,
        &at_ports,
    )?;

    let health = match e.health {
        Some(h) => {
            let at_hc = format!("{at}.health");
            let port = field(PortName::parse(&h.port), &format!("{at_hc}.port"))?;
            let target = ports.iter().find(|p| p.name == port);
            check(
                target.is_some_and(|p| p.proto == Proto::Tcp && p.exposure == Exposure::Internal),
                Rule::Health,
                &format!("{at_hc}.port"),
            )?;
            check(health_path(&h.path), Rule::Health, &format!("{at_hc}.path"))?;
            Some(HealthCheck { port, path: h.path })
        }
        None => None,
    };

    let at_t = format!("{at}.templates");
    check(e.templates.len() <= MAX_TEMPLATES, Rule::Count, &at_t)?;
    check(unique(e.templates.iter()), Rule::Duplicate, &at_t)?;
    for (j, t) in e.templates.iter().enumerate() {
        let ok = if t.is_native_config() {
            native.is_some()
        } else {
            container.is_some()
        };
        check(ok, Rule::Template, &format!("{at_t}[{j}]"))?;
    }

    Ok(Entry {
        id,
        name: e.name,
        licence,
        homepage,
        needed_for: e.needed_for,
        opt_in: e.opt_in,
        hosts,
        native,
        container,
        ports,
        health,
        templates: e.templates,
    })
}

fn validate_native(n: RawNative, hosts: &[Hostname], at: &str) -> Result<Native, CatalogueError> {
    let at_a = format!("{at}.artifacts");
    check(
        !n.artifacts.is_empty() && n.artifacts.len() <= MAX_ARTIFACTS,
        Rule::Count,
        &at_a,
    )?;
    let mut artifacts = Vec::with_capacity(n.artifacts.len());
    for (j, a) in n.artifacts.into_iter().enumerate() {
        artifacts.push(validate_artifact(a, hosts, &format!("{at_a}[{j}]"))?);
    }
    check(
        unique(artifacts.iter().map(|a| (a.platform, a.arch))),
        Rule::Duplicate,
        &at_a,
    )?;
    // One release per entry: every pin names the same version.
    let mut versions = artifacts
        .iter()
        .filter_map(|a| a.pin.as_ref())
        .map(|p| &p.version);
    if let Some(first) = versions.next() {
        check(versions.all(|v| v == first), Rule::Version, &at_a)?;
    }
    Ok(Native { artifacts })
}

fn validate_artifact(
    a: RawArtifact,
    hosts: &[Hostname],
    at: &str,
) -> Result<Artifact, CatalogueError> {
    check(
        SUPPORTED.contains(&(a.platform, a.arch)),
        Rule::Platform,
        at,
    )?;
    let at_x = format!("{at}.executable");
    let executable = match (a.archive, a.executable) {
        (ArchiveKind::Zip | ArchiveKind::TarGz, Some(x)) => {
            let x = field(RelPath::parse(&x), &at_x)?;
            let exe = x.as_str().ends_with(".exe");
            check(
                exe == (a.platform == Platform::Windows),
                Rule::Executable,
                &at_x,
            )?;
            Some(x)
        }
        (ArchiveKind::Binary | ArchiveKind::Installer, None) => None,
        _ => return Err(CatalogueError::new(Rule::Executable, at_x)),
    };
    check(
        a.archive != ArchiveKind::Installer || a.platform == Platform::Windows,
        Rule::Archive,
        &format!("{at}.archive"),
    )?;
    let pin = match a.pin {
        Some(p) => Some(validate_pin(
            p,
            a.platform,
            a.archive,
            hosts,
            &format!("{at}.pin"),
        )?),
        None => None,
    };
    Ok(Artifact {
        platform: a.platform,
        arch: a.arch,
        archive: a.archive,
        executable,
        pin,
    })
}

fn validate_pin(
    p: RawPin,
    platform: Platform,
    archive: ArchiveKind,
    hosts: &[Hostname],
    at: &str,
) -> Result<Pin, CatalogueError> {
    let version = field(Version::parse(&p.version), &format!("{at}.version"))?;
    let at_u = format!("{at}.url");
    let url = field(HttpsUrl::parse(&p.url), &at_u)?;
    check(
        hosts.iter().any(|h| h.as_str() == url.host()),
        Rule::HostNotAllowed,
        &at_u,
    )?;
    let file = url.file_name();
    let ext_ok = match archive {
        ArchiveKind::Zip => file.ends_with(".zip"),
        ArchiveKind::TarGz => file.ends_with(".tar.gz") || file.ends_with(".tgz"),
        ArchiveKind::Binary => {
            !file.is_empty()
                && (file.ends_with(".exe") == (platform == Platform::Windows))
                && !file.ends_with(".zip")
                && !file.ends_with(".gz")
                && !file.ends_with(".msi")
        }
        ArchiveKind::Installer => file.ends_with(".exe") || file.ends_with(".msi"),
    };
    // A file name of only an extension (`/.zip`) isn't a file name.
    check(ext_ok && !file.starts_with('.'), Rule::Archive, &at_u)?;
    check(
        p.size > 0 && p.size <= MAX_ARTIFACT_SIZE,
        Rule::Size,
        &format!("{at}.size"),
    )?;
    let sha256 = field(Sha256::parse_hex(&p.sha256), &format!("{at}.sha256"))?;
    let at_s = format!("{at}.upstream_sig");
    let upstream_sig = match (p.upstream_sig.kind, p.upstream_sig.fingerprint) {
        (RawSigKind::None, None) => UpstreamSig::None,
        (RawSigKind::VerifiedAtPin, Some(f)) => UpstreamSig::VerifiedAtPin {
            fingerprint: field(Fingerprint::parse(&f), &format!("{at_s}.fingerprint"))?,
        },
        _ => return Err(CatalogueError::new(Rule::Fingerprint, at_s)),
    };
    Ok(Pin {
        version,
        url,
        size: p.size,
        sha256,
        upstream_sig,
    })
}

fn validate_container(c: RawContainer, at: &str) -> Result<Container, CatalogueError> {
    let image = field(ImageRef::parse(&c.image), &format!("{at}.image"))?;
    let at_d = format!("{at}.digest");
    let digest = match c.digest {
        Some(d) => {
            let hex = d
                .strip_prefix("sha256:")
                .ok_or_else(|| CatalogueError::new(Rule::Sha256, at_d.clone()))?;
            Some(field(Sha256::parse_hex(hex), &at_d)?)
        }
        None => None,
    };
    let name = ContainerName::parse(&c.name)
        .map_err(|_| CatalogueError::new(Rule::Name, format!("{at}.name")))?;
    Ok(Container {
        image,
        digest,
        name,
    })
}

/// 1–64 bytes of ASCII letters, digits, space and `.+()-`, with no leading
/// or trailing space.
fn display_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with(' ')
        && !s.ends_with(' ')
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b' ' | b'.' | b'+' | b'(' | b')' | b'-')
        })
}

/// An SPDX expression of licence ids joined by single spaces and `AND`,
/// `OR` or `WITH`; at most 128 bytes. No parentheses.
fn spdx(s: &str) -> bool {
    if s.is_empty() || s.len() > 128 {
        return false;
    }
    s.split(' ').enumerate().all(|(i, t)| {
        if i % 2 == 1 {
            matches!(t, "AND" | "OR" | "WITH")
        } else {
            !t.is_empty()
                && !matches!(t, "AND" | "OR" | "WITH")
                && t.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
        }
    }) && s.split(' ').count() % 2 == 1
}

/// `/` then letters, digits and `/._-`; no empty, `.` or `..` segment
/// except a trailing `/`; at most 128 bytes.
fn health_path(s: &str) -> bool {
    if s.is_empty() || s.len() > 128 || !s.starts_with('/') {
        return false;
    }
    if !s
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-'))
    {
        return false;
    }
    let segs: Vec<&str> = s[1..].split('/').collect();
    let last = segs.len() - 1;
    segs.iter()
        .enumerate()
        .all(|(i, g)| *g != "." && *g != ".." && (!g.is_empty() || i == last))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_parses() {
        let c = builtin().expect("data/catalogue.json must validate");
        assert!(c.get("qbittorrent").is_some());
        // Parsed once: the same catalogue every time.
        assert!(std::ptr::eq(c, builtin().unwrap()));
    }

    #[test]
    fn ids() {
        for ok in ["qbittorrent", "a", "flare-solverr", "x9"] {
            assert!(CompanionId::parse(ok).is_ok(), "{ok}");
        }
        for bad in ["", "Qbit", "9x", "-x", "x-", "a_b", "a.b", &"a".repeat(33)] {
            assert_eq!(CompanionId::parse(bad), Err(Rule::Id), "{bad}");
        }
    }

    #[test]
    fn versions() {
        let v = |s| Version::parse(s).unwrap();
        assert!(v("5.0.3") < v("5.0.10"));
        assert!(v("1.28.2.4885") > v("1.28.2"));
        assert_eq!(v("5.0"), v("5.0.0"));
        assert_eq!(v("1.28.2.4885").as_str(), "1.28.2.4885");
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
        ] {
            assert_eq!(Version::parse(bad), Err(Rule::Version), "{bad}");
        }
        assert!(Version::parse("0.0.0").is_ok());
        assert!(Version::parse("999999999").is_ok());
    }

    #[test]
    fn digests() {
        let hex = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let d = Sha256::parse_hex(hex).unwrap();
        assert_eq!(d.to_hex(), hex);
        assert_eq!(d.0[0], 0xe3);
        assert_eq!(d.0[31], 0x55);
        assert_eq!(format!("{d:?}"), format!("Sha256({hex})"));
        for bad in [
            &hex[..63],
            &format!("{hex}0"),
            &hex.to_uppercase(),
            &"0".repeat(64),
            &"f".repeat(64),
            &format!("g{}", &hex[1..]),
            &format!("sha256:{hex}"),
        ] {
            assert_eq!(Sha256::parse_hex(bad), Err(Rule::Sha256), "{bad}");
        }
    }

    #[test]
    fn hosts() {
        for ok in [
            "github.com",
            "release-assets.githubusercontent.com",
            "a.b",
            "x1.io",
        ] {
            assert!(Hostname::parse(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "localhost",
            "GitHub.com",
            "github.com.",
            ".github.com",
            "-a.com",
            "a-.com",
            "a..com",
            "127.0.0.1",
            "1.2.3.4",
            "github.com:443",
            "user@github.com",
            "[::1]",
            "xn--e1a.com:1",
            "a_b.com",
        ] {
            assert_eq!(Hostname::parse(bad), Err(Rule::Host), "{bad}");
        }
        assert!(Hostname::parse(&format!("{}.com", "a".repeat(64))).is_err());
    }

    #[test]
    fn urls() {
        let u =
            HttpsUrl::parse("https://github.com/a/b/releases/download/v1.2/x_1.tar.gz").unwrap();
        assert_eq!(u.host(), "github.com");
        assert_eq!(u.path(), "/a/b/releases/download/v1.2/x_1.tar.gz");
        assert_eq!(u.file_name(), "x_1.tar.gz");
        assert!(HttpsUrl::parse("https://www.qbittorrent.org/").is_ok());
        for bad in [
            "http://github.com/x",
            "https://github.com",
            "https://github.com:443/x",
            "https://user@github.com/x",
            "https://github.com/x?y=1",
            "https://github.com/x#y",
            "https://github.com/a/../b",
            "https://github.com/a/./b",
            "https://github.com//b",
            "https://github.com/a%2e%2e/b",
            "https://github.com/a b",
            "https://github.com/a\\b",
            "https://127.0.0.1/x",
            "HTTPS://github.com/x",
            " https://github.com/x",
            "https://github.com/x\n",
        ] {
            assert_eq!(HttpsUrl::parse(bad), Err(Rule::Url), "{bad}");
        }
        let long = format!("https://github.com/{}", "a".repeat(MAX_URL));
        assert_eq!(HttpsUrl::parse(&long), Err(Rule::Url));
    }

    #[test]
    fn rel_paths() {
        assert!(RelPath::parse("Prowlarr/Prowlarr").is_ok());
        assert!(RelPath::parse("Prowlarr/Prowlarr.exe").is_ok());
        assert_eq!(
            RelPath::parse("a/b")
                .unwrap()
                .components()
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        for bad in [
            "",
            "/abs",
            "a/../b",
            "..",
            ".",
            "a//b",
            "a/",
            "a\\b",
            "C:/x",
            "a b",
            &"a/".repeat(16).to_string(),
        ] {
            assert_eq!(RelPath::parse(bad), Err(Rule::Executable), "{bad:?}");
        }
    }

    #[test]
    fn images() {
        let i = ImageRef::parse("lscr.io/linuxserver/qbittorrent").unwrap();
        assert_eq!(i.registry(), "lscr.io");
        assert_eq!(i.repository(), "linuxserver/qbittorrent");
        assert!(ImageRef::parse("docker.io/qmcgaw/gluetun").is_ok());
        assert!(ImageRef::parse("ghcr.io/flaresolverr/flaresolverr").is_ok());
        for bad in [
            "qbittorrent",
            "linuxserver/qbittorrent",
            "evil.example/linuxserver/qbittorrent",
            "lscr.io/linuxserver/qbittorrent:latest",
            "lscr.io/linuxserver/qbittorrent@sha256:00",
            "lscr.io/LinuxServer/qbittorrent",
            "lscr.io/a/b/c/d",
            "lscr.io/a--b",
            "lscr.io/-a",
            "lscr.io/a.",
            "lscr.io/a__b",
            "lscr.io//a",
            "lscr.io",
        ] {
            assert_eq!(ImageRef::parse(bad), Err(Rule::Image), "{bad}");
        }
        assert!(ImageRef::parse("lscr.io/a_b/c.d-e").is_ok());
    }

    #[test]
    fn small_grammars() {
        assert!(display_name("qBittorrent"));
        assert!(display_name("FlareSolverr (opt-in)"));
        assert!(!display_name(""));
        assert!(!display_name(" x"));
        assert!(!display_name("<b>"));
        assert!(!display_name(&"a".repeat(65)));
        assert!(spdx("GPL-2.0-or-later"));
        assert!(spdx("MIT OR Apache-2.0"));
        assert!(spdx("GPL-2.0-or-later WITH openvpn-openssl-exception"));
        for bad in [
            "",
            "MIT OR",
            "OR MIT",
            "MIT  OR Apache-2.0",
            "(MIT)",
            "MIT OR AND X",
            "MIT;",
        ] {
            assert!(!spdx(bad), "{bad}");
        }
        assert!(health_path("/ping"));
        assert!(health_path("/api/v2/app/version"));
        assert!(health_path("/"));
        for bad in ["", "ping", "/a/../b", "//", "/a?b", "/a b", "/a//b"] {
            assert!(!health_path(bad), "{bad}");
        }
        assert!(Fingerprint::parse(&"A".repeat(40)).is_ok());
        assert!(Fingerprint::parse(&"0".repeat(64)).is_ok());
        assert!(Fingerprint::parse(&"a".repeat(40)).is_err());
        assert!(Fingerprint::parse(&"A".repeat(41)).is_err());
        assert!(PortName::parse("p2p_udp").is_ok());
        assert!(PortName::parse("P2p").is_err());
    }

    #[test]
    fn rule_codes_are_distinct() {
        let all = [
            Rule::TooLarge,
            Rule::Syntax,
            Rule::Format,
            Rule::Count,
            Rule::Duplicate,
            Rule::Id,
            Rule::Name,
            Rule::Licence,
            Rule::Url,
            Rule::Host,
            Rule::HostNotAllowed,
            Rule::Platform,
            Rule::Archive,
            Rule::Executable,
            Rule::Version,
            Rule::Size,
            Rule::Sha256,
            Rule::Fingerprint,
            Rule::Image,
            Rule::PortName,
            Rule::Port,
            Rule::Exposure,
            Rule::Health,
            Rule::Template,
            Rule::Need,
            Rule::Shape,
        ];
        assert!(unique(all.iter().map(|r| r.code())));
    }
}
