//! A companion's `docker compose` service, and the checked inputs both it
//! and the Unraid template ([`crate::unraid`]) are built from (plan
//! 2026-11-06-v06-companions §4.3 CMP-7, architecture §18.10).
//!
//! The output is applied by a root-equivalent `docker compose`, so:
//! - **Only closed values reach it:** catalogue values (the image pinned by
//!   digest, the container name, the port numbers), CMP-0's input
//!   newtypes ([`HostPath`], [`Puid`], [`Pgid`], [`NetworkName`],
//!   [`PortNumber`]) and fixed text in this file. `TZ` is fixed at
//!   `Etc/UTC` and `UMASK` at `022`.
//! - **Every YAML key and scalar is a JSON string** written by
//!   `serde_json` (JSON is valid YAML), so no value can end its string, add
//!   a key, an anchor, an alias or a second document. A scalar holding `$`
//!   is refused as well, because compose interpolates inside quotes.
//!   Volumes and ports use compose's long syntax, so no value is joined to
//!   another with `:`.
//! - **What it never writes:** a Docker socket mount, `privileged`,
//!   `pid: host`, `network_mode: host`, or any capability except gluetun's
//!   `NET_ADMIN` and its `/dev/net/tun` device, which gluetun's upstream
//!   setup requires.
//! - **Ports stay on the internal network** unless you ask to publish one,
//!   on `127.0.0.1` ([`Bind::Loopback`]) or on every address
//!   ([`Bind::Lan`]).
//! - **gluetun's VPN settings are placeholders** you fill in yourself.
//!   Lodestar never asks for, stores or writes a VPN key.

use std::fmt;
use std::path::Path;

use serde_json::Value;

use crate::catalogue::{Catalogue, Entry, Exposure, NotAvailable, PortName, Proto, TemplateId};
use crate::inputs::{
    CompanionId, ContainerName, HostPath, InputError, NetworkMode, NetworkName, Pgid, PortNumber,
    Puid,
};

/// `TZ` in every generated service: log times are UTC, like Lodestar's own.
pub const TZ: &str = "Etc/UTC";
/// `UMASK` for linuxserver.io images: files `644`, folders `755`.
pub const UMASK: &str = "022";
/// The value every gluetun VPN setting is written with. gluetun refuses to
/// start with it, and a companion routed through gluetun can't start
/// without gluetun, so nothing leaves before you fill them in.
pub const PLACEHOLDER: &str = "FILL_IN_YOURSELF";
/// gluetun's catalogue id.
pub const GLUETUN: &str = "gluetun";

/// Why a template wasn't generated. Carries no input value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TemplateError {
    /// No catalogue entry has this id.
    UnknownCompanion,
    /// The entry has no image, or it isn't pinned yet.
    NotAvailable(NotAvailable),
    /// The entry doesn't list this template, or the code has no profile
    /// for it.
    NoTemplate,
    /// The companion needs this folder and none was given.
    MissingPath(MountRole),
    /// A folder was given that the companion doesn't use.
    UnexpectedPath(MountRole),
    /// A folder is, holds or sits inside Lodestar's own data folder.
    DataFolder,
    /// A folder sits inside another companion's `appdata/<name>` folder.
    OtherCompanion,
    /// A folder has a component starting with `.` (`~/.ssh`, `.config`).
    DotFolder,
    /// A Lodestar data folder that isn't an absolute, clean path.
    BadDataFolder(InputError),
    /// A published port the companion doesn't have.
    UnknownPort,
    /// The same port, or the same host port and protocol, twice.
    DuplicatePort,
    /// gluetun can't be routed through itself.
    GluetunSelf,
    /// A peer-to-peer port published while routed through gluetun.
    P2pThroughVpn,
    /// A value held `$`, which compose would interpolate.
    Interpolation,
    /// The XML writer failed.
    Encode,
}

