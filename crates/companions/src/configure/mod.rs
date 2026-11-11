//! Configuring companions through their own APIs (plan
//! 2026-11-06-v06-companions §4 CMP-8b, Q8, Q19; architecture §18.7,
//! §18.10).
//!
//! - **[`qbittorrent`]:** the WebUI bound to loopback or the container's
//!   network, localhost sign-in required, the save path and categories, the
//!   network interface or bind address the egress route names, and, for a
//!   guided install, the temporary password replaced by the generated one.
//! - **[`prowlarr`]:** its SOCKS5 proxy, per the egress route.
//!
//! **Plan, then apply.** [`Configure::plan`] reads the companion's state
//! and lists the settings that differ from what this module wants, writing
//! nothing; `apply` writes only those. Every write is idempotent, so a plan
//! made after a successful apply is empty. Each applied setting is logged
//! by its name, never its value ([`CONFIG_APPLIED`]).
//!
//! **Consent.** Neither call sends anything unless the input states where
//! the companion came from ([`Origin`]): one Lodestar installed, or one you
//! confirmed for this exact target. [`ConfigureInput::new`] alone leaves it
//! [`Origin::Unstated`], which is refused, so a caller can't forget it.
//!
//! **Lodestar's own settings** (the Prowlarr plugin's address and key, the
//! qBittorrent entry in `integrations.clients`) go through [`ConfigSink`],
//! which `crates/lodestar` implements (CMP-5g). This crate depends on
//! neither `crates/pipeline` nor the settings code.
//!
//! **Answers are untrusted:** bounded by each request's [`Limits`], read
//! with the bounded JSON reader, compared and never kept, logged or shown.
//! Credentials travel only in a sensitive header or a request body, never
//! in a path or query.
//!
//! [`Limits`]: crate::adapter::Limits
//! [`CONFIG_APPLIED`]: crate::log_events::CONFIG_APPLIED

use std::net::{IpAddr, SocketAddr};

use lodestar_integrations::secret::Secret;

use crate::adapter::{
    AdapterError, BoxFuture, Configure, ConfigureInput, CookieName, HeaderName, HeaderValue,
    SettingKey, Target,
};
use crate::config_files::Username;
use crate::credentials::Field;
use crate::inputs::{CompanionId, HostPath, InputError};
use crate::log_events::{CONFIG_FAILED, CONFIG_HANDED_OVER};
pub use crate::windows_qbittorrent::WindowsPath;

pub mod prowlarr;
pub mod qbittorrent;

/// The refusal when the input doesn't establish consent for this target.
pub const NOT_CONFIRMED: &str = "not_confirmed_for_target";
/// The refusal when the companion can't follow the route's kind (Q8).
pub const ROUTE_UNSUPPORTED: &str = "route_unsupported";
/// The refusal when the WebUI binding doesn't suit the target, or the
/// companion isn't one Lodestar installed.
pub const BIND_UNSUITED: &str = "webui_bind_unsuited";
/// The refusal when a route would clear or replace a binding or proxy that
/// a companion you confirmed already has.
pub const WOULD_LOOSEN: &str = "would_loosen_companion_setting";
/// The refusal when a plan lists a setting its input can't supply.
pub const PLAN_MISMATCH: &str = "plan_does_not_match_input";
/// The refusal when a plan lists a setting the adapter doesn't know.
pub const UNKNOWN_SETTING: &str = "unknown_setting";
/// The refusal when a save path's kind doesn't match the companion's
/// platform: a Windows path for a qBittorrent that isn't Windows', or a
/// Linux path for Windows' (#705's review, I2). A Linux `/downloads` would
/// otherwise land on the current drive's root folder.
pub const SAVE_PATH_PLATFORM: &str = "save_path_wrong_platform";
/// The refusal when Lodestar's own settings didn't take the entry.
pub const SINK_REFUSED: &str = "sink_refused";

/// The configuration adapter for a catalogue entry, if there is one.
/// gluetun has none, and FlareSolverr is CMP-12's (Q21).
pub fn configurator(id: &CompanionId) -> Option<Box<dyn Configure>> {
    match id.as_str() {
        qbittorrent::ID => Some(Box::new(qbittorrent::QBittorrent::new(id.clone()))),
        prowlarr::ID => Some(Box::new(prowlarr::Prowlarr::new(id.clone()))),
        _ => None,
    }
}

/// You agreed to Lodestar configuring the companion at this target. Bound
/// to the target, so a confirmation for one can't be spent on another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirmation {
    target: Target,
}

impl Confirmation {
    /// For the target you were shown and confirmed (CMP-5g's endpoint).
    pub fn of(target: Target) -> Confirmation {
        Confirmation { target }
    }
}

