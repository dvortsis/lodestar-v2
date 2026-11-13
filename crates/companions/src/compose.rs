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

/// Lodestar's post-rules for gluetun (X-gluetun-leak): without them gluetun
/// lets connections opened through the tunnel leave by your normal network
/// once the tunnel drops. A copy of `packaging/companions/gluetun-post-rules.txt`,
/// kept inside the crate so every build context has it; a test keeps the two
/// identical byte for byte. gluetun runs its lines as root.
pub const GLUETUN_POST_RULES: &str = include_str!("gluetun-post-rules.txt");

/// Where gluetun reads its post-rules, inside its container.
pub const GLUETUN_POST_RULES_TARGET: &str = "/iptables/post-rules.txt";

/// The tunnel interface the post-rules name. gluetun's image defaults
/// `VPN_INTERFACE` to `tun0`, so every template sets it: the templates are
/// WireGuard only (`VPN_TYPE`), and the rules name `wg0`.
pub const GLUETUN_INTERFACE: &str = "wg0";

/// The compose config that carries the post-rules.
const POST_RULES_CONFIG: &str = "gluetun-post-rules";

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
    /// A peer-to-peer port published on `127.0.0.1`, where no peer can
    /// reach it.
    P2pLoopback,
    /// A port published on a different host port where the image needs
    /// both sides equal, or peer ports on different host ports.
    PortRemap,
    /// gluetun's control port published on your network.
    LanRefused,
    /// A port published while routed through a gluetun you already run.
    PublishOnExisting,
    /// The running instance's data folder couldn't be resolved.
    DataFolderUnreadable,
    /// A value held a character YAML can't hold in a quoted string, or
    /// one YAML 1.1 reads as a line break (U+2028, U+2029).
    Unprintable,
    /// A value held `$`, which compose would interpolate.
    Interpolation,
    /// The XML writer failed.
    Encode,
    /// An Unraid `ExtraParams` flag held a character outside its
    /// allowlist; Unraid pastes that field into a shell line.
    ShellUnsafe,
    /// An existing gluetun's name is another catalogue companion's.
    ExistingName,
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
            TemplateError::P2pLoopback => "p2p_loopback",
            TemplateError::PortRemap => "port_remap",
            TemplateError::LanRefused => "lan_refused",
            TemplateError::PublishOnExisting => "publish_on_existing",
            TemplateError::DataFolderUnreadable => "data_folder_unreadable",
            TemplateError::Unprintable => "unprintable",
            TemplateError::Interpolation => "interpolation",
            TemplateError::Encode => "encode",
            TemplateError::ShellUnsafe => "shell_unsafe",
            TemplateError::ExistingName => "existing_name",
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
                TemplateError::P2pLoopback => "a peer port on 127.0.0.1 can't be reached by peers",
                TemplateError::PortRemap => "this port must be published on the same number",
                TemplateError::LanRefused => "gluetun's control port stays off your network",
                TemplateError::PublishOnExisting => {
                    "publish the port on your gluetun container instead"
                }
                TemplateError::DataFolderUnreadable => "the data folder couldn't be resolved",
                TemplateError::Unprintable => "a value holds a character that can't be written",
                TemplateError::ShellUnsafe => "an Unraid flag holds a character that isn't allowed",
                TemplateError::ExistingName => "that name belongs to another companion",
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
    pub fn from_running(path: &Path) -> Result<DataFolder, TemplateError> {
        let real = std::fs::canonicalize(path).map_err(|_| TemplateError::DataFolderUnreadable)?;
        let s = real
            .to_str()
            .ok_or(TemplateError::BadDataFolder(InputError::Character))?;
        DataFolder::parse(s).map_err(TemplateError::BadDataFolder)
    }
}

/// Lodestar's data folders on the host: the main one, and Unraid's
/// optional hot and cold paths. Under Docker, Lodestar sees `/data`, so
/// these come from the Unraid template or from you.
#[derive(Debug, Clone)]
pub struct DataFolders(Vec<DataFolder>);