impl TemplateError {
    /// A stable code for logs and the API.
    pub fn code(self) -> &'static str {
        match self {
            TemplateError::UnknownCompanion => "unknown_companion",
            TemplateError::NotAvailable(NotAvailable::AwaitingPin) => "awaiting_pin",
            TemplateError::NotAvailable(_) => "no_image",
            TemplateError::NoTemplate => "no_template",
            TemplateError::MissingPath(_) => "missing_path",
            TemplateError::UnexpectedPath(_) => "unexpected_path",
            TemplateError::DataFolder => "inside_data_folder",
            TemplateError::OtherCompanion => "inside_other_companion",
            TemplateError::DotFolder => "dot_folder",
            TemplateError::BadDataFolder(_) => "bad_data_folder",
            TemplateError::UnknownPort => "unknown_port",
            TemplateError::DuplicatePort => "duplicate_port",
            TemplateError::GluetunSelf => "gluetun_self",
            TemplateError::P2pThroughVpn => "p2p_through_vpn",
            TemplateError::Interpolation => "interpolation",
            TemplateError::Encode => "encode",
        }
    }
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TemplateError::NotAvailable(n) => write!(f, "can't generate it: {n}"),
            TemplateError::BadDataFolder(e) => write!(f, "the data folder {e}"),
            TemplateError::MissingPath(r) => write!(f, "the {} folder is needed", r.name()),
            TemplateError::UnexpectedPath(r) => {
                write!(f, "this companion has no {} folder", r.name())
            }
            other => f.write_str(match other {
                TemplateError::UnknownCompanion => "no companion has this id",
                TemplateError::NoTemplate => "this companion has no such template",
                TemplateError::DataFolder => "a folder overlaps Lodestar's data folder",
                TemplateError::OtherCompanion => "a folder is inside another companion's folder",
                TemplateError::DotFolder => "a folder name starts with a dot",
                TemplateError::UnknownPort => "the companion has no such port",
                TemplateError::DuplicatePort => "a port is published twice",
                TemplateError::GluetunSelf => "gluetun can't be routed through itself",
                TemplateError::P2pThroughVpn => {
                    "a peer port can't be published through the VPN; forward it in gluetun yourself"
                }
                TemplateError::Interpolation => "a value holds `$`",
                _ => "the template couldn't be written",
            }),
        }
    }
}

impl std::error::Error for TemplateError {}

impl From<NotAvailable> for TemplateError {
    fn from(n: NotAvailable) -> Self {
        TemplateError::NotAvailable(n)
    }
}

/// What a host folder is mounted as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountRole {
    /// The companion's settings, at `/config`.
    Config,
    /// Where a download client saves, at `/downloads`.
    Downloads,
}

impl MountRole {
    fn name(self) -> &'static str {
        match self {
            MountRole::Config => "config",
            MountRole::Downloads => "downloads",
        }
    }
}

/// The host folders a companion is given.
#[derive(Debug, Clone, Default)]
pub struct Paths {
    pub config: Option<HostPath>,
    pub downloads: Option<HostPath>,
}

impl Paths {
    fn get(&self, role: MountRole) -> Option<&HostPath> {
        match role {
            MountRole::Config => self.config.as_ref(),
            MountRole::Downloads => self.downloads.as_ref(),
        }
    }
}

/// Where a published port listens on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bind {
    /// `127.0.0.1` only: reachable from the host itself.
    Loopback,
    /// Every address: reachable from your network. Asked for explicitly.
    Lan,
}

/// A catalogue port to publish on the host. A port not listed stays on
/// the internal network, which is the default for every port.
#[derive(Debug, Clone)]
pub struct Publish {
    pub port: PortName,
    pub bind: Bind,
    pub host_port: PortNumber,
}

/// One of Lodestar's own data folders on the host. Compared against, never
/// written, so it takes any clean absolute path (`/var/lib/lodestar`,
/// which a [`HostPath`] refuses).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataFolder(Vec<String>);

