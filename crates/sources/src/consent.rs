//! The consent framework (architecture §18.7): what enabling a source
//! requires, the consent records that say it was given, and the
//! per-source credential store.
//!
//! - **Class gating.** [`need`] says what stands between a source and
//!   being enabled:
//!   - **None:** nothing; enabling is a toggle.
//!   - **External:** the endpoint list, acknowledged ([`Need::Endpoints`]).
//!   - **Account:** its declared required credentials, stored
//!     ([`Need::Credentials`], [`need_with`]).
//!   - **Restricted** and **Overlay:** the consent screen, acknowledged
//!     with the admin password ([`Need::Consent`], [`needs_password`]).
//!
//!   The server checks it on every path that can turn a source on: the
//!   API's enable, and every start (a source the store has as enabled
//!   whose condition no longer holds is switched off before it runs).
//! - **Consent records** ([`Ack`]) hold the class, the version of what
//!   was shown and a boolean, with no time and no author (Q14, §1.8.8).
//!   The version ([`version`]) holds the class's consent text version
//!   ([`text_version`]) and the full address list ([`addresses`]), and is
//!   compared exactly, so a new text, a plugin update that contacts new
//!   hosts, or settings that list another address ask again.
//! - **Credentials** ([`SourceCredentials`]) are sealed with the
//!   integrations `SecretStore`, under a context of their own per source
//!   and name ([`context`]), so a sealed value copied onto another source
//!   or name doesn't open. They're never returned by the API, logged, put
//!   in an error or exported, and purging the source drops them. The
//!   store implements SRC-5's [`EgressCredentials`] for SOCKS5 routes.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lodestar_integrations::SealedSecret;
use lodestar_integrations::net::SocksAuth;
pub use lodestar_integrations::{Secret, SecretStore};
use lodestar_store::{SourceState, Store};

use crate::egress::{CredentialError, EgressCredentials};
use crate::manifest::{Consent, Manifest};
pub use crate::manifest::{CredentialKind, CredentialSpec};
use crate::plugin::BoxFuture;

/// The current version of each class's consent text. Raise a class's
/// number whenever its text changes in substance: every source of that
/// class asks again before it's next enabled, and one already enabled is
/// switched off at the next start until it's acknowledged again.
///
/// External, Restricted and Overlay are at 2: the owner-approved texts
/// (§2.49) as SRC-15b's dialogs show them. Version 1 was SRC-4's draft,
/// which a build before the consent gate could record through the API
/// with no dialog showing it, so a record of it covers nothing now and
/// the source asks again (X-consent-gate-lift). Account is 0: its consent
/// is its credentials, never an acknowledgement, until X-consent-gate-lift
/// r2 ships its dialog.
pub const fn text_version(class: Consent) -> u32 {
    match class {
        Consent::None => 0,
        Consent::External => 2,
        Consent::Account => 0,
        Consent::Restricted => 2,
        Consent::Overlay => 2,
    }
}

/// What enabling a source still needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// External: the endpoint list, acknowledged.
    Endpoints,
    /// Account: credentials, stored.
    Credentials,
    /// Restricted or Overlay: the consent screen, acknowledged with the
    /// password.
    Consent,
}

/// Whether the class is enabled by acknowledging what's shown.
pub const fn needs_ack(class: Consent) -> bool {
    matches!(
        class,
        Consent::External | Consent::Restricted | Consent::Overlay
    )
}

/// Whether accepting the class's consent screen asks for the admin
/// password (Q13): Restricted and Overlay do.
pub const fn needs_password(class: Consent) -> bool {
    matches!(class, Consent::Restricted | Consent::Overlay)
}

/// Whether the class needs stored credentials.
pub const fn needs_credentials(class: Consent) -> bool {
    matches!(class, Consent::Account)
}

/// The version an acknowledgement of `manifest` is bound to, and what
/// `PATCH /sources/{id}` must name: the class's text version and every
/// address the source contacts ([`addresses`]), exactly. Empty for a
/// class that's never acknowledged.
///
/// The form is `t<text version>` and then ` <length>:<address>` for each
/// address, sorted and once each. Each address is prefixed by its length
/// in bytes, so the text is injective: two versions are equal only when
/// the text version and the address lists are (SRC-13c,
/// `docs/decisions/2026-11-05-SRC-13c.md`). An endpoint list can come from
/// the user's settings (a torrent cache's), so a digest that only notices
/// a change isn't enough: a list chosen to collide would be covered by an
/// acknowledgement of another one. A record written before this form (SRC-4's
/// `<text version>-<FNV-1a digest>`) never equals it, so it counts as not
/// acknowledged and the source asks again (fail closed).
pub fn version(manifest: &Manifest) -> String {
    use std::fmt::Write as _;
    if !needs_ack(manifest.consent) {
        return String::new();
    }
    let mut out = format!("t{}", text_version(manifest.consent));
    for a in addresses(manifest) {
        let _ = write!(out, " {}:{a}", a.len());
    }
    out
}

/// What the source contacts, as the consent dialog shows it and an
/// acknowledgement covers: each endpoint as `protocol://address`, sorted
/// and once each. For a compiled-in manifest the address is a host; for
/// one built from the user's settings it can be a whole address template
/// (a torrent cache's `https://cache.example/~me/{hash}.torrent`), so
/// consent covers the path as well as the host.
pub fn addresses(manifest: &Manifest) -> Vec<String> {
    let mut list: Vec<String> = manifest
        .endpoints
        .iter()
        .map(|e| format!("{}://{}", e.protocol, e.host))
        .collect();
    list.sort_unstable();
    list.dedup();
    list
}