impl DataFolders {
    /// None known: only for data in a Docker named volume, which lives
    /// under `/var/lib/docker`, a place [`HostPath`] already refuses.
    pub fn none() -> DataFolders {
        DataFolders(Vec::new())
    }

    /// The main folder plus the hot and cold paths where set. Each is
    /// parsed with [`DataFolder::parse`].
    pub fn new<'a>(
        main: &'a str,
        hot: Option<&'a str>,
        cold: Option<&'a str>,
    ) -> Result<Self, TemplateError> {
        let mut v = Vec::new();
        // Unraid writes an unset hot or cold path as an empty value.
        let set = |o: Option<&'a str>| o.filter(|s| !s.is_empty());
        for s in std::iter::once(main).chain(set(hot)).chain(set(cold)) {
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// Straight out, on the given network.
    Direct,
    /// Through a gluetun generated alongside it
    /// (`network_mode: service:gluetun`). Published ports move to gluetun.
    Gluetun,
    /// Through a gluetun container you already run, by its name
    /// (`network_mode: container:<name>`). Nothing is generated for it, so
    /// its VPN settings are left alone, and the companion can't publish a
    /// port. Lodestar reaches the companion at that container's name.
    ExistingGluetun(ContainerName),
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
    /// Other companions' host folders, where known (CMP-5 and CMP-6 know
    /// what they generated). A folder that overlaps one is refused.
    pub taken: Vec<HostPath>,
    /// Where Lodestar saved gluetun's post-rules on the host, when it
    /// could. Unraid's gluetun template then mounts that file; without it,
    /// you save the file yourself. Compose carries the rules inline either
    /// way, so it doesn't read this.
    pub gluetun_paths: Option<GluetunPaths>,
}

/// The host folders gluetun's files come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GluetunPaths {
    /// Lodestar's companion-files folder on the host. Lodestar writes
    /// `gluetun/post-rules.txt` inside it. It must not overlap Lodestar's
    /// data folder, which holds its secrets.
    pub companions: HostPath,
}

/// The folder inside the companion-files folder that holds gluetun's files.
pub const GLUETUN_FILES_DIR: &str = "gluetun";
/// The post-rules file's name in [`GLUETUN_FILES_DIR`].
pub const POST_RULES_NAME: &str = "post-rules.txt";

impl GluetunPaths {
    /// The post-rules file on the host.
    pub fn post_rules(&self) -> String {
        format!(
            "{}/{GLUETUN_FILES_DIR}/{POST_RULES_NAME}",
            self.companions.as_str()
        )
    }

    /// Refuses a companion-files folder that overlaps Lodestar's data folder
    /// (the running one where known, and Unraid's `appdata/lodestar`, which a
    /// container can't see), or another companion's folder, or has a dot
    /// component. gluetun mounts a file from it, so it must hold nothing of
    /// Lodestar's.
    pub fn check(&self, input: &ServiceInput) -> Result<(), TemplateError> {
        let c = components(&self.companions);
        if c.iter().any(|x| x.starts_with('.')) {
            return Err(TemplateError::DotFolder);
        }
        let lodestar_appdata = c
            .windows(2)
            .any(|w| w[0] == "appdata" && crate::inputs::LODESTAR_APPDATA.contains(&w[1].as_str()));
        if lodestar_appdata || input.data.0.iter().any(|d| overlaps(&c, &d.0)) {
            return Err(TemplateError::DataFolder);
        }
        if input.taken.iter().any(|t| overlaps(&c, &components(t))) {
            return Err(TemplateError::OtherCompanion);
        }
        Ok(())
    }
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
    Vpn {
        help: &'static str,
        secret: bool,
    },
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
                Env::Vpn {
                    help: "Your VPN provider, as gluetun names it.",
                    secret: false,
                },
            ),
            ("VPN_TYPE", Env::Fixed("wireguard")),
            ("VPN_INTERFACE", Env::Fixed(GLUETUN_INTERFACE)),
            (
                "WIREGUARD_PRIVATE_KEY",
                Env::Vpn {
                    help: "Your WireGuard private key, from your provider.",
                    secret: true,
                },
            ),
            (
                "WIREGUARD_ADDRESSES",
                Env::Vpn {
                    help: "Your WireGuard address, from your provider.",
                    secret: true,
                },
            ),
        ],
        category: "Network:VPN",
        overview: "gluetun, the VPN client companion traffic goes through. Fill in the VPN settings yourself: Lodestar never asks for or stores them. While the VPN is down, its firewall blocks new connections, and Lodestar's post-rules stop connections that were already open from leaving outside the VPN.",
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