impl DataFolder {
    /// From the Unraid template's value or what you typed.
    pub fn parse(s: &str) -> Result<DataFolder, InputError> {
        let rest = s.strip_prefix('/').ok_or(InputError::NotAbsolute)?;
        if rest.is_empty() || s.len() > 4096 {
            return Err(InputError::Length);
        }
        let mut c = Vec::new();
        for part in rest.split('/') {
            if part.is_empty() || part == "." || part == ".." {
                return Err(InputError::Component);
            }
            if part.chars().any(char::is_control) {
                return Err(InputError::Character);
            }
            c.push(part.to_lowercase());
        }
        Ok(DataFolder(c))
    }

    /// The running instance's own data folder (native installs), resolved
    /// through symlinks first.
    pub fn from_running(path: &Path) -> Result<DataFolder, InputError> {
        let real = std::fs::canonicalize(path).map_err(|_| InputError::NotAbsolute)?;
        DataFolder::parse(real.to_str().ok_or(InputError::Character)?)
    }
}

/// Lodestar's data folders on the host: the main one, and Unraid's
/// optional hot and cold paths. Under Docker, Lodestar sees `/data`, so
/// these come from the Unraid template or from you.
#[derive(Debug, Clone, Default)]
pub struct DataFolders(Vec<DataFolder>);

impl DataFolders {
    /// None known: only for data in a Docker named volume, which lives
    /// under `/var/lib/docker`, a place [`HostPath`] already refuses.
    pub fn none() -> DataFolders {
        DataFolders(Vec::new())
    }

    /// The main folder plus the hot and cold paths where set. Each is
    /// parsed with [`DataFolder::parse`].
    pub fn new(main: &str, hot: Option<&str>, cold: Option<&str>) -> Result<Self, TemplateError> {
        let mut v = Vec::new();
        for s in std::iter::once(main).chain(hot).chain(cold) {
            v.push(DataFolder::parse(s).map_err(TemplateError::BadDataFolder)?);
        }
        Ok(DataFolders(v))
    }

    pub fn with(mut self, folder: DataFolder) -> Self {
        self.0.push(folder);
        self
    }
}

/// How the companion's traffic leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Straight out, on the given network.
    Direct,
    /// Through gluetun (`network_mode: service:gluetun`), which is
    /// generated alongside it. Published ports move to gluetun.
    Gluetun,
}

/// Everything a generated service is built from besides the catalogue.
#[derive(Debug, Clone)]
pub struct ServiceInput {
    pub puid: Puid,
    pub pgid: Pgid,
    /// The user-defined network Lodestar is on, so it can reach the
    /// companion by name.
    pub network: NetworkName,
    pub route: Route,
    pub paths: Paths,
    pub publish: Vec<Publish>,
    pub data: DataFolders,
}

// ─── Per-companion profiles ────────────────────────────────────────────
//
// What the catalogue doesn't hold: an image's mounts and variables. Code,
// not data, so nothing here can be changed without a review.

