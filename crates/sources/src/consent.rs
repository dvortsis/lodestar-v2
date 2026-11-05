//! The consent framework (architecture §18.7): what enabling a source
//! requires, the consent records that say it was given, and the
//! per-source credential store.
//!
//! - **Class gating.** [`need`] says what stands between a source and
//!   being enabled:
//!   - **None:** nothing; enabling is a toggle.
//!   - **External:** the endpoint list, acknowledged ([`Need::Endpoints`]).
//!   - **Account:** credentials, stored ([`Need::Credentials`]).
//!   - **Restricted** and **Overlay:** the consent screen, acknowledged
//!     with the admin password ([`Need::Consent`], [`needs_password`]).
//!
//!   The server checks it on every path that can turn a source on: the
//!   API's enable, and every start (a source the store has as enabled
//!   whose condition no longer holds is switched off before it runs).
//! - **Consent records** ([`Ack`]) hold the class, the version of what
//!   was shown and a boolean, with no time and no author (Q14, §1.8.8).
//!   The version ([`version`]) covers the class's consent text
//!   ([`text_version`]) and the endpoint list, so a new text, or a plugin
//!   update that contacts new hosts, asks again.
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
use lodestar_store::Store;

use crate::egress::{CredentialError, EgressCredentials};
use crate::manifest::{Consent, Endpoint, Manifest};
use crate::plugin::BoxFuture;

/// The current version of each class's consent text. Raise a class's
/// number whenever its text changes in substance: every source of that
/// class asks again before it's next enabled, and one already enabled is
/// switched off at the next start until it's acknowledged again.
///
/// The texts are drafts until the owner approves them (Q1,
/// `docs/decisions/2026-11-02-SRC-4.md`).
pub const fn text_version(class: Consent) -> u32 {
    match class {
        Consent::None => 0,
        Consent::External => 1,
        Consent::Account => 0,
        Consent::Restricted => 1,
        Consent::Overlay => 1,
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

/// The version an acknowledgement of `manifest` is bound to:
/// `<text version>-<endpoint digest>`. Empty for a class that's never
/// acknowledged.
pub fn version(manifest: &Manifest) -> String {
    if !needs_ack(manifest.consent) {
        return String::new();
    }
    format!(
        "{}-{:016x}",
        text_version(manifest.consent),
        endpoint_digest(manifest.endpoints)
    )
}

/// FNV-1a over the sorted, de-duplicated endpoint list. Not a security
/// boundary (the manifest is compiled in): it only notices a change.
fn endpoint_digest(endpoints: &[Endpoint]) -> u64 {
    let mut list: Vec<(&str, &str)> = endpoints.iter().map(|e| (e.protocol, e.host)).collect();
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
    h
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

    /// Whether this record covers `manifest` as it is now.
    pub fn covers(&self, manifest: &Manifest) -> bool {
        self.accepted && self.class == manifest.consent && self.version == version(manifest)
    }

    /// The stored form: `<class>|<version>|<0 or 1>`.
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
        let mut it = text.split('|');
        let (Some(class), Some(version), Some(accepted), None) =
            (it.next(), it.next(), it.next(), it.next())
        else {
            return None;
        };
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
/// record and the names of its stored credentials; `None` when its
/// class's condition holds.
pub fn need(manifest: &Manifest, ack: Option<&Ack>, credentials: &[String]) -> Option<Need> {
    let class = manifest.consent;
    if needs_credentials(class) && !credentials.iter().any(|n| !is_egress_name(n)) {
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

/// [`need`], read from the store (blocking).
pub fn need_in(store: &Store, manifest: &Manifest) -> Result<Option<Need>, lodestar_store::Error> {
    let ack = store
        .source_consent(manifest.id)?
        .as_deref()
        .and_then(Ack::decode);
    let names = store.source_secret_names(manifest.id)?;
    Ok(need(manifest, ack.as_ref(), &names))
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
    /// Over the install's key file in `data_dir`.
    pub fn new(store: Arc<Mutex<Store>>, data_dir: PathBuf) -> SourceCredentials {
        SourceCredentials {
            inner: Arc::new(Inner {
                store,
                keys: Keys::Dir(data_dir, Mutex::new(None)),
            }),
        }
    }

    /// Over a key the caller holds (tests; workers, §19.6).
    pub fn with_keys(store: Arc<Mutex<Store>>, keys: Arc<SecretStore>) -> SourceCredentials {
        SourceCredentials {
            inner: Arc::new(Inner {
                store,
                keys: Keys::Fixed(keys),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Kind, Network, Priority, Risk};
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
            version: format!("0-{}", ack.version.split_once('-').unwrap().1),
            ..ack.clone()
        };
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
        assert_eq!(Ack::decode(&text), Some(ack));
        for bad in ["", "overlay", "overlay|1-0|2", "x|1-0|1", "overlay|1|1|1"] {
            assert_eq!(Ack::decode(bad), None, "{bad}");
        }
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
}