/// Proof that Lodestar installed the companion at this target. Only this
/// crate mints one (CMP-5's manager, from its own state row), so no caller
/// can claim an install it didn't make: a detected companion on a fixed
/// target needs your [`Confirmation`] instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReceipt {
    target: Target,
}

impl InstallReceipt {
    /// For CMP-5's manager, after it installed the companion at `target`.
    pub(crate) fn new(target: Target) -> InstallReceipt {
        InstallReceipt { target }
    }

    /// For other crates' and this crate's `tests/`. Only under the
    /// `testing` feature, which only dev-dependencies enable.
    #[cfg(feature = "testing")]
    pub fn for_tests(target: Target) -> InstallReceipt {
        InstallReceipt::new(target)
    }
}

/// Where the companion came from, which decides whether Lodestar may
/// configure it.
#[derive(Debug, Clone, Default)]
pub enum Origin {
    /// Not stated: every call is refused before anything is sent.
    #[default]
    Unstated,
    /// Lodestar installed it, on loopback or as a container.
    Installed(InstallReceipt),
    /// Detected or entered, and you confirmed it.
    Confirmed(Confirmation),
    /// A guided install you confirmed (Q19): Lodestar signs in with the
    /// temporary password you pasted and replaces it with the generated one
    /// it holds as the companion's credential.
    Guided {
        confirmation: Confirmation,
        temporary: Secret,
    },
}

/// Checks consent before anything is sent: an install receipt or a
/// confirmation must name this target, and an installed companion is never
/// at a URL you entered.
pub(crate) fn consent(input: &ConfigureInput<'_>, target: &Target) -> Result<(), AdapterError> {
    let ok = match &input.origin {
        Origin::Unstated => false,
        Origin::Installed(r) => r.target == *target && !matches!(target, Target::Entered(_)),
        Origin::Confirmed(c)
        | Origin::Guided {
            confirmation: c, ..
        } => c.target == *target,
    };
    if ok {
        Ok(())
    } else {
        Err(AdapterError::Refused(NOT_CONFIRMED))
    }
}

/// The longest network interface name accepted, in bytes.
pub const INTERFACE_MAX: usize = 64;

/// A network interface's name (`tun0`, `wg0`): 1–64 of
/// `A–Z a–z 0–9 _ . : -`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceName(String);

