//! The adapter traits every companion's detection, health and
//! configuration code implements (plan 2026-11-06-v06-companions §4.3,
//! CMP-0): CMP-8a implements [`Detect`] and [`Health`], CMP-4a's
//! supervisor and CMP-5g's watch loop call [`Health`], and CMP-8b
//! implements [`Configure`]. Defining them here lets those slices run at
//! the same time.
//!
//! **Everything an adapter reads from a companion is untrusted**: a
//! companion's API is a third-party program, and a URL you enter can
//! point anywhere. So the traits pass back only closed, bounded values: a
//! [`Version`] parsed by its strict grammar, a status from a closed set,
//! and errors that carry the code's own words, never response text. The
//! SSRF guard, timeouts and byte caps are the implementations' (CMP-8a);
//! the trait documents require them.
//!
//! The traits are object-safe (`Box<dyn Detect>`) without a macro crate:
//! async methods return a boxed future, as `lodestar-api`'s `Ops` does.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use lodestar_integrations::secret::Secret;

use crate::catalogue::{CompanionId, Version};
use crate::inputs::{ContainerName, InputError, PortNumber};

/// A boxed, sendable future.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The longest URL you may enter for a companion, in bytes.
pub const ENTERED_URL_MAX: usize = 2048;

/// Where an adapter looks for a companion (plan Q12: never a range scan).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    /// `127.0.0.1` on one of the catalogue's ports.
    Loopback(PortNumber),
    /// A catalogue container name on Docker's network, on one of the
    /// catalogue's ports.
    Container(ContainerName, PortNumber),
    /// A URL you entered.
    Entered(EnteredUrl),
}

/// A URL you entered for a companion, checked for shape only: `http` or
/// `https`, no user information, no whitespace or control characters, at
/// most [`ENTERED_URL_MAX`] bytes. **Where it points isn't checked here:**
/// every request to it goes through the SSRF guard's mode for companions
/// (CMP-8a), which decides at connect time.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct EnteredUrl(String);

impl EnteredUrl {
    pub fn parse(s: &str) -> Result<EnteredUrl, InputError> {
        if s.is_empty() || s.len() > ENTERED_URL_MAX {
            return Err(InputError::Length);
        }
        if !s.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(InputError::Character);
        }
        let lower = s.to_ascii_lowercase();
        let rest = lower
            .strip_prefix("http://")
            .or_else(|| lower.strip_prefix("https://"))
            .ok_or(InputError::Character)?;
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        // `user:pass@host` would put a credential where it gets logged and
        // shown; it's refused rather than stripped.
        if authority.is_empty() || authority.contains('@') {
            return Err(InputError::Character);
        }
        Ok(EnteredUrl(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Shown through `lodestar_log::Url` and the API, never raw, so `Debug`
/// keeps only the scheme.
impl fmt::Debug for EnteredUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scheme = if self.0[..5].eq_ignore_ascii_case("https") {
            "https"
        } else {
            "http"
        };
        write!(f, "EnteredUrl({scheme}://…)")
    }
}

/// Why an adapter call failed. The variants carry the code's own words
/// (`&'static str`), never text a companion sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AdapterError {
    /// Nothing answered in time.
    Timeout,
    /// The connection was refused or the host couldn't be reached.
    Unreachable,
    /// The SSRF guard refused the address.
    Blocked,
    /// The answer passed the byte cap.
    TooLarge,
    /// It answered, but isn't the companion asked for.
    NotThisCompanion,
    /// It refused the credentials.
    Unauthorized,
    /// It answered with something the adapter couldn't read.
    Protocol(&'static str),
    /// A setting couldn't be applied.
    Refused(&'static str),
}

impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AdapterError::Timeout => f.write_str("timed out"),
            AdapterError::Unreachable => f.write_str("unreachable"),
            AdapterError::Blocked => f.write_str("address not allowed"),
            AdapterError::TooLarge => f.write_str("answer too large"),
            AdapterError::NotThisCompanion => f.write_str("not the expected companion"),
            AdapterError::Unauthorized => f.write_str("credentials refused"),
            AdapterError::Protocol(what) => write!(f, "unexpected answer: {what}"),
            AdapterError::Refused(what) => write!(f, "setting refused: {what}"),
        }
    }
}

impl std::error::Error for AdapterError {}

/// A companion found at a target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub target: Target,
    /// Its version, when it reports one in the strict [`Version`] form.
    pub version: Option<Version>,
}

/// One health check's result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    /// It answered its health endpoint as expected.
    Healthy,
    /// It answered, but not as a healthy companion does.
    Unhealthy,
    /// Nothing answered (or the answer was refused before it was read).
    Unreachable,
}

/// Finds a companion at a target.
///
/// Implementations probe only the target given, with a timeout and a
/// byte cap, through the SSRF guard's companion mode, and never follow a
/// redirect off the target.
pub trait Detect: Send + Sync {
    /// The catalogue entry it detects.
    fn companion(&self) -> &CompanionId;