pub(crate) enum Env {
    Puid,
    Pgid,
    Tz,
    Umask,
    Fixed(&'static str),
    /// The catalogue port's number.
    Port(&'static str),
    /// A VPN setting you fill in.
    Vpn(&'static str),
}

pub(crate) struct Mount {
    pub role: MountRole,
    pub target: &'static str,
    pub label: &'static str,
    pub help: &'static str,
}

pub(crate) struct Profile {
    pub id: &'static str,
    pub mounts: &'static [Mount],
    pub env: &'static [(&'static str, Env)],
    pub category: &'static str,
    pub overview: &'static str,
    pub webui: Option<&'static str>,
}

const CONFIG: Mount = Mount {
    role: MountRole::Config,
    target: "/config",
    label: "Config",
    help: "Its settings. A real folder of its own, not a link.",
};

const PROFILES: &[Profile] = &[
    Profile {
        id: "qbittorrent",
        mounts: &[
            CONFIG,
            Mount {
                role: MountRole::Downloads,
                target: "/downloads",
                label: "Downloads",
                help: "Where downloads are saved. A real folder, not a link.",
            },
        ],
        env: &[
            ("PUID", Env::Puid),
            ("PGID", Env::Pgid),
            ("TZ", Env::Tz),
            ("UMASK", Env::Umask),
            ("WEBUI_PORT", Env::Port("webui")),
            ("TORRENTING_PORT", Env::Port("p2p")),
        ],
        category: "Downloaders:",
        overview: "qBittorrent, the download client Lodestar sends your downloads to. Generated by Lodestar: it configures qBittorrent once it's running.",
        webui: Some("webui"),
    },
    Profile {
        id: "prowlarr",
        mounts: &[CONFIG],
        env: &[
            ("PUID", Env::Puid),
            ("PGID", Env::Pgid),
            ("TZ", Env::Tz),
            ("UMASK", Env::Umask),
        ],
        category: "Downloaders: Tools:",
        overview: "Prowlarr, the indexer manager behind Lodestar's Prowlarr source. Generated by Lodestar: it configures Prowlarr once it's running.",
        webui: Some("webui"),
    },
    Profile {
        id: "flaresolverr",
        mounts: &[],
        env: &[("TZ", Env::Tz), ("LOG_LEVEL", Env::Fixed("info"))],
        category: "Tools:",
        overview: "FlareSolverr, which Prowlarr uses for indexers behind a browser check. Generated by Lodestar.",
        webui: None,
    },
    Profile {
        id: GLUETUN,
        mounts: &[],
        env: &[
            ("TZ", Env::Tz),
            (
                "VPN_SERVICE_PROVIDER",
                Env::Vpn("Your VPN provider, as gluetun names it."),
            ),
            ("VPN_TYPE", Env::Fixed("wireguard")),
            (
                "WIREGUARD_PRIVATE_KEY",
                Env::Vpn("Your WireGuard private key, from your provider."),
            ),
            (
                "WIREGUARD_ADDRESSES",
                Env::Vpn("Your WireGuard address, from your provider."),
            ),
        ],
        category: "Network:VPN",
        overview: "gluetun, the VPN client companion traffic goes through. Fill in the VPN settings yourself: Lodestar never asks for or stores them. Its firewall blocks all traffic while the VPN is down.",
        webui: None,
    },
];

/// gluetun's capability, with why: it sets up the tunnel interface, its
/// routes and its firewall (the kill switch). gluetun's upstream setup
/// requires it; nothing else gets a capability.
pub(crate) const GLUETUN_CAP: &str = "NET_ADMIN";
/// gluetun's one device: the kernel's tunnel device, which WireGuard and
/// OpenVPN need (upstream setup). No other device is ever passed.
pub(crate) const GLUETUN_DEVICE: &str = "/dev/net/tun:/dev/net/tun";

static NO_PATHS: Paths = Paths {
    config: None,
    downloads: None,
};

/// A companion after every check: what both generators render.
pub(crate) struct Checked<'a> {
    pub entry: &'a Entry,
    pub profile: &'static Profile,
    pub image: String,
    pub name: &'a ContainerName,
    pub mounts: Vec<(&'static Mount, &'a HostPath)>,
    pub env: Vec<(&'static str, String, Option<&'static str>)>,
    pub publish: Vec<Published>,
}

/// A port to publish, after the checks.
#[derive(Clone)]
pub(crate) struct Published {
    pub name: &'static str,
    pub target: PortNumber,
    pub host: PortNumber,
    pub proto: Proto,
    pub bind: Bind,
    /// The companion's web UI, for Unraid's WebUI link.
    pub webui: bool,
}