impl InterfaceName {
    pub fn parse(s: &str) -> Result<InterfaceName, InputError> {
        if s.is_empty() || s.len() > INTERFACE_MAX {
            return Err(InputError::Length);
        }
        if !s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
        {
            return Err(InputError::Character);
        }
        Ok(InterfaceName(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The egress route a companion follows (Q18), in this crate's own closed
/// form: CMP-5 maps CMP-4b's route onto it. A companion that can't follow
/// a route's kind refuses it ([`ROUTE_UNSUPPORTED`]), never falls back to
/// the direct route (Q8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Direct,
    Interface(InterfaceName),
    BindAddress(BindIp),
    Socks5(Socks5Proxy),
}

/// An address to bind to: not unspecified, multicast or broadcast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BindIp(IpAddr);

impl BindIp {
    pub fn new(ip: IpAddr) -> Result<BindIp, InputError> {
        let broadcast = matches!(ip, IpAddr::V4(v4) if v4.is_broadcast());
        if ip.is_unspecified() || ip.is_multicast() || broadcast {
            return Err(InputError::Range);
        }
        Ok(BindIp(ip))
    }

    pub fn get(&self) -> IpAddr {
        self.0
    }
}

/// A SOCKS5 proxy's address: a specific address and a port other than 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Socks5Proxy(SocketAddr);

impl Socks5Proxy {
    pub fn new(addr: SocketAddr) -> Result<Socks5Proxy, InputError> {
        BindIp::new(addr.ip())?;
        if addr.port() == 0 {
            return Err(InputError::Range);
        }
        Ok(Socks5Proxy(addr))
    }

    pub fn get(&self) -> SocketAddr {
        self.0
    }
}

/// Where a companion's WebUI listens. Set only on a companion Lodestar
/// installed ([`Origin::Installed`]); for one you confirmed it's refused
/// ([`BIND_UNSUITED`]), because Lodestar can't see how its container is
/// networked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebUiBind {
    /// `127.0.0.1`: only this machine. For a loopback target only.
    Loopback,
    /// `*`: every address in the companion's network namespace. For a
    /// container target only. What can reach it is then decided by how the
    /// container is networked, which CMP-7 generates, not by this setting:
    /// Docker's network, and also anything that reaches a published port,
    /// host networking, or gluetun's namespace under `service:gluetun`.
    ContainerNetwork,
}

impl WebUiBind {
    /// Whether this binding keeps the target reachable: loopback for a
    /// loopback target, the container's network for a container. A URL
    /// you entered may be another machine, so neither is set there.
    pub(crate) fn suits(self, target: &Target) -> bool {
        matches!(
            (self, target),
            (WebUiBind::Loopback, Target::Loopback(_))
                | (WebUiBind::ContainerNetwork, Target::Container(..))
        )
    }
}

/// The longest category name accepted, in bytes.
pub const CATEGORY_MAX: usize = 64;
/// The most categories one configuration sets.
pub const CATEGORIES_MAX: usize = 16;

/// A download client category (`lodestar`): 1–64 of `A–Z a–z 0–9 _ . -`
/// and inner spaces, not starting with `.`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CategoryName(String);

impl CategoryName {
    pub fn parse(s: &str) -> Result<CategoryName, InputError> {
        if s.is_empty() || s.len() > CATEGORY_MAX {
            return Err(InputError::Length);
        }
        let b = s.as_bytes();
        if !b
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-' | b' '))
            || b[0] == b' '
            || b[0] == b'.'
            || b[b.len() - 1] == b' '
        {
            return Err(InputError::Character);
        }
        Ok(CategoryName(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One category and its save path; `None` uses the client's default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Category {
    pub name: CategoryName,
    pub save_path: Option<HostPath>,
}

/// A download client's save path: a Linux path (in its container, or
/// a native Linux install) or, for CMP-11's qBittorrent on Windows, a
/// [`WindowsPath`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SavePath {
    Host(HostPath),
    Windows(WindowsPath),
}

impl SavePath {
    pub fn as_str(&self) -> &str {
        match self {
            SavePath::Host(p) => p.as_str(),
            SavePath::Windows(p) => p.as_str(),
        }
    }
}

impl From<HostPath> for SavePath {
    fn from(p: HostPath) -> SavePath {
        SavePath::Host(p)
    }
}

impl From<WindowsPath> for SavePath {
    fn from(p: WindowsPath) -> SavePath {
        SavePath::Windows(p)
    }
}

/// The download client's save path and categories. Paths are as the
/// companion sees them (`/downloads` inside its container, `D:\Downloads`
/// on Windows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downloads {
    save_path: SavePath,
    categories: Vec<Category>,
}

impl Downloads {
    /// At most [`CATEGORIES_MAX`] categories, each name once. With a
    /// Windows save path a category has no path of its own (a Linux one
    /// would mean nothing there, [`InputError::NotAbsolute`]): qBittorrent
    /// then saves it in a folder of its name under the save path.
    pub fn new(
        save_path: impl Into<SavePath>,
        categories: Vec<Category>,
    ) -> Result<Downloads, InputError> {
        let save_path = save_path.into();
        if categories.len() > CATEGORIES_MAX {
            return Err(InputError::Length);
        }
        if matches!(save_path, SavePath::Windows(_))
            && categories.iter().any(|c| c.save_path.is_some())
        {
            return Err(InputError::NotAbsolute);
        }
        for (i, c) in categories.iter().enumerate() {
            if categories[..i].iter().any(|d| d.name == c.name) {
                return Err(InputError::Reserved);
            }
        }
        Ok(Downloads {
            save_path,
            categories,
        })
    }

    pub fn save_path(&self) -> &SavePath {
        &self.save_path
    }

    pub fn categories(&self) -> &[Category] {
        &self.categories
    }
}

/// Lodestar's own settings, written for a configured companion. Implemented
/// by `crates/lodestar` (CMP-5g) over its settings registry; this crate only
/// calls it, from [`hand_over`].
///
/// Every value is a closed type Lodestar chose or holds: the target it
/// configured, the credential it generated or you pasted, a category from
/// its input. Nothing a companion sent reaches it.
pub trait ConfigSink: Send + Sync {
    /// The qBittorrent entry in `integrations.clients`. The sink seals the
    /// password under its own context.
    fn download_client<'a>(
        &'a self,
        entry: DownloadClientEntry,
    ) -> BoxFuture<'a, Result<(), SinkError>>;

    /// The Prowlarr plugin's address and key. The sink seals the key under
    /// the plugin's own context.
    fn indexer_manager<'a>(
        &'a self,
        entry: IndexerManagerEntry,
    ) -> BoxFuture<'a, Result<(), SinkError>>;
}

/// A download client entry for Lodestar's settings.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DownloadClientEntry {
    pub companion: CompanionId,
    pub target: Target,
    pub username: Username,
    pub password: Secret,
    /// The first configured category, which Lodestar adds torrents under.
    pub category: Option<CategoryName>,
}