/// A consent record: the class acknowledged, the version shown and
/// whether it was accepted (Q14). Nothing else: no time, no author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ack {
    pub class: Consent,
    pub version: String,
    pub accepted: bool,
}

impl Ack {
    /// An accepted acknowledgement of `manifest` as it is now.
    pub fn of(manifest: &Manifest) -> Ack {
        Ack {
            class: manifest.consent,
            version: version(manifest),
            accepted: true,
        }
    }

    /// Whether this record covers `manifest` as it is now: an exact
    /// compare of what was shown ([`version`]) with what the source
    /// contacts now.
    pub fn covers(&self, manifest: &Manifest) -> bool {
        self.accepted && self.class == manifest.consent && self.version == version(manifest)
    }

    /// The stored form: `<class>|<version>|<0 or 1>`. The version can
    /// hold `|` (an address may), so it's read as everything between the
    /// first and the last.
    pub fn encode(&self) -> String {
        format!(
            "{}|{}|{}",
            class_name(self.class),
            self.version,
            u8::from(self.accepted)
        )
    }

    /// `None` for a record this build can't read; it counts as no record,
    /// so the source asks again.
    pub fn decode(text: &str) -> Option<Ack> {
        let (class, rest) = text.split_once('|')?;
        let (version, accepted) = rest.rsplit_once('|')?;
        Some(Ack {
            class: class_from_name(class)?,
            version: version.to_string(),
            accepted: match accepted {
                "0" => false,
                "1" => true,
                _ => return None,
            },
        })
    }
}

pub const fn class_name(c: Consent) -> &'static str {
    match c {
        Consent::None => "none",
        Consent::External => "external",
        Consent::Account => "account",
        Consent::Restricted => "restricted",
        Consent::Overlay => "overlay",
    }
}

fn class_from_name(s: &str) -> Option<Consent> {
    Some(match s {
        "none" => Consent::None,
        "external" => Consent::External,
        "account" => Consent::Account,
        "restricted" => Consent::Restricted,
        "overlay" => Consent::Overlay,
        _ => return None,
    })
}

/// What enabling `manifest`'s source still needs, given its consent
/// record and the names of its stored credentials, for a plugin that
/// declares no credentials: [`need_with`] with an empty spec. A caller
/// that has the plugin's `Policy::credentials` passes them to
/// [`need_with`] instead.
pub fn need(manifest: &Manifest, ack: Option<&Ack>, credentials: &[String]) -> Option<Need> {
    need_with(manifest, &[], ack, credentials)
}

/// What enabling `manifest`'s source still needs, given its declared
/// credentials (`spec`, the plugin's `Policy::credentials`), its consent
/// record and the names of its stored credentials; `None` when its
/// class's condition holds.
///
/// An Account source's condition (X-consent-gate-lift step 1):
/// - **Required credentials declared:** every one of them is stored.
///   Another name stored, declared or not, never stands in for one.
/// - **Only optional ones declared:** at least one declared name is
///   stored. An undeclared name (a typo) doesn't count.
/// - **None declared:** at least one name that isn't an egress name is
///   stored, the rule before credential specs existed.
///
/// Egress names (`egress.socks5.*`) never count: they belong to the
/// route, not to the plugin, and [`check_spec`] refuses them in a spec.
pub fn need_with(
    manifest: &Manifest,
    spec: &[CredentialSpec],
    ack: Option<&Ack>,
    credentials: &[String],
) -> Option<Need> {
    let class = manifest.consent;
    if needs_credentials(class) && !credentials_held(spec, credentials) {
        return Some(Need::Credentials);
    }
    if needs_ack(class) && !ack.is_some_and(|a| a.covers(manifest)) {
        return Some(if needs_password(class) {
            Need::Consent
        } else {
            Need::Endpoints
        });
    }
    None
}

/// Whether `stored` satisfies an Account source declaring `spec`
/// ([`need_with`]).
fn credentials_held(spec: &[CredentialSpec], stored: &[String]) -> bool {
    let has = |name: &str| !is_egress_name(name) && stored.iter().any(|n| n == name);
    let mut required = spec.iter().filter(|c| c.required).peekable();
    if required.peek().is_some() {
        required.all(|c| has(c.name))
    } else if !spec.is_empty() {
        spec.iter().any(|c| has(c.name))
    } else {
        stored.iter().any(|n| !is_egress_name(n))
    }
}

/// [`need`], read from the store (blocking), for a plugin that declares
/// no credentials.
pub fn need_in(store: &Store, manifest: &Manifest) -> Result<Option<Need>, lodestar_store::Error> {
    need_in_with(store, manifest, &[])
}

/// [`need_with`], read from the store (blocking).
pub fn need_in_with(
    store: &Store,
    manifest: &Manifest,
    spec: &[CredentialSpec],
) -> Result<Option<Need>, lodestar_store::Error> {
    let ack = store
        .source_consent(manifest.id)?
        .as_deref()
        .and_then(Ack::decode);
    let names = store.source_secret_names(manifest.id)?;
    Ok(need_with(manifest, spec, ack.as_ref(), &names))
}

/// The SOCKS5 user name's credential name.
pub const SOCKS5_USERNAME: &str = "egress.socks5.username";
/// The SOCKS5 password's credential name.
pub const SOCKS5_PASSWORD: &str = "egress.socks5.password";