    /// `Ok(None)` when the target answered but isn't this companion, or
    /// nothing is there.
    fn detect<'a>(
        &'a self,
        target: &'a Target,
    ) -> BoxFuture<'a, Result<Option<Detected>, AdapterError>>;
}

/// Checks a running companion's health, with the same limits as
/// [`Detect`]. The caller sets the cadence (plan Q7).
pub trait Health: Send + Sync {
    fn companion(&self) -> &CompanionId;

    fn check<'a>(&'a self, target: &'a Target) -> BoxFuture<'a, HealthStatus>;
}

/// A setting's name, as a configuration plan lists it and the log records
/// it (never its value): `[a-z][a-z0-9_.]{0,63}`, like
/// `webui.bind_address`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SettingKey(&'static str);

impl SettingKey {
    /// For the adapters' own constants. Returns `None` for a name outside
    /// the grammar; adapters test their keys once.
    pub const fn new(key: &'static str) -> Option<SettingKey> {
        let b = key.as_bytes();
        if b.is_empty() || b.len() > 64 || !b[0].is_ascii_lowercase() {
            return None;
        }
        let mut i = 0;
        while i < b.len() {
            let c = b[i];
            if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'.') {
                return None;
            }
            i += 1;
        }
        Some(SettingKey(key))
    }

    pub fn as_str(&self) -> &'static str {
        self.0
    }
}

/// What a configuration run will change, shown to you before it runs
/// (plan §4.3, CMP-5g) and logged by name as it's applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPlan {
    pub companion: CompanionId,
    pub changes: Vec<SettingKey>,
}

/// The credentials a configuration run may use, by field (`password`,
/// `api_key`). CMP-3 implements it over sealed settings rows.
pub trait Credentials: Send + Sync {
    fn get(&self, field: &str) -> Option<Secret>;
}

/// What [`Configure`] is given beyond the target. Built with
/// [`ConfigureInput::new`]; later slices add fields (the egress route,
/// CMP-8b) without breaking implementations.
#[non_exhaustive]
pub struct ConfigureInput<'a> {
    pub credentials: &'a dyn Credentials,
}

impl<'a> ConfigureInput<'a> {
    pub fn new(credentials: &'a dyn Credentials) -> ConfigureInput<'a> {
        ConfigureInput { credentials }
    }
}

impl fmt::Debug for ConfigureInput<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfigureInput").finish_non_exhaustive()
    }
}

/// Configures a companion through its own API. Each change is idempotent:
/// applying the same plan twice leaves the same state.
pub trait Configure: Send + Sync {
    fn companion(&self) -> &CompanionId;