/// The Prowlarr plugin's settings.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct IndexerManagerEntry {
    pub companion: CompanionId,
    pub target: Target,
    pub api_key: Secret,
}

/// Why the sink refused an entry. Carries no value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SinkError {
    /// The settings refused the value.
    Rejected,
    /// The settings couldn't be written.
    Unavailable,
}

/// Hands a configured companion to Lodestar's own settings, after a
/// successful `apply`: qBittorrent as a download client, Prowlarr as the
/// plugin's indexer manager. Consent is checked as for configuring; the
/// credential comes from the input, never from a companion's answer.
pub fn hand_over<'a>(
    sink: &'a dyn ConfigSink,
    companion: &'a CompanionId,
    target: &'a Target,
    input: &'a ConfigureInput<'a>,
) -> BoxFuture<'a, Result<(), AdapterError>> {
    Box::pin(async move {
        consent(input, target)?;
        let (id, entry, result) = match companion.as_str() {
            qbittorrent::ID => {
                let password = credential(input, Field::WebUiPassword)?;
                let entry = DownloadClientEntry {
                    companion: companion.clone(),
                    target: target.clone(),
                    username: input.username.clone(),
                    password,
                    category: input
                        .downloads
                        .as_ref()
                        .and_then(|d| d.categories.first())
                        .map(|c| c.name.clone()),
                };
                (
                    qbittorrent::ID,
                    "download_client",
                    sink.download_client(entry).await,
                )
            }
            prowlarr::ID => {
                let api_key = credential(input, Field::ApiKey)?;
                let entry = IndexerManagerEntry {
                    companion: companion.clone(),
                    target: target.clone(),
                    api_key,
                };
                (
                    prowlarr::ID,
                    "indexer_manager",
                    sink.indexer_manager(entry).await,
                )
            }
            _ => return Err(AdapterError::Refused("no configuration adapter")),
        };
        match result {
            Ok(()) => {
                lodestar_log::log_event!(CONFIG_HANDED_OVER, companion = id, entry = entry);
                Ok(())
            }
            Err(_) => Err(failed(
                id,
                "lodestar.settings",
                AdapterError::Refused(SINK_REFUSED),
            )),
        }
    })
}

/// The stored credential for `field`: none is [`AdapterError::Unauthorized`],
/// a broken store [`AdapterError::Credentials`].
pub(crate) fn credential(input: &ConfigureInput<'_>, field: Field) -> Result<Secret, AdapterError> {
    input
        .credentials
        .get(field.as_str())?
        .ok_or(AdapterError::Unauthorized)
}

/// An adapter's own setting key; the constants are checked by tests.
pub(crate) const fn key(name: &'static str) -> SettingKey {
    match SettingKey::new(name) {
        Some(k) => k,
        None => panic!("setting key outside the grammar"),
    }
}

pub(crate) const fn header(name: &'static str) -> HeaderName {
    match HeaderName::new(name) {
        Some(h) => h,
        None => panic!("header name outside the grammar"),
    }
}

pub(crate) const fn cookie(c: Option<CookieName>) -> CookieName {
    match c {
        Some(c) => c,
        None => panic!("cookie name outside the grammar"),
    }
}

pub(crate) const CONTENT_TYPE: HeaderName = header("Content-Type");

/// A plain header value from an adapter's constant.
pub(crate) fn plain(value: &str) -> Result<HeaderValue, AdapterError> {
    HeaderValue::plain(value).map_err(|_| AdapterError::Protocol("header value"))
}

/// `application/x-www-form-urlencoded` for one value: everything but
/// RFC 3986's unreserved characters is escaped, so no value can add a
/// field or end one.
pub(crate) fn form_value(out: &mut Vec<u8>, value: &str) {
    for &b in value.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b);
        } else {
            out.extend_from_slice(format!("%{b:02X}").as_bytes());
        }
    }
}

/// A form body from fixed field names and values.
pub(crate) fn form(fields: &[(&str, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, (name, value)) in fields.iter().enumerate() {
        if i > 0 {
            out.push(b'&');
        }
        form_value(&mut out, name);
        out.push(b'=');
        form_value(&mut out, value);
    }
    out
}

/// A failure's code for the log: the error's own words, never response
/// text.
pub(crate) fn code(e: &AdapterError) -> &'static str {
    match e {
        AdapterError::Timeout => "timeout",
        AdapterError::Unreachable => "unreachable",
        AdapterError::Blocked => "blocked",
        AdapterError::TooLarge => "too_large",
        AdapterError::NotThisCompanion => "not_this_companion",
        AdapterError::Unauthorized => "unauthorized",
        AdapterError::Credentials => "credentials",
        AdapterError::Protocol(w) | AdapterError::Refused(w) => w,
    }
}