/// The longest credential name.
pub const MAX_NAME: usize = 64;
/// The longest credential value, in bytes.
pub const MAX_VALUE: usize = 4096;
/// The most credentials one source may hold.
pub const MAX_CREDENTIALS: usize = 16;

fn is_egress_name(name: &str) -> bool {
    name == SOCKS5_USERNAME || name == SOCKS5_PASSWORD
}

/// Why a plugin's declared credentials ([`CredentialSpec`]) are wrong.
/// A test failure, never a user's: the spec is compiled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecError {
    /// The name can't be stored: [`check_name`] refuses it.
    Name(&'static str),
    /// The name is one of the egress names (`egress.socks5.*`), which
    /// belong to the egress setting, not to the plugin.
    Egress(&'static str),
    /// Declared twice.
    Repeated(&'static str),
    /// No label to show.
    Label(&'static str),
    /// More than [`MAX_CREDENTIALS`] declared.
    TooMany,
    /// An Account source whose required credentials, all stored, still
    /// leave [`need_with`] asking for credentials.
    NotEnough,
}

/// Checks `spec` against what the credential store takes and what
/// [`need_with`] accepts for `manifest`: each name passes [`check_name`], isn't
/// an egress name and is declared once with a label, and for an Account
/// source, storing every required one satisfies [`need_with`].
pub fn check_spec(manifest: &Manifest, spec: &[CredentialSpec]) -> Result<(), SpecError> {
    if spec.len() > MAX_CREDENTIALS {
        return Err(SpecError::TooMany);
    }
    for (i, c) in spec.iter().enumerate() {
        if is_egress_name(c.name) {
            return Err(SpecError::Egress(c.name));
        }
        if check_name(c.name).is_err() {
            return Err(SpecError::Name(c.name));
        }
        if spec[..i].iter().any(|o| o.name == c.name) {
            return Err(SpecError::Repeated(c.name));
        }
        if c.label.trim().is_empty() {
            return Err(SpecError::Label(c.name));
        }
    }
    if needs_credentials(manifest.consent) && spec.iter().any(|c| c.required) {
        let names: Vec<String> = spec
            .iter()
            .filter(|c| c.required)
            .map(|c| c.name.to_string())
            .collect();
        if need_with(manifest, spec, Some(&Ack::of(manifest)), &names) == Some(Need::Credentials) {
            return Err(SpecError::NotEnough);
        }
    }
    Ok(())
}

/// Why a credential change was refused. Carries no value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialRefusal {
    /// A name isn't 1–64 of `a-z`, `0-9` and `_`, or one of the two
    /// SOCKS5 names.
    Name,
    /// A value is empty or longer than [`MAX_VALUE`] bytes.
    Value,
    /// More than [`MAX_CREDENTIALS`] would be stored.
    TooMany,
}

impl std::fmt::Display for CredentialRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            CredentialRefusal::Name => {
                "a credential name is 1 to 64 lowercase letters, digits or underscores"
            }
            CredentialRefusal::Value => {
                "a credential value is 1 to 4096 bytes, and a SOCKS5 user name or password 1 to 255"
            }
            CredentialRefusal::TooMany => "a source holds at most 16 credentials",
        })
    }
}

/// Checks a credential's name.
pub fn check_name(name: &str) -> Result<(), CredentialRefusal> {
    let plain = !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if plain || is_egress_name(name) {
        Ok(())
    } else {
        Err(CredentialRefusal::Name)
    }
}

/// The longest SOCKS5 user name or password (RFC 1929).
pub const MAX_SOCKS5_VALUE: usize = 255;

/// Checks a credential's value: 1–[`MAX_VALUE`] bytes, or
/// 1–[`MAX_SOCKS5_VALUE`] for the SOCKS5 pair, which RFC 1929 limits and
/// SRC-5's route would otherwise refuse only when it's built.
pub fn check_value(name: &str, value: &Secret) -> Result<(), CredentialRefusal> {
    let max = if is_egress_name(name) {
        MAX_SOCKS5_VALUE
    } else {
        MAX_VALUE
    };
    if value.is_empty() || value.expose().len() > max {
        Err(CredentialRefusal::Value)
    } else {
        Ok(())
    }
}

/// The `SecretStore` context a credential is sealed under: one per source
/// and name, so a sealed value doesn't open anywhere else.
pub fn context(plugin: &str, name: &str) -> String {
    format!("source:{plugin}:credential:{name}")
}

/// Where the `SecretStore` comes from.
enum Keys {
    /// `secrets.key` in the data directory, opened on first use, so an
    /// install with no credentials never creates it.
    Dir(PathBuf, Mutex<Option<Arc<SecretStore>>>),
    Fixed(Arc<SecretStore>),
}

struct Inner {
    store: Arc<Mutex<Store>>,
    /// Where [`EgressCredentials::source_enabled`] reads the source's
    /// state: `store` unless [`SourceCredentials::reading_states_from`]
    /// gave it a connection of its own (X-egress-state-watch).
    states: Arc<Mutex<Store>>,
    keys: Keys,
}

/// The per-source credential store: sealed values in the store's
/// database, the key outside it. Cheap to clone.
#[derive(Clone)]
pub struct SourceCredentials {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for SourceCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourceCredentials")
    }
}