pub(crate) fn check<'a>(
    catalogue: &'a Catalogue,
    id: &CompanionId,
    input: &ServiceInput,
    paths: &'a Paths,
    publish_in: &[Publish],
    template: TemplateId,
) -> Result<Checked<'a>, TemplateError> {
    let entry = catalogue
        .get(id.as_str())
        .ok_or(TemplateError::UnknownCompanion)?;
    let profile = PROFILES
        .iter()
        .find(|p| p.id == id.as_str())
        .filter(|_| entry.templates.contains(&template))
        .ok_or(TemplateError::NoTemplate)?;
    let (container, image) = entry.renderable()?;

    let mut mounts = Vec::new();
    for m in profile.mounts {
        let path = paths
            .get(m.role)
            .ok_or(TemplateError::MissingPath(m.role))?;
        check_path(catalogue, entry, path, &input.data)?;
        mounts.push((m, path));
    }
    for role in [MountRole::Config, MountRole::Downloads] {
        if paths.get(role).is_some() && !profile.mounts.iter().any(|m| m.role == role) {
            return Err(TemplateError::UnexpectedPath(role));
        }
    }

    let mut env = Vec::new();
    for (key, e) in profile.env {
        let (value, help) = match e {
            Env::Puid => (input.puid.to_string(), None),
            Env::Pgid => (input.pgid.to_string(), None),
            Env::Tz => (TZ.to_owned(), None),
            Env::Umask => (UMASK.to_owned(), None),
            Env::Fixed(v) => ((*v).to_owned(), None),
            Env::Port(name) => (
                entry
                    .port(name)
                    .ok_or(TemplateError::NoTemplate)?
                    .number
                    .to_string(),
                None,
            ),
            Env::Vpn(help) => (PLACEHOLDER.to_owned(), Some(*help)),
        };
        env.push((*key, value, help));
    }

    let mut publish: Vec<Published> = Vec::new();
    for (i, p) in publish_in.iter().enumerate() {
        let port = entry
            .port(p.port.as_str())
            .ok_or(TemplateError::UnknownPort)?;
        let twice = publish_in[..i].iter().any(|q| q.port == p.port)
            || publish
                .iter()
                .any(|q| q.host == p.host_port && q.proto == port.proto);
        if twice {
            return Err(TemplateError::DuplicatePort);
        }
        // Through the VPN, a peer port only works if your provider forwards
        // one, and publishing it on the host would answer peers from your
        // own address. So it's refused rather than half-made.
        if input.route == Route::Gluetun && port.exposure == Exposure::P2p {
            return Err(TemplateError::P2pThroughVpn);
        }
        publish.push(Published {
            name: port_label(port.name.as_str()),
            target: port.number,
            host: p.host_port,
            proto: port.proto,
            bind: p.bind,
            webui: profile.webui == Some(port.name.as_str()),
        });
    }

    Ok(Checked {
        entry,
        profile,
        image: image.reference(),
        name: container.name(),
        mounts,
        env,
        publish,
    })
}

/// Refuses a folder that overlaps Lodestar's data, sits inside another
/// catalogue companion's `appdata/<name>`, or has a dot component.
fn check_path(
    catalogue: &Catalogue,
    entry: &Entry,
    path: &HostPath,
    data: &DataFolders,
) -> Result<(), TemplateError> {
    let c: Vec<String> = path.as_str()[1..]
        .split('/')
        .map(str::to_ascii_lowercase)
        .collect();
    if c.iter().any(|x| x.starts_with('.')) {
        return Err(TemplateError::DotFolder);
    }
    if data
        .0
        .iter()
        .any(|d| c.starts_with(&d.0) || d.0.starts_with(&c))
    {
        return Err(TemplateError::DataFolder);
    }
    let others: Vec<&str> = catalogue
        .entries()
        .iter()
        .filter(|e| e.id != entry.id)
        .filter_map(|e| e.container.as_ref().map(|k| k.name().as_str()))
        .collect();
    if c.windows(2)
        .any(|w| w[0] == "appdata" && others.contains(&w[1].as_str()))
    {
        return Err(TemplateError::OtherCompanion);
    }
    Ok(())
}