/// Logs a failed step by the setting's name and the error's code, and
/// hands the error back.
pub(crate) fn failed(companion: &'static str, step: &'static str, e: AdapterError) -> AdapterError {
    lodestar_log::log_event!(
        CONFIG_FAILED,
        companion = companion,
        setting = step,
        reason = code(&e)
    );
    e
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::EnteredUrl;
    use crate::inputs::PortNumber;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn closed_inputs() {
        assert!(InterfaceName::parse("tun0").is_ok());
        assert!(InterfaceName::parse("wg-vpn.1:a").is_ok());
        for bad in ["", "a b", "eth0\n", "é", "a/b"] {
            assert!(InterfaceName::parse(bad).is_err(), "{bad:?}");
        }
        assert!(InterfaceName::parse(&"a".repeat(65)).is_err());

        assert!(BindIp::new(IpAddr::V4(Ipv4Addr::new(10, 8, 0, 2))).is_ok());
        for bad in [
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            IpAddr::V4(Ipv4Addr::BROADCAST),
            IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1)),
        ] {
            assert_eq!(BindIp::new(bad), Err(InputError::Range));
        }
        assert!(Socks5Proxy::new("172.17.0.2:1080".parse().unwrap()).is_ok());
        assert!(Socks5Proxy::new("172.17.0.2:0".parse().unwrap()).is_err());
        assert!(Socks5Proxy::new("0.0.0.0:1080".parse().unwrap()).is_err());

        assert!(CategoryName::parse("lodestar").is_ok());
        assert!(CategoryName::parse("TV shows-2.x").is_ok());
        for bad in ["", " a", "a ", ".hidden", "a/b", "a\\b", "a\tb", "é"] {
            assert!(CategoryName::parse(bad).is_err(), "{bad:?}");
        }
        let cat = |n: &str| Category {
            name: CategoryName::parse(n).unwrap(),
            save_path: None,
        };
        let path = HostPath::parse("/downloads").unwrap();
        assert!(Downloads::new(path.clone(), vec![cat("a"), cat("b")]).is_ok());
        assert_eq!(
            Downloads::new(path.clone(), vec![cat("a"), cat("a")]),
            Err(InputError::Reserved)
        );
        let many = (0..=CATEGORIES_MAX)
            .map(|i| cat(&format!("c{i}")))
            .collect();
        assert_eq!(Downloads::new(path, many), Err(InputError::Length));
    }

    #[test]
    fn webui_binding_suits_only_its_target() {
        let lo = Target::Loopback(PortNumber::new(8080).unwrap());
        let ct = Target::Container(
            crate::inputs::ContainerName::parse("qbittorrent").unwrap(),
            PortNumber::new(8080).unwrap(),
        );
        let en = Target::Entered(EnteredUrl::parse("http://10.0.0.2:8080").unwrap());
        assert!(WebUiBind::Loopback.suits(&lo));
        assert!(!WebUiBind::Loopback.suits(&ct));
        assert!(!WebUiBind::Loopback.suits(&en));
        assert!(WebUiBind::ContainerNetwork.suits(&ct));
        assert!(!WebUiBind::ContainerNetwork.suits(&lo));
        assert!(!WebUiBind::ContainerNetwork.suits(&en));
    }

    #[test]
    fn form_encoding_escapes_everything_but_unreserved() {
        assert_eq!(
            form(&[("username", "a b"), ("password", "x&y=z%+\r\n")]),
            b"username=a%20b&password=x%26y%3Dz%25%2B%0D%0A".to_vec()
        );
        assert_eq!(form(&[("json", "{\"a\":1}")]), b"json=%7B%22a%22%3A1%7D");
    }

    #[test]
    fn codes_are_the_errors_own_words() {
        assert_eq!(code(&AdapterError::Refused(NOT_CONFIRMED)), NOT_CONFIRMED);
        assert_eq!(code(&AdapterError::Unauthorized), "unauthorized");
        assert_eq!(code(&AdapterError::Protocol("x")), "x");
    }

    #[test]
    fn every_registered_adapter_is_in_the_catalogue() {
        let c = crate::builtin().unwrap();
        for id in [qbittorrent::ID, prowlarr::ID] {
            let id = &c.get(id).unwrap().id;
            assert_eq!(configurator(id).unwrap().companion(), id);
        }
        for other in ["gluetun", "flaresolverr"] {
            assert!(configurator(&CompanionId::parse(other).unwrap()).is_none());
        }
    }
}