impl SourceCredentials {
    /// Over the install's key file in `data_dir`. Source states are read
    /// from `store` too, until [`reading_states_from`](Self::reading_states_from)
    /// says otherwise.
    pub fn new(store: Arc<Mutex<Store>>, data_dir: PathBuf) -> SourceCredentials {
        SourceCredentials {
            inner: Arc::new(Inner {
                states: store.clone(),
                store,
                keys: Keys::Dir(data_dir, Mutex::new(None)),
            }),
        }
    }

    /// Over a key the caller holds (tests; workers, §19.6).
    pub fn with_keys(store: Arc<Mutex<Store>>, keys: Arc<SecretStore>) -> SourceCredentials {
        SourceCredentials {
            inner: Arc::new(Inner {
                states: store.clone(),
                store,
                keys: Keys::Fixed(keys),
            }),
        }
    }

    /// Reads source states for egress from `reader`, a read-only
    /// connection to the same database ([`Store::open_reader`]) that only
    /// this check uses, so a long API or Control operation holding the
    /// shared store's lock can't hold back a plugin's requests
    /// (X-egress-state-watch). The reader takes a fresh WAL snapshot for
    /// every read, so a state any writer commits is seen by the next
    /// request: there's no second copy to keep in step. Call it before
    /// cloning: a clone of `self` keeps reading states under the shared
    /// store's lock.
    pub fn reading_states_from(self, reader: Arc<Mutex<Store>>) -> SourceCredentials {
        let keys = match &self.inner.keys {
            Keys::Fixed(k) => Keys::Fixed(k.clone()),
            // Opened again on first use, from the same file.
            Keys::Dir(dir, _) => Keys::Dir(dir.clone(), Mutex::new(None)),
        };
        SourceCredentials {
            inner: Arc::new(Inner {
                store: self.inner.store.clone(),
                states: reader,
                keys,
            }),
        }
    }

    fn keys(&self) -> Result<Arc<SecretStore>, CredentialError> {
        match &self.inner.keys {
            Keys::Fixed(k) => Ok(k.clone()),
            Keys::Dir(dir, slot) => {
                let mut slot = slot.lock().map_err(|_| CredentialError)?;
                if let Some(k) = &*slot {
                    return Ok(k.clone());
                }
                let k = Arc::new(SecretStore::open_or_create(dir).map_err(|_| CredentialError)?);
                *slot = Some(k.clone());
                Ok(k)
            }
        }
    }

    /// `value` sealed for `plugin`'s credential `name`, as stored
    /// (blocking: may create the key file).
    pub fn seal(
        &self,
        plugin: &str,
        name: &str,
        value: &Secret,
    ) -> Result<Vec<u8>, CredentialError> {
        let sealed = self
            .keys()?
            .seal(&context(plugin, name), value)
            .map_err(|_| CredentialError)?;
        Ok(sealed.as_bytes().to_vec())
    }

    /// `plugin`'s credential `name`, opened (blocking: reads the store).
    /// `Ok(None)` when none is stored; an error when one is stored but
    /// doesn't open (the key file was lost or replaced).
    pub fn get(&self, plugin: &str, name: &str) -> Result<Option<Secret>, CredentialError> {
        let sealed = self
            .inner
            .store
            .lock()
            .map_err(|_| CredentialError)?
            .source_secret(plugin, name)
            .map_err(|_| CredentialError)?;
        let Some(sealed) = sealed else {
            return Ok(None);
        };
        let sealed = SealedSecret::from_bytes(sealed).map_err(|_| CredentialError)?;
        self.keys()?
            .open(&context(plugin, name), &sealed)
            .map(Some)
            .map_err(|_| CredentialError)
    }

    /// The SOCKS5 credentials (blocking).
    pub fn socks5_now(&self, plugin: &str) -> Result<Option<SocksAuth>, CredentialError> {
        match (
            self.get(plugin, SOCKS5_USERNAME)?,
            self.get(plugin, SOCKS5_PASSWORD)?,
        ) {
            (None, None) => Ok(None),
            (Some(username), Some(password)) => Ok(Some(SocksAuth { username, password })),
            // Half a pair: the route stays down rather than trying the
            // proxy without what the user meant it to have.
            _ => Err(CredentialError),
        }
    }
}