/// Key, value, and for a VPN setting its help and whether it's secret.
pub(crate) type EnvVar = (&'static str, String, Option<(&'static str, bool)>);

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
    pub env: Vec<EnvVar>,
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
    pub p2p: bool,
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
        check_path(catalogue, entry, path, input)?;
        mounts.push((m, path));
    }
    for role in [MountRole::Config, MountRole::Downloads] {
        if paths.get(role).is_some() && !profile.mounts.iter().any(|m| m.role == role) {
            return Err(TemplateError::UnexpectedPath(role));
        }
    }

    let publish = check_publish(entry, profile, input, publish_in)?;
    // A peer port listens inside on the port it's published on, so the
    // port qBittorrent announces is the one peers reach.
    let p2p_host = publish.iter().find(|p| p.p2p).map(|p| p.host);

    let mut env = Vec::new();
    for (key, e) in profile.env {
        let (value, help) = match e {
            Env::Puid => (input.puid.to_string(), None),
            Env::Pgid => (input.pgid.to_string(), None),
            Env::Tz => (TZ.to_owned(), None),
            Env::Umask => (UMASK.to_owned(), None),
            Env::Fixed(v) => ((*v).to_owned(), None),
            Env::Port(name) => {
                let port = entry.port(name).ok_or(TemplateError::NoTemplate)?;
                let n = match (port.exposure, p2p_host) {
                    (Exposure::P2p, Some(h)) => h,
                    _ => port.number,
                };
                (n.to_string(), None)
            }
            Env::Vpn { help, secret } => (PLACEHOLDER.to_owned(), Some((*help, *secret))),
        };
        env.push((*key, value, help));
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

fn check_publish(
    entry: &Entry,
    profile: &Profile,
    input: &ServiceInput,
    publish_in: &[Publish],
) -> Result<Vec<Published>, TemplateError> {
    let mut publish: Vec<Published> = Vec::new();
    for (i, p) in publish_in.iter().enumerate() {
        let port = entry
            .port(p.port.as_str())
            .ok_or(TemplateError::UnknownPort)?;
        let p2p = port.exposure == Exposure::P2p;
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
        if p2p && input.route != Route::Direct {
            return Err(TemplateError::P2pThroughVpn);
        }
        // No peer can reach a port on 127.0.0.1.
        if p2p && p.bind == Bind::Loopback {
            return Err(TemplateError::P2pLoopback);
        }
        // gluetun's control server reports the public address and can
        // change the VPN's state; FlareSolverr's API fetches any URL it's
        // given, with no sign-in (CMP-12): neither on your network.
        if matches!(profile.id, GLUETUN | crate::flaresolverr::ID) && p.bind == Bind::Lan {
            return Err(TemplateError::LanRefused);
        }
        // A port the image is told about (qBittorrent's `WEBUI_PORT`) must
        // be the same on both sides: its web UI checks the port it's
        // reached on. Peer ports follow the host port instead.
        let told = profile
            .env
            .iter()
            .any(|(_, e)| matches!(e, Env::Port(n) if *n == port.name.as_str()));
        if told && !p2p && p.host_port != port.number {
            return Err(TemplateError::PortRemap);
        }
        // All of an entry's peer ports are one listening port.
        if p2p && publish.iter().any(|q| q.p2p && q.host != p.host_port) {
            return Err(TemplateError::PortRemap);
        }
        publish.push(Published {
            name: port_label(port.name.as_str()),
            target: if p2p { p.host_port } else { port.number },
            host: p.host_port,
            proto: port.proto,
            bind: p.bind,
            webui: profile.webui == Some(port.name.as_str()),
            p2p,
        });
    }
    Ok(publish)
}

/// A path's components for comparison, lower case. Under `/mnt`, Unraid
/// shows one share through every pool and disk (`/mnt/user/x`,
/// `/mnt/user0/x`, `/mnt/cache/x` and `/mnt/disk1/x` are the same files),
/// so there the pool level is dropped and the key starts at the share.
fn key(c: &[String]) -> (bool, &[String]) {
    match c {
        [first, _pool, rest @ ..] if first == "mnt" => (true, rest),
        _ => (false, c),
    }
}

/// Whether two folders are the same, or one holds the other.
fn overlaps(a: &[String], b: &[String]) -> bool {
    let (ma, ka) = key(a);
    let (mb, kb) = key(b);
    a.starts_with(b) || b.starts_with(a) || (ma && mb && (ka.starts_with(kb) || kb.starts_with(ka)))
}

fn components(path: &HostPath) -> Vec<String> {
    path.as_str()
        .split('/')
        .skip(1)
        .map(str::to_ascii_lowercase)
        .collect()
}

/// Refuses a folder that overlaps Lodestar's data or another companion's
/// folder, sits inside another catalogue companion's `appdata/<name>`, or
/// has a dot component.
fn check_path(
    catalogue: &Catalogue,
    entry: &Entry,
    path: &HostPath,
    input: &ServiceInput,
) -> Result<(), TemplateError> {
    let c = components(path);
    if c.iter().any(|x| x.starts_with('.')) {
        return Err(TemplateError::DotFolder);
    }
    if input.data.0.iter().any(|d| overlaps(&c, &d.0)) {
        return Err(TemplateError::DataFolder);
    }
    if input.taken.iter().any(|t| overlaps(&c, &components(t))) {
        return Err(TemplateError::OtherCompanion);
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

/// Checks both companions of a routed pair: gluetun carrying the
/// companion's published ports, and the companion.
pub(crate) fn check_pair<'a>(
    catalogue: &'a Catalogue,
    id: &CompanionId,
    input: &'a ServiceInput,
    template: TemplateId,
) -> Result<(Checked<'a>, Option<Checked<'a>>), TemplateError> {
    let mut main = check(catalogue, id, input, &input.paths, &input.publish, template)?;
    if input.route != Route::Direct && id.as_str() == GLUETUN {
        return Err(TemplateError::GluetunSelf);
    }
    match &input.route {
        Route::Direct => Ok((main, None)),
        // Its ports would have to be published on a container this doesn't
        // generate.
        // A companion's own name would route it through that companion.
        Route::ExistingGluetun(name)
            if catalogue.entries().iter().any(|e| {
                e.id.as_str() != GLUETUN && e.container.as_ref().is_some_and(|k| k.name() == name)
            }) =>
        {
            Err(TemplateError::ExistingName)
        }
        Route::ExistingGluetun(_) if !main.publish.is_empty() => {
            Err(TemplateError::PublishOnExisting)
        }
        Route::ExistingGluetun(_) => Ok((main, None)),
        Route::Gluetun => {
            let gid = CompanionId::parse(GLUETUN).map_err(|_| TemplateError::UnknownCompanion)?;
            let mut vpn = check(catalogue, &gid, input, &NO_PATHS, &[], template)?;
            // The companion shares gluetun's network stack, so its ports are
            // published on gluetun.
            vpn.publish = std::mem::take(&mut main.publish);
            Ok((main, Some(vpn)))
        }
    }
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
    /// Fixed multi-line text from this crate, written as a literal block
    /// (`|`), each line indented. Only the embedded post-rules are ever one.
    Block(&'static str),
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
    printable(v)?;
    Ok(Value::String(v.to_owned()).to_string())
}

/// Refuses what neither YAML nor XML may hold in a value: every control
/// character (serde_json escapes C0, but YAML can't hold DEL or C1 in a
/// quoted string, and quick-xml writes C0 raw, which XML 1.0 forbids),
/// U+2028 and U+2029 (line breaks to YAML 1.1), U+FEFF, and the
/// non-characters U+FFFE and U+FFFF. No closed input holds any of them;
/// this keeps both writers safe on their own.
pub(crate) fn printable(v: &str) -> Result<(), TemplateError> {
    let bad = |c: char| {
        c.is_control()
            || matches!(
                c,
                '\u{2028}' | '\u{2029}' | '\u{feff}' | '\u{fffe}' | '\u{ffff}'
            )
    };
    if v.chars().any(bad) {
        return Err(TemplateError::Unprintable);
    }
    Ok(())
}

fn scalar(y: &Y) -> Result<Option<String>, TemplateError> {
    Ok(match y {
        Y::Block(_) => None,
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
    if let Y::Block(text) = value {
        return emit_block(out, text, indent);
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

/// A literal block: ` |`, then each line at `indent`. The text must be
/// whole lines, each printable, none with `$` (compose interpolates config
/// content too) or a tab, and the first must not start with a space (its
/// indent sets the block's).
fn emit_block(out: &mut String, text: &str, indent: usize) -> Result<(), TemplateError> {
    // YAML reads a literal block back unchanged only if it ends in exactly one
    // newline (clip chomping) and its first non-empty line takes the indent.
    let first = text.lines().find(|l| !l.is_empty()).unwrap_or("");
    if !text.ends_with('\n')
        || text.ends_with("\n\n")
        || first.is_empty()
        || first.starts_with(' ')
        || text.contains(['$', '\t', '\r'])
    {
        return Err(TemplateError::Encode);
    }
    let pad = " ".repeat(indent);
    out.push_str(" |\n");
    for line in text.lines() {
        printable(line)?;
        if !line.is_empty() {
            out.push_str(&pad);
            out.push_str(line);
        }
        out.push('\n');
    }
    Ok(())
}

const HEADER: &str = "Generated by Lodestar. Check it before you apply it.
Mount real folders only, never a link to somewhere else.
Ports stay on the Docker network unless you chose to publish them.
A container that already has one of these names stops it from starting.";

const VPN_NOTE: &str = "Fill these in yourself, from your VPN provider. Lodestar never asks for
or stores them. gluetun won't start until they are real values.";

fn environment(c: &Checked<'_>) -> Y {
    Y::Map(
        c.env
            .iter()
            .map(|(k, v, vpn)| (vpn.map(|(h, _)| h), (*k).to_owned(), s(v.as_str())))
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
        None => match &input.route {
            Route::ExistingGluetun(name) => {
                // `container:` plus a closed ContainerName ([a-z0-9_-]).
                let mode = format!("container:{}", name.as_str());
                let own = vec![(
                    Some("All of its traffic goes through your gluetun container."),
                    "network_mode".to_owned(),
                    s(mode),
                )];
                services.push((None, main.name.as_str().to_owned(), service(&main, own)));
            }
            _ if main.profile.id == GLUETUN => {
                let mut own = gluetun_own();
                own.push(joins());
                services.push((Some(VPN_NOTE), GLUETUN.to_owned(), service(&main, own)));
            }
            _ => {
                let own = vec![joins()];
                services.push((None, main.name.as_str().to_owned(), service(&main, own)));
            }
        },
    }
    let rules = vpn.is_some() || main.profile.id == GLUETUN;
    render(services, net, rules)
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
        (
            Some(
                "Lodestar's post-rules (configs, at the end): gluetun reads them when it\nturns its firewall on. Compose copies them in read-only (mode 0444 by default).",
            ),
            "configs".to_owned(),
            Y::Seq(vec![map(vec![
                ("source", s(POST_RULES_CONFIG)),
                ("target", s(GLUETUN_POST_RULES_TARGET)),
            ])]),
        ),
    ]
}

fn render(
    services: Vec<(Option<&'static str>, String, Y)>,
    net: &str,
    rules: bool,
) -> Result<String, TemplateError> {
    let mut doc = vec![
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
    if rules {
        doc.push((
            Some(
                "gluetun's post-rules. Without them, gluetun lets connections opened through\nthe tunnel leave by your normal network once the tunnel drops. Each rule\nonly marks or drops traffic. Keep VPN_INTERFACE as it is: the rules name it.",
            ),
            "configs".to_owned(),
            map(vec![(
                POST_RULES_CONFIG,
                map(vec![("content", Y::Block(GLUETUN_POST_RULES))]),
            )]),
        ));
    }
    let mut out = String::new();
    comment(&mut out, "", HEADER);
    emit_map(&mut out, &doc, 0, None)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_holds_only_safe_whole_lines() {
        let mut out = String::new();
        assert!(emit_block(&mut out, "a\n\nb c\n", 4).is_ok());
        assert_eq!(out, " |\n    a\n\n    b c\n");
        for bad in [
            "no newline",
            " leading\n",
            "a $X\n",
            "a\tb\n",
            "a\u{1}\n",
            "a\r\n",
            "a\n\n",
            "\n",
            "\n  x\n",
        ] {
            assert!(emit_block(&mut String::new(), bad, 2).is_err(), "{bad:?}");
        }
        assert!(emit_block(&mut String::new(), GLUETUN_POST_RULES, 6).is_ok());
    }

    #[test]
    fn quote_refuses_interpolation_and_escapes() {
        assert_eq!(quote("${HOME}"), Err(TemplateError::Interpolation));
        assert_eq!(quote("a\"b\\#: ").as_deref(), Ok("\"a\\\"b\\\\#: \""));
        assert_eq!(quote("a\nb"), Err(TemplateError::Unprintable));
        assert_eq!(quote("a\u{2028}"), Err(TemplateError::Unprintable));
        assert_eq!(quote("a\u{ffff}"), Err(TemplateError::Unprintable));
        assert_eq!(printable("a\u{1}"), Err(TemplateError::Unprintable));
    }

    #[test]
    fn emitter_nests_maps_and_sequences() {
        let doc = vec![(
            Some("note"),
            "k".to_owned(),
            Y::Seq(vec![
                map(vec![("a", Y::N(1025)), ("b", Y::B(false))]),
                s("x"),
            ]),
        )];
        let mut out = String::new();
        emit_map(&mut out, &doc, 0, None).expect("emit");
        assert_eq!(
            out,
            "# note\n\"k\":\n  - \"a\": 1025\n    \"b\": false\n  - \"x\"\n"
        );
        let mut out = String::new();
        emit_map(&mut out, &[(None, "e".to_owned(), Y::Map(vec![]))], 0, None).expect("emit");
        assert_eq!(out, "\"e\": {}\n");
    }

    proptest::proptest! {
        /// Whatever a key or value holds, it's refused or written as one
        /// JSON string on one line, which reads back as itself. So no value
        /// can add a key, a document, an anchor or an alias, even if a
        /// closed input type were loosened.
        #[test]
        fn any_text_stays_one_scalar(k in "\\PC*|[\\x00-\\x7f\\u{85}\\u{2028}\\u{2029}]*", v in proptest::prelude::any::<String>()) {
            let doc = vec![(None, k.clone(), map(vec![("x", s(v.as_str()))]))];
            let mut out = String::new();
            if emit_map(&mut out, &doc, 0, None).is_ok() {
                let lines: Vec<&str> = out.lines().collect();
                proptest::prop_assert_eq!(lines.len(), 2);
                let key = lines[0].strip_suffix(':').unwrap_or("");
                proptest::prop_assert_eq!(serde_json::from_str::<String>(key).ok(), Some(k));
                let val = lines[1].strip_prefix("  \"x\": ").unwrap_or("");
                proptest::prop_assert_eq!(serde_json::from_str::<String>(val).ok(), Some(v));
                let breaks = ['\u{85}', '\u{2028}', '\u{2029}', '$'];
                proptest::prop_assert!(!out.contains(breaks), "a line break or `$` was written");
            }
        }
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
            TemplateError::P2pLoopback,
            TemplateError::PortRemap,
            TemplateError::LanRefused,
            TemplateError::PublishOnExisting,
            TemplateError::DataFolderUnreadable,
            TemplateError::Unprintable,
            TemplateError::ShellUnsafe,
            TemplateError::ExistingName,
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