    /// The settings [`Configure::apply`] would change, without changing
    /// anything.
    fn plan<'a>(
        &'a self,
        target: &'a Target,
        input: &'a ConfigureInput<'a>,
    ) -> BoxFuture<'a, Result<ConfigPlan, AdapterError>>;

    /// Applies a plan from [`Configure::plan`]. Changes nothing the plan
    /// doesn't list.
    fn apply<'a>(
        &'a self,
        target: &'a Target,
        input: &'a ConfigureInput<'a>,
        plan: &'a ConfigPlan,
    ) -> BoxFuture<'a, Result<(), AdapterError>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn entered_urls() {
        for ok in [
            "http://192.168.1.10:8080",
            "https://qbit.example.lan/",
            "HTTP://Box:9696/prowlarr",
            "http://[::1]:8080/x?y=1",
        ] {
            assert!(EnteredUrl::parse(ok).is_ok(), "{ok}");
        }
        let cases: &[(&str, InputError)] = &[
            ("", InputError::Length),
            ("ftp://x", InputError::Character),
            ("file:///etc/passwd", InputError::Character),
            ("http://user:pass@host/", InputError::Character),
            ("http://@host/", InputError::Character),
            ("http:///path", InputError::Character),
            ("http://", InputError::Character),
            ("http://a b", InputError::Character),
            ("http://a\nb", InputError::Character),
            ("http://é", InputError::Character),
            ("javascript:alert(1)", InputError::Character),
        ];
        for (s, e) in cases {
            assert_eq!(EnteredUrl::parse(s), Err(*e), "{s:?}");
        }
        assert_eq!(
            EnteredUrl::parse(&format!("http://a/{}", "b".repeat(ENTERED_URL_MAX))),
            Err(InputError::Length)
        );
        // A user@ after the path is not user information.
        assert!(EnteredUrl::parse("http://host/a@b").is_ok());
        let u = EnteredUrl::parse("https://secret-host.lan/").unwrap();
        assert_eq!(format!("{u:?}"), "EnteredUrl(https://…)");
        let u = EnteredUrl::parse("http://secret-host.lan/").unwrap();
        assert!(!format!("{u:?}").contains("secret"));
    }

    #[test]
    fn setting_keys() {
        assert_eq!(
            SettingKey::new("webui.bind_address").unwrap().as_str(),
            "webui.bind_address"
        );
        for bad in ["", "Web", "1a", "a-b", "a b", &"a".repeat(65)] {
            let leaked: &'static str = Box::leak(bad.to_owned().into_boxed_str());
            assert!(SettingKey::new(leaked).is_none(), "{bad}");
        }
    }

    // A fake companion, to check the traits are usable as objects and
    // that plan-then-apply composes.
    struct Fake {
        id: CompanionId,
        applied: Mutex<Vec<&'static str>>,
    }

    struct NoCredentials;
    impl Credentials for NoCredentials {
        fn get(&self, _: &str) -> Option<Secret> {
            None
        }
    }

    struct OneCredential;
    impl Credentials for OneCredential {
        fn get(&self, field: &str) -> Option<Secret> {
            (field == "password").then(|| Secret::new("hunter2"))
        }
    }

    impl Detect for Fake {
        fn companion(&self) -> &CompanionId {
            &self.id
        }
        fn detect<'a>(
            &'a self,
            target: &'a Target,
        ) -> BoxFuture<'a, Result<Option<Detected>, AdapterError>> {
            Box::pin(async move {
                Ok(match target {
                    Target::Loopback(p) if p.get() == 8080 => Some(Detected {
                        target: target.clone(),
                        version: Version::parse("5.0.3").ok(),
                    }),
                    Target::Entered(_) => return Err(AdapterError::Blocked),
                    _ => None,
                })
            })
        }
    }

    impl Health for Fake {
        fn companion(&self) -> &CompanionId {
            &self.id
        }
        fn check<'a>(&'a self, target: &'a Target) -> BoxFuture<'a, HealthStatus> {
            Box::pin(async move {
                match target {
                    Target::Loopback(_) => HealthStatus::Healthy,
                    _ => HealthStatus::Unreachable,
                }
            })
        }
    }

    impl Configure for Fake {
        fn companion(&self) -> &CompanionId {
            &self.id
        }
        fn plan<'a>(
            &'a self,
            _target: &'a Target,
            input: &'a ConfigureInput<'a>,
        ) -> BoxFuture<'a, Result<ConfigPlan, AdapterError>> {
            Box::pin(async move {
                if input.credentials.get("password").is_none() {
                    return Err(AdapterError::Unauthorized);
                }
                Ok(ConfigPlan {
                    companion: self.id.clone(),
                    changes: vec![SettingKey::new("webui.bind_address").unwrap()],
                })
            })
        }
        fn apply<'a>(
            &'a self,
            _target: &'a Target,
            _input: &'a ConfigureInput<'a>,
            plan: &'a ConfigPlan,
        ) -> BoxFuture<'a, Result<(), AdapterError>> {
            Box::pin(async move {
                let mut applied = self.applied.lock().unwrap();
                for c in &plan.changes {
                    if !applied.contains(&c.as_str()) {
                        applied.push(c.as_str());
                    }
                }
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn traits_work_as_objects() {
        let fake = std::sync::Arc::new(Fake {
            id: CompanionId::parse("qbittorrent").unwrap(),
            applied: Mutex::new(Vec::new()),
        });
        let detect: std::sync::Arc<dyn Detect> = fake.clone();
        let health: std::sync::Arc<dyn Health> = fake.clone();
        let configure: std::sync::Arc<dyn Configure> = fake.clone();
        assert_eq!(detect.companion().as_str(), "qbittorrent");

        let here = Target::Loopback(PortNumber::new(8080).unwrap());
        let found = detect.detect(&here).await.unwrap().unwrap();
        assert_eq!(found.version.unwrap().as_str(), "5.0.3");
        let elsewhere = Target::Container(
            ContainerName::parse("qbittorrent").unwrap(),
            PortNumber::new(8080).unwrap(),
        );
        assert_eq!(detect.detect(&elsewhere).await.unwrap(), None);
        let entered = Target::Entered(EnteredUrl::parse("http://10.0.0.1:8080").unwrap());
        assert_eq!(detect.detect(&entered).await, Err(AdapterError::Blocked));
        assert_eq!(health.check(&here).await, HealthStatus::Healthy);
        assert_eq!(health.check(&elsewhere).await, HealthStatus::Unreachable);

        let none = NoCredentials;
        let input = ConfigureInput::new(&none);
        assert_eq!(
            configure.plan(&here, &input).await,
            Err(AdapterError::Unauthorized)
        );
        let creds = OneCredential;
        let input = ConfigureInput::new(&creds);
        assert!(!format!("{input:?}").contains("hunter2"));
        let plan = configure.plan(&here, &input).await.unwrap();
        configure.apply(&here, &input, &plan).await.unwrap();
        configure.apply(&here, &input, &plan).await.unwrap();
        assert_eq!(*fake.applied.lock().unwrap(), ["webui.bind_address"]);
    }

    #[test]
    fn errors_carry_no_response_text() {
        assert_eq!(
            AdapterError::Protocol("no version field").to_string(),
            "unexpected answer: no version field"
        );
    }
}