impl EgressCredentials for SourceCredentials {
    fn socks5<'a>(
        &'a self,
        source: &'a str,
    ) -> BoxFuture<'a, Result<Option<SocksAuth>, CredentialError>> {
        let this = self.clone();
        let source = source.to_string();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || this.socks5_now(&source))
                .await
                .map_err(|_| CredentialError)?
        })
    }

    /// The source's row in the store, read now on the states connection:
    /// Enabled or not. A missing row or a read error is never Enabled.
    fn source_enabled<'a>(
        &'a self,
        source: &'a str,
    ) -> BoxFuture<'a, Result<bool, CredentialError>> {
        let this = self.clone();
        let source = source.to_string();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let rows = this
                    .inner
                    .states
                    .lock()
                    .map_err(|_| CredentialError)?
                    .sources()
                    .map_err(|_| CredentialError)?;
                Ok(rows
                    .iter()
                    .any(|r| r.plugin == source && r.state == SourceState::Enabled))
            })
            .await
            .map_err(|_| CredentialError)?
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Endpoint, Kind, Network, Priority, Risk};
    use lodestar_store::{SourceId, SourceState, StoreConfig};

    const ENDPOINTS: &[Endpoint] = &[
        Endpoint {
            protocol: "https",
            host: "a.example",
        },
        Endpoint {
            protocol: "https",
            host: "b.example",
        },
    ];

    const REORDERED: &[Endpoint] = &[ENDPOINTS[1], ENDPOINTS[0], ENDPOINTS[1]];

    fn manifest(consent: Consent) -> Manifest {
        Manifest {
            id: "probe",
            name: "Probe",
            version: "1",
            kinds: &[Kind::Discovery],
            networks: &[Network::Bt],
            endpoints: ENDPOINTS,
            risk: Risk::High,
            consent,
            default_priority: Priority::Normal,
            enabled_by_default: false,
            always_adult: false,
            finds_private: false,
        }
    }

    const ALL: [Consent; 5] = [
        Consent::None,
        Consent::External,
        Consent::Account,
        Consent::Restricted,
        Consent::Overlay,
    ];

    #[test]
    fn each_class_needs_its_own_condition() {
        let none: &[String] = &[];
        let want = [
            None,
            Some(Need::Endpoints),
            Some(Need::Credentials),
            Some(Need::Consent),
            Some(Need::Consent),
        ];
        for (class, want) in ALL.into_iter().zip(want) {
            let m = manifest(class);
            assert_eq!(need(&m, None, none), want, "{class:?}");
            // Its own acknowledgement satisfies an ack class, and nothing
            // else's does.
            if needs_ack(class) {
                assert_eq!(need(&m, Some(&Ack::of(&m)), none), None);
                for other in ALL.into_iter().filter(|c| *c != class) {
                    let wrong = Ack {
                        class: other,
                        ..Ack::of(&m)
                    };
                    assert_eq!(need(&m, Some(&wrong), none), want, "{class:?} by {other:?}");
                }
                let declined = Ack {
                    accepted: false,
                    ..Ack::of(&m)
                };
                assert_eq!(need(&m, Some(&declined), none), want);
            }
        }
        // Account: a plugin credential, not an egress one.
        let m = manifest(Consent::Account);
        let socks = [SOCKS5_USERNAME.to_string(), SOCKS5_PASSWORD.to_string()];
        assert_eq!(need(&m, None, &socks), Some(Need::Credentials));
        assert_eq!(need(&m, None, &["api_key".to_string()]), None);
        // An acknowledgement doesn't stand in for credentials.
        assert_eq!(
            need(&m, Some(&Ack::of(&m)), &socks),
            Some(Need::Credentials)
        );
    }

    #[test]
    fn a_new_text_or_endpoint_list_asks_again() {
        let m = manifest(Consent::Restricted);
        let ack = Ack::of(&m);
        let older = Ack {
            version: ack.version.replacen("t2 ", "t1 ", 1),
            ..ack.clone()
        };
        assert_ne!(older.version, ack.version);
        assert_eq!(need(&m, Some(&older), &[]), Some(Need::Consent));
        let moved = Manifest {
            endpoints: &ENDPOINTS[..1],
            ..m.clone()
        };
        assert_eq!(need(&moved, Some(&ack), &[]), Some(Need::Consent));
        // Order and repeats don't count as a change.
        let reordered = Manifest {
            endpoints: REORDERED,
            ..m.clone()
        };
        assert_eq!(need(&reordered, Some(&ack), &[]), None);
        // The same for an External source: a new host asks again.
        let ext = manifest(Consent::External);
        let ext_ack = Ack::of(&ext);
        let ext_moved = Manifest {
            endpoints: &ENDPOINTS[..1],
            ..ext.clone()
        };
        assert_eq!(need(&ext, Some(&ext_ack), &[]), None);
        assert_eq!(need(&ext_moved, Some(&ext_ack), &[]), Some(Need::Endpoints));
        assert_eq!(version(&manifest(Consent::None)), "");
        assert_eq!(version(&manifest(Consent::Account)), "");
    }

    #[test]
    fn records_hold_class_version_and_a_boolean_only() {
        let m = manifest(Consent::Overlay);
        let ack = Ack::of(&m);
        let text = ack.encode();
        assert_eq!(text, format!("overlay|{}|1", ack.version));
        assert_eq!(Ack::decode(&text), Some(ack.clone()));
        for bad in ["", "overlay", "overlay|t1|2", "x|t1|1", "overlay|t1"] {
            assert_eq!(Ack::decode(bad), None, "{bad}");
        }
        // An address may hold `|`: the version is everything between the
        // first and the last.
        let piped = Ack {
            version: "t1 22:https://a.example/?a|b".into(),
            ..ack
        };
        assert_eq!(Ack::decode(&piped.encode()), Some(piped));
    }

    /// The legacy digest, SRC-4's `<text version>-<FNV-1a>`, kept here to
    /// show that a record in that form covers nothing now.
    fn legacy_version(m: &Manifest) -> String {
        let mut list: Vec<(&str, &str)> =
            m.endpoints.iter().map(|e| (e.protocol, e.host)).collect();
        list.sort_unstable();
        list.dedup();
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for (protocol, host) in list {
            for b in protocol
                .bytes()
                .chain(*b"://")
                .chain(host.bytes())
                .chain(*b"\n")
            {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        }
        // SRC-4 wrote its records at text version 1.
        format!("1-{h:016x}")
    }

    #[test]
    fn a_record_in_the_old_digest_form_fails_closed() {
        for class in [Consent::External, Consent::Restricted, Consent::Overlay] {
            let m = manifest(class);
            // The very record SRC-4 wrote for this manifest, as stored.
            let text = format!("{}|{}|1", class_name(class), legacy_version(&m));
            let old = Ack::decode(&text).expect("it still reads");
            assert!(!old.covers(&m), "{class:?}");
            assert!(need(&m, Some(&old), &[]).is_some(), "{class:?}");
        }
    }

    /// Two hosts whose SRC-4 digests collide (found offline once with a
    /// distinguished-point search, about 2^32 FNV-1a evaluations, as the
    /// SRC-13b review warned): an acknowledgement of one covered the other
    /// under the digest, and doesn't now.
    #[test]
    fn a_list_that_collided_under_fnv_no_longer_matches() {
        let shown = with(&["c8562448a4e5c066.example"]);
        let crafted = with(&["09dc070a8ee44c03.example"]);
        assert_eq!(legacy_version(&shown), legacy_version(&crafted));
        assert_eq!(legacy_version(&shown), "1-80b91d73c61aaa3a");
        assert_ne!(version(&shown), version(&crafted));
        assert!(Ack::of(&shown).covers(&shown));
        assert!(!Ack::of(&shown).covers(&crafted));
        assert_eq!(
            need(&crafted, Some(&Ack::of(&shown)), &[]),
            Some(Need::Endpoints)
        );
    }

    fn leak(list: &[&str]) -> &'static [Endpoint] {
        let v: Vec<Endpoint> = list
            .iter()
            .map(|h| Endpoint {
                protocol: "https",
                host: Box::leak(h.to_string().into_boxed_str()),
            })
            .collect();
        Box::leak(v.into_boxed_slice())
    }

    fn with(list: &[&str]) -> Manifest {
        Manifest {
            endpoints: leak(list),
            ..manifest(Consent::External)
        }
    }

    #[test]
    fn the_compare_is_exact_and_ignores_order() {
        let shown = with(&["a.example/{hash}", "b.example/t/{hash}"]);
        let ack = Ack::of(&shown);
        // The same list, in another order and with a repeat.
        let same = with(&["b.example/t/{hash}", "a.example/{hash}", "a.example/{hash}"]);
        assert!(ack.covers(&same));
        // One host more, one fewer, one changed, one path changed.
        for other in [
            &["a.example/{hash}", "b.example/t/{hash}", "c.example/{hash}"][..],
            &["a.example/{hash}"],
            &["a.example/{hash}", "b.examp1e/t/{hash}"],
            &["a.example/{hash}", "b.example/~me/{hash}"],
            &[],
        ] {
            assert!(!ack.covers(&with(other)), "{other:?}");
        }
        // Lists a separator could confuse are told apart: the length
        // prefix makes the form injective.
        let pairs: [(&[&str], &[&str]); 3] = [
            (&["a 9:https://b"], &["a", "b"]),
            (&["a.example"], &["a.example "]),
            (&["x/{hash} 1:y"], &["x/{hash}", "y"]),
        ];
        for (a, b) in pairs {
            assert_ne!(version(&with(a)), version(&with(b)), "{a:?} {b:?}");
        }
        // An acknowledgement given while the list was empty covers no host.
        assert!(!Ack::of(&with(&[])).covers(&with(&["a.example/{hash}"])));
        assert_eq!(
            addresses(&same),
            ["https://a.example/{hash}", "https://b.example/t/{hash}"]
        );
    }

    proptest::proptest! {
        /// Two address lists give one version only when they're the same
        /// set, whatever bytes the addresses hold.
        #[test]
        fn versions_are_injective(
            a in proptest::collection::vec("[ -~]{0,12}", 0..5),
            b in proptest::collection::vec("[ -~]{0,12}", 0..5),
        ) {
            let ra: Vec<&str> = a.iter().map(String::as_str).collect();
            let rb: Vec<&str> = b.iter().map(String::as_str).collect();
            let (ma, mb) = (with(&ra), with(&rb));
            let set = |v: &[String]| v.iter().cloned().collect::<std::collections::BTreeSet<_>>();
            proptest::prop_assert_eq!(version(&ma) == version(&mb), set(&a) == set(&b));
        }
    }

    const fn field(name: &'static str, required: bool) -> CredentialSpec {
        CredentialSpec {
            name,
            label: "API key",
            kind: CredentialKind::Secret,
            required,
        }
    }

    #[test]
    fn a_spec_is_checked_against_the_store_and_need() {
        let account = manifest(Consent::Account);
        assert_eq!(check_spec(&account, &[field("api_key", true)]), Ok(()));
        assert_eq!(
            check_spec(
                &account,
                &[
                    field("user", true),
                    CredentialSpec {
                        kind: CredentialKind::Username,
                        ..field("password", false)
                    }
                ]
            ),
            Ok(())
        );
        // Nothing declared, and an optional credential on a class that
        // never needs one, are both fine.
        assert_eq!(check_spec(&account, &[]), Ok(()));
        assert_eq!(
            check_spec(&manifest(Consent::None), &[field("token", false)]),
            Ok(())
        );
        // Names the store refuses, or that belong to the egress setting.
        for bad in ["", "Api", "a.b", "a-b"] {
            assert_eq!(
                check_spec(&account, &[field(bad, true)]),
                Err(SpecError::Name(bad))
            );
        }
        for egress in [SOCKS5_USERNAME, SOCKS5_PASSWORD] {
            assert_eq!(
                check_spec(&account, &[field(egress, true)]),
                Err(SpecError::Egress(egress))
            );
        }
        assert_eq!(
            check_spec(&account, &[field("k", true), field("k", false)]),
            Err(SpecError::Repeated("k"))
        );
        let unlabelled = CredentialSpec {
            label: " ",
            ..field("k", true)
        };
        assert_eq!(
            check_spec(&account, &[unlabelled]),
            Err(SpecError::Label("k"))
        );
        let many: Vec<CredentialSpec> = (0..=MAX_CREDENTIALS)
            .map(|i| field(Box::leak(format!("k{i}").into_boxed_str()), false))
            .collect();
        assert_eq!(check_spec(&account, &many), Err(SpecError::TooMany));
        // Every required credential stored satisfies `need`.
        let names = vec!["api_key".to_string()];
        assert_eq!(need(&account, None, &names), None);
    }

    /// Account's condition against its declared spec (X-consent-gate-lift
    /// step 1): every required credential stored, and nothing else stands
    /// in for one.
    #[test]
    fn account_needs_exactly_its_declared_required_credentials() {
        let m = manifest(Consent::Account);
        let s = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        let two = [field("api_key", true), field("user", true)];
        let cases: [(&[CredentialSpec], &[&str], bool); 17] = [
            // Two required: both, in any order, with extras.
            (&two, &["api_key", "user"], true),
            (&two, &["user", "api_key", "other"], true),
            (&two, &["user", "api_key", SOCKS5_USERNAME], true),
            (&two, &["api_key"], false),
            (&two, &["user"], false),
            (&two, &[], false),
            (&two, &["other", "api_key_"], false),
            (&two, &[SOCKS5_USERNAME, SOCKS5_PASSWORD], false),
            // One required, one optional: the optional one isn't enough.
            (
                &[field("api_key", true), field("note", false)],
                &["note"],
                false,
            ),
            (
                &[field("api_key", true), field("note", false)],
                &["api_key"],
                true,
            ),
            // Only optional ones: a declared one, never an undeclared one.
            (&[field("note", false)], &["note"], true),
            (&[field("note", false)], &["typo"], false),
            (&[field("note", false)], &[SOCKS5_PASSWORD], false),
            // None declared: any plugin name, never an egress one.
            (&[], &["api_key"], true),
            (&[], &[SOCKS5_USERNAME, SOCKS5_PASSWORD], false),
            (&[], &[], false),
            (&[], &["anything", SOCKS5_PASSWORD], true),
        ];
        for (spec, stored, held) in cases {
            let want = (!held).then_some(Need::Credentials);
            assert_eq!(
                need_with(&m, spec, None, &s(stored)),
                want,
                "{spec:?} {stored:?}"
            );
            // An acknowledgement never stands in for credentials.
            assert_eq!(
                need_with(&m, spec, Some(&Ack::of(&m)), &s(stored)),
                want,
                "{spec:?} {stored:?} acked"
            );
        }
        // `need` is `need_with` and no spec.
        assert_eq!(need(&m, None, &s(&["api_key"])), None);
        // The spec matters only to Account: other classes ignore it.
        for class in ALL.into_iter().filter(|c| *c != Consent::Account) {
            let m = manifest(class);
            assert_eq!(
                need_with(&m, &two, Some(&Ack::of(&m)), &[]),
                need(&m, Some(&Ack::of(&m)), &[]),
                "{class:?}"
            );
        }
    }

    /// A spec `check_spec` passes is one storing every required name
    /// satisfies, and `need_with` agrees with it.
    #[test]
    fn check_spec_agrees_with_need_with() {
        let m = manifest(Consent::Account);
        let spec = [
            field("api_key", true),
            field("user", true),
            field("x", false),
        ];
        assert_eq!(check_spec(&m, &spec), Ok(()));
        let all: Vec<String> = vec!["api_key".into(), "user".into()];
        assert_eq!(need_with(&m, &spec, None, &all), None);
        assert_eq!(
            need_with(&m, &spec, None, &all[..1]),
            Some(Need::Credentials)
        );
    }

    /// The approved texts are version 2 for every acknowledged class, and
    /// a record of SRC-4's draft (version 1) covers nothing.
    #[test]
    fn the_approved_texts_are_version_2_and_a_draft_record_asks_again() {
        for class in [Consent::External, Consent::Restricted, Consent::Overlay] {
            assert_eq!(text_version(class), 2, "{class:?}");
            let m = manifest(class);
            assert!(version(&m).starts_with("t2 "), "{class:?}");
            let draft = Ack {
                version: version(&m).replacen("t2 ", "t1 ", 1),
                ..Ack::of(&m)
            };
            assert!(!draft.covers(&m), "{class:?}");
            assert!(need(&m, Some(&draft), &[]).is_some(), "{class:?}");
        }
        assert_eq!(text_version(Consent::None), 0);
        assert_eq!(text_version(Consent::Account), 0);
    }

    #[test]
    fn names_and_values_are_checked() {
        for ok in ["api_key", "a", SOCKS5_USERNAME, SOCKS5_PASSWORD] {
            assert_eq!(check_name(ok), Ok(()));
        }
        let long = "a".repeat(MAX_NAME + 1);
        for bad in ["", "Api", "a.b", "egress.other", "a/b", long.as_str()] {
            assert_eq!(check_name(bad), Err(CredentialRefusal::Name), "{bad}");
        }
        assert!(check_value("k", &Secret::new("x")).is_ok());
        assert!(check_value("k", &Secret::new("")).is_err());
        assert!(check_value("k", &Secret::new("x".repeat(MAX_VALUE))).is_ok());
        assert!(check_value("k", &Secret::new("x".repeat(MAX_VALUE + 1))).is_err());
        let socks = "x".repeat(MAX_SOCKS5_VALUE);
        assert!(check_value(SOCKS5_PASSWORD, &Secret::new(socks.as_str())).is_ok());
        let long = "x".repeat(MAX_SOCKS5_VALUE + 1);
        assert!(check_value(SOCKS5_USERNAME, &Secret::new(long.as_str())).is_err());
    }

    fn creds() -> (SourceCredentials, Arc<Mutex<Store>>, SourceId) {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let id = store
            .register_source_as("probe", "1", SourceState::Disabled, 0)
            .unwrap();
        store
            .register_source_as("other", "1", SourceState::Disabled, 0)
            .unwrap();
        let store = Arc::new(Mutex::new(store));
        let keys = Arc::new(SecretStore::from_key(&[7; 32]));
        (SourceCredentials::with_keys(store.clone(), keys), store, id)
    }

    fn put(
        c: &SourceCredentials,
        store: &Arc<Mutex<Store>>,
        id: SourceId,
        plugin: &str,
        name: &str,
        v: &str,
    ) {
        let sealed = c.seal(plugin, name, &Secret::new(v)).unwrap();
        store
            .lock()
            .unwrap()
            .update_source_secrets(id, &[(name, &sealed)], &[], None)
            .unwrap();
    }

    #[tokio::test]
    async fn credentials_are_sealed_per_source_and_serve_socks5() {
        let (c, store, id) = creds();
        assert_eq!(c.socks5("probe").await, Ok(None));
        put(&c, &store, id, "probe", SOCKS5_USERNAME, "user 1");
        // Half a pair leaves the route down.
        assert_eq!(c.socks5("probe").await, Err(CredentialError));
        put(&c, &store, id, "probe", SOCKS5_PASSWORD, "p@ss:w/rd");
        let auth = c.socks5("probe").await.unwrap().unwrap();
        assert_eq!(auth.username.expose(), "user 1");
        assert_eq!(auth.password.expose(), "p@ss:w/rd");
        // At rest it's sealed: the plain text isn't in the database.
        let raw = store
            .lock()
            .unwrap()
            .source_secret("probe", SOCKS5_PASSWORD)
            .unwrap()
            .unwrap();
        assert!(!raw.windows(9).any(|w| w == b"p@ss:w/rd"));
        // Copied onto another source or name, it doesn't open.
        let other = store.lock().unwrap().sources().unwrap()[1].id;
        store
            .lock()
            .unwrap()
            .update_source_secrets(other, &[(SOCKS5_PASSWORD, &raw)], &[], None)
            .unwrap();
        assert!(c.get("other", SOCKS5_PASSWORD).is_err());
        store
            .lock()
            .unwrap()
            .update_source_secrets(id, &[("api_key", &raw)], &[], None)
            .unwrap();
        assert!(c.get("probe", "api_key").is_err());
        // Nothing prints it.
        assert!(!format!("{c:?} {auth:?}").contains("p@ss"));
    }

    #[test]
    fn purge_drops_credentials_and_the_record() {
        let (c, store, id) = creds();
        put(&c, &store, id, "probe", "api_key", "k");
        let m = manifest(Consent::Restricted);
        let mut s = store.lock().unwrap();
        s.enable_source(id, Some(&Ack::of(&m).encode())).unwrap();
        assert!(s.source_consent("probe").unwrap().is_some());
        s.set_source_state(id, SourceState::Disabled).unwrap();
        assert_eq!(s.source_secret_names("probe").unwrap(), ["api_key"]);
        // Enabling goes only through `enable_source`, where the consent
        // check runs.
        assert!(matches!(
            s.set_source_state(id, SourceState::Enabled),
            Err(lodestar_store::Error::Invalid(_))
        ));
        s.set_source_state(id, SourceState::Purged).unwrap();
        assert!(s.source_consent("probe").unwrap().is_none());
        assert!(s.source_secret_names("probe").unwrap().is_empty());
    }

    /// The egress's source-state check reads the store's own row for the
    /// plugin, by its id: Enabled only, never another plugin's row
    /// (X-tcache-recheck).
    #[tokio::test]
    async fn source_enabled_reads_the_plugins_own_row() {
        let (c, store, id) = creds();
        let other = store
            .lock()
            .unwrap()
            .sources()
            .unwrap()
            .into_iter()
            .find(|r| r.plugin == "other")
            .unwrap()
            .id;
        assert_eq!(c.source_enabled("probe").await, Ok(false));
        store.lock().unwrap().enable_source(other, None).unwrap();
        assert_eq!(c.source_enabled("probe").await, Ok(false), "another's row");
        assert_eq!(c.source_enabled("other").await, Ok(true));
        store.lock().unwrap().enable_source(id, None).unwrap();
        assert_eq!(c.source_enabled("probe").await, Ok(true));
        for state in [
            SourceState::Paused,
            SourceState::Disabled,
            SourceState::Purged,
        ] {
            store.lock().unwrap().set_source_state(id, state).unwrap();
            assert_eq!(c.source_enabled("probe").await, Ok(false), "{state:?}");
        }
        assert_eq!(c.source_enabled("nobody").await, Ok(false));
    }
}