/// Checks both companions of a routed pair: the companion, and gluetun
/// carrying the companion's published ports.
pub(crate) fn check_pair<'a>(
    catalogue: &'a Catalogue,
    id: &CompanionId,
    input: &'a ServiceInput,
    template: TemplateId,
) -> Result<(Checked<'a>, Option<Checked<'a>>), TemplateError> {
    let mut main = check(catalogue, id, input, &input.paths, &input.publish, template)?;
    if input.route == Route::Direct {
        return Ok((main, None));
    }
    if id.as_str() == GLUETUN {
        return Err(TemplateError::GluetunSelf);
    }
    let gid = CompanionId::parse(GLUETUN).map_err(|_| TemplateError::UnknownCompanion)?;
    let mut vpn = check(catalogue, &gid, input, &NO_PATHS, &[], template)?;
    // The companion shares gluetun's network stack, so its ports are
    // published on gluetun.
    vpn.publish = std::mem::take(&mut main.publish);
    Ok((main, Some(vpn)))
}

/// A port's label in Unraid's form: fixed text for the catalogue's names.
fn port_label(name: &str) -> &'static str {
    match name {
        "webui" => "Web UI port",
        "p2p" => "Peer port (TCP)",
        "p2p_udp" => "Peer port (UDP)",
        "control" => "Control port",
        _ => "Port",
    }
}

pub(crate) fn proto_name(p: Proto) -> &'static str {
    match p {
        Proto::Tcp => "tcp",
        Proto::Udp => "udp",
    }
}

// ─── The compose writer ─────────────────────────────────────────────────

/// A YAML node. Keys and strings are written by `serde_json`, numbers and
/// booleans as JSON too; comments are fixed text from this file.
enum Y {
    S(String),
    N(u16),
    B(bool),
    Map(Vec<(Option<&'static str>, String, Y)>),
    Seq(Vec<Y>),
}

fn s(v: impl Into<String>) -> Y {
    Y::S(v.into())
}

fn map(entries: Vec<(&str, Y)>) -> Y {
    Y::Map(
        entries
            .into_iter()
            .map(|(k, v)| (None, k.to_owned(), v))
            .collect(),
    )
}

/// `v` as a JSON string. `$` is refused: compose interpolates `$VAR` even
/// inside quotes.
fn quote(v: &str) -> Result<String, TemplateError> {
    if v.contains('$') {
        return Err(TemplateError::Interpolation);
    }
    Ok(Value::String(v.to_owned()).to_string())
}

fn scalar(y: &Y) -> Result<Option<String>, TemplateError> {
    Ok(match y {
        Y::S(v) => Some(quote(v)?),
        Y::N(n) => Some(n.to_string()),
        Y::B(b) => Some(b.to_string()),
        Y::Map(m) if m.is_empty() => Some("{}".to_owned()),
        Y::Seq(q) if q.is_empty() => Some("[]".to_owned()),
        Y::Map(_) | Y::Seq(_) => None,
    })
}

fn comment(out: &mut String, pad: &str, text: &str) {
    for line in text.lines() {
        out.push_str(pad);
        out.push_str("# ");
        out.push_str(line);
        out.push('\n');
    }
}

/// Writes a map's entries at `indent`. `first` is written in place of the
/// first entry's indent (a sequence item's `- `).
fn emit_map(
    out: &mut String,
    entries: &[(Option<&'static str>, String, Y)],
    indent: usize,
    mut first: Option<&str>,
) -> Result<(), TemplateError> {
    let pad = " ".repeat(indent);
    for (note, key, value) in entries {
        if let Some(text) = note {
            comment(out, &pad, text);
        }
        out.push_str(first.take().unwrap_or(&pad));
        out.push_str(&quote(key)?);
        out.push(':');
        emit_value(out, value, indent + 2)?;
    }
    Ok(())
}

fn emit_value(out: &mut String, value: &Y, indent: usize) -> Result<(), TemplateError> {
    if let Some(v) = scalar(value)? {
        out.push(' ');
        out.push_str(&v);
        out.push('\n');
        return Ok(());
    }
    out.push('\n');
    match value {
        Y::Map(m) => emit_map(out, m, indent, None),
        Y::Seq(items) => {
            let pad = " ".repeat(indent);
            for item in items {
                match (scalar(item)?, item) {
                    (Some(v), _) => {
                        out.push_str(&pad);
                        out.push_str("- ");
                        out.push_str(&v);
                        out.push('\n');
                    }
                    (None, Y::Map(m)) => emit_map(out, m, indent + 2, Some(&format!("{pad}- ")))?,
                    // A sequence of sequences is never built.
                    (None, _) => return Err(TemplateError::Encode),
                }
            }
            Ok(())
        }
        _ => Err(TemplateError::Encode),
    }
}

const HEADER: &str = "Generated by Lodestar. Check it before you apply it.
Mount real folders only, never a link to somewhere else.
Ports stay on the Docker network unless you chose to publish them.";

const VPN_NOTE: &str = "Fill these in yourself, from your VPN provider. Lodestar never asks for
or stores them. gluetun won't start until they are real values.";

fn environment(c: &Checked<'_>) -> Y {
    Y::Map(
        c.env
            .iter()
            .map(|(k, v, help)| (*help, (*k).to_owned(), s(v.as_str())))
            .collect(),
    )
}

fn ports(c: &Checked<'_>) -> Y {
    Y::Seq(
        c.publish
            .iter()
            .map(|p| {
                let mut m = vec![
                    ("target", Y::N(p.target.get())),
                    ("published", Y::N(p.host.get())),
                ];
                if p.bind == Bind::Loopback {
                    m.push(("host_ip", s("127.0.0.1")));
                }
                m.push(("protocol", s(proto_name(p.proto))));
                map(m)
            })
            .collect(),
    )
}

/// The parts every service has, then its own.
fn service(c: &Checked<'_>, mut own: Vec<(Option<&'static str>, String, Y)>) -> Y {
    let mut m = vec![
        (None, "image".to_owned(), s(c.image.as_str())),
        (None, "container_name".to_owned(), s(c.name.as_str())),
        (None, "restart".to_owned(), s("unless-stopped")),
    ];
    m.append(&mut own);
    m.push((None, "environment".to_owned(), environment(c)));
    if !c.mounts.is_empty() {
        let vols = c
            .mounts
            .iter()
            .map(|(m, path)| {
                map(vec![
                    ("type", s("bind")),
                    ("source", s(path.as_str())),
                    ("target", s(m.target)),
                    ("bind", map(vec![("create_host_path", Y::B(true))])),
                ])
            })
            .collect();
        m.push((None, "volumes".to_owned(), Y::Seq(vols)));
    }
    if !c.publish.is_empty() {
        m.push((None, "ports".to_owned(), ports(c)));
    }
    Y::Map(m)
}

/// The compose document for companion `id`: its service, gluetun's when
/// `input.route` is [`Route::Gluetun`], and the external network Lodestar
/// is on. What `lodestar companion compose <id>` prints (CMP-5).
pub fn compose(
    catalogue: &Catalogue,
    id: &CompanionId,
    input: &ServiceInput,
) -> Result<String, TemplateError> {
    let (main, vpn) = check_pair(catalogue, id, input, TemplateId::ComposeService)?;
    let net = input.network.as_str();
    let mut services = Vec::new();
    let joins = || (None, "networks".to_owned(), Y::Seq(vec![s(net)]));
    match &vpn {
        Some(v) => {
            let mut own = gluetun_own();
            own.push((
                Some("Lodestar reaches the companion by its own name, through gluetun."),
                "networks".to_owned(),
                map(vec![(
                    net,
                    map(vec![("aliases", Y::Seq(vec![s(main.name.as_str())]))]),
                )]),
            ));
            services.push((Some(VPN_NOTE), GLUETUN.to_owned(), service(v, own)));
            let own = vec![
                (
                    Some("All of its traffic goes through gluetun."),
                    "network_mode".to_owned(),
                    s(NetworkMode::ServiceGluetun.compose_value()),
                ),
                (
                    None,
                    "depends_on".to_owned(),
                    map(vec![(
                        GLUETUN,
                        map(vec![("condition", s("service_healthy"))]),
                    )]),
                ),
            ];
            services.push((None, main.name.as_str().to_owned(), service(&main, own)));
        }
        None if main.profile.id == GLUETUN => {
            let mut own = gluetun_own();
            own.push(joins());
            services.push((Some(VPN_NOTE), GLUETUN.to_owned(), service(&main, own)));
        }
        None => {
            let own = vec![joins()];
            services.push((None, main.name.as_str().to_owned(), service(&main, own)));
        }
    }
    render(services, net)
}

/// gluetun's capability and device, each with why.
fn gluetun_own() -> Vec<(Option<&'static str>, String, Y)> {
    vec![
        (
            Some(
                "NET_ADMIN lets gluetun set up the tunnel, its routes and its firewall\n(the kill switch). gluetun's upstream setup requires it.",
            ),
            "cap_add".to_owned(),
            Y::Seq(vec![s(GLUETUN_CAP)]),
        ),
        (
            Some("The kernel's tunnel device, which the VPN needs (upstream setup)."),
            "devices".to_owned(),
            Y::Seq(vec![s(GLUETUN_DEVICE)]),
        ),
    ]
}

fn render(
    services: Vec<(Option<&'static str>, String, Y)>,
    net: &str,
) -> Result<String, TemplateError> {
    let doc = vec![
        (None, "services".to_owned(), Y::Map(services)),
        (
            Some("The network Lodestar is on. It must exist already (docker network ls)."),
            "networks".to_owned(),
            map(vec![(
                net,
                map(vec![("name", s(net)), ("external", Y::B(true))]),
            )]),
        ),
    ];
    let mut out = String::new();
    comment(&mut out, "", HEADER);
    emit_map(&mut out, &doc, 0, None)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_refuses_interpolation_and_escapes() {
        assert_eq!(quote("${HOME}"), Err(TemplateError::Interpolation));
        assert_eq!(quote("a\"b\n").as_deref(), Ok("\"a\\\"b\\n\""));
    }

    #[test]
    fn emitter_nests_maps_and_sequences() {
        let doc = vec![(
            Some("note"),
            "k".to_owned(),
            Y::Seq(vec![map(vec![("a", Y::N(1025)), ("b", Y::B(false))]), s("x")]),
        )];
        let mut out = String::new();
        emit_map(&mut out, &doc, 0, None).expect("emit");
        assert_eq!(out, "# note\n\"k\":\n  - \"a\": 1025\n    \"b\": false\n  - \"x\"\n");
        let mut out = String::new();
        emit_map(&mut out, &[(None, "e".to_owned(), Y::Map(vec![]))], 0, None).expect("emit");
        assert_eq!(out, "\"e\": {}\n");
    }

    #[test]
    fn data_folder_compares_without_case() {
        let d = DataFolder::parse("/MNT/Cache/Lodestar").expect("folder");
        assert_eq!(d.0, ["mnt", "cache", "lodestar"]);
        assert_eq!(DataFolder::parse("/"), Err(InputError::Length));
        assert_eq!(DataFolder::parse("/a//b"), Err(InputError::Component));
    }

    #[test]
    fn every_error_has_a_distinct_code() {
        let all = [
            TemplateError::UnknownCompanion,
            TemplateError::NotAvailable(NotAvailable::AwaitingPin),
            TemplateError::NotAvailable(NotAvailable::NoBuild),
            TemplateError::NoTemplate,
            TemplateError::MissingPath(MountRole::Config),
            TemplateError::UnexpectedPath(MountRole::Config),
            TemplateError::DataFolder,
            TemplateError::OtherCompanion,
            TemplateError::DotFolder,
            TemplateError::BadDataFolder(InputError::Length),
            TemplateError::UnknownPort,
            TemplateError::DuplicatePort,
            TemplateError::GluetunSelf,
            TemplateError::P2pThroughVpn,
            TemplateError::Interpolation,
            TemplateError::Encode,
        ];
        let mut codes: Vec<&str> = all.iter().map(|e| e.code()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), all.len());
        assert!(all.iter().all(|e| !e.to_string().is_empty()));
    }
}
