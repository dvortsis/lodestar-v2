//! Integrations (§10.10.8): the download clients Send goes to, and a
//! Torznab endpoint (Prowlarr, Jackett). Stored and shown; the send
//! endpoint and the connection tests read them.
//!
//! - **Clients** are a list, each with a stable `id` that the default and
//!   a send name. A client's `password` (or Aria2's RPC secret) is sealed
//!   at rest and never returned: `secrets_set` lists the clients that have
//!   one, an item saved without it keeps the saved one, and `""` removes
//!   it (`Field::secret_keys`).
//! - **The Torznab API key** is a secret field.
//! - Addresses are checked for shape here (`http` or `https`, a host, no
//!   user name or password in them); where they may point is the SSRF
//!   guard's call when they're used.
//! - No `config.toml` key or variable sets these: credentials belong in
//!   the app, where they're sealed.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Field, SecretText, Section};
use crate::config::Config;

/// The most download clients the list holds.
pub const MAX_CLIENTS: usize = 8;
/// The longest address, in bytes.
pub const MAX_URL: usize = 2048;
/// The longest name, user name, category or path, in characters.
pub const MAX_TEXT: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ClientKind {
    Qbittorrent,
    Transmission,
    Deluge,
    Rtorrent,
    Aria2,
}

/// One download client. Fields that don't apply to its kind are ignored
/// (Deluge has no user name; Aria2 takes only the RPC secret).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DownloadClient {
    /// Stable, unique in the list: lowercase letters, digits and `-`.
    pub id: String,
    /// What the UI calls it ("Home qBittorrent").
    pub name: String,
    #[schema(inline)]
    pub kind: ClientKind,
    /// The client's address (`http://192.168.1.5:8080/`).
    pub url: String,
    #[serde(default)]
    pub username: String,
    /// The password or RPC secret. Write-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, write_only)]
    pub password: Option<SecretText>,
    /// The category (label) new torrents get; empty for none.
    #[serde(default)]
    pub category: String,
    /// Where the client saves them; empty for its own default.
    #[serde(default)]
    pub save_path: String,
    /// Add torrents paused.
    #[serde(default)]
    pub paused: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Integrations {
    /// The download clients, at most eight.
    #[schema(inline)]
    pub clients: Vec<DownloadClient>,
    /// The `id` Send uses when none is named; empty for the first client.
    pub default_client: String,
    /// A Torznab endpoint (`https://prowlarr.lan/1/api`); empty for none.
    pub torznab_url: String,
    /// Its API key. Write-only.
    #[schema(value_type = String)]
    pub torznab_api_key: SecretText,
}

impl Integrations {
    fn client(&self, id: &str) -> Option<&DownloadClient> {
        self.clients.iter().find(|c| c.id == id)
    }
}

/// Checks an address's shape: `http` or `https`, a host, and no
/// credentials in it (they go in their own fields, which are sealed).
/// The message never quotes it.
pub fn check_url(url: &str) -> Result<(), String> {
    if url.len() > MAX_URL {
        return Err(format!("Must be at most {MAX_URL} characters."));
    }
    let u = reqwest::Url::parse(url)
        .map_err(|_| "Must be a full address, such as http://192.168.1.5:8080/.".to_string())?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err("Must start with http:// or https://.".into());
    }
    if u.host_str().is_none_or(str::is_empty) {
        return Err("Must include a host name or IP address.".into());
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err("Put the user name and password in their own fields, not the address.".into());
    }
    Ok(())
}

fn check_text(what: &str, s: &str) -> Result<(), String> {
    if s.chars().count() > MAX_TEXT {
        return Err(format!("{what} must be at most {MAX_TEXT} characters."));
    }
    if s.chars().any(char::is_control) {
        return Err(format!("{what} can't contain control characters."));
    }
    Ok(())
}

fn check_client(c: &DownloadClient) -> Result<(), String> {
    let id_ok = !c.id.is_empty()
        && c.id.len() <= 32
        && c.id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !id_ok {
        return Err(
            "Each client needs an id of up to 32 lowercase letters, digits or hyphens.".into(),
        );
    }
    let at = |e: String| format!("Client {}: {e}", c.id);
    if c.name.trim().is_empty() {
        return Err(at("Give it a name.".into()));
    }
    check_text("The name", &c.name).map_err(at)?;
    check_url(&c.url).map_err(at)?;
    check_text("The user name", &c.username).map_err(at)?;
    check_text("The category", &c.category).map_err(at)?;
    check_text("The save path", &c.save_path).map_err(at)?;
    if let Some(p) = &c.password
        && p.0.len() > MAX_TEXT
    {
        return Err(at(format!(
            "The password must be at most {MAX_TEXT} characters."
        )));
    }
    Ok(())
}

impl Section for Integrations {
    const SECTION: &'static str = "integrations";
    const FIELDS: &'static [Field] = &[
        Field::new("clients")
            .group("download_clients")
            .secret_keys(&["password"])
            .keywords(),
        Field::new("default_client").group("download_clients"),
        Field::new("torznab_url").group("torznab"),
        Field::new("torznab_api_key").group("torznab").secret(),
    ];

    /// No `config.toml` key or variable backs these (see the module docs).
    fn from_config(_: &Config) -> Self {
        Integrations::default()
    }

    fn apply(&self, _: &str, _: &mut Config) {}

    fn check(&self, field: &str) -> Result<(), String> {
        match field {
            "clients" => {
                if self.clients.len() > MAX_CLIENTS {
                    return Err(format!("Must be at most {MAX_CLIENTS} clients."));
                }
                for (i, c) in self.clients.iter().enumerate() {
                    check_client(c)?;
                    if self.clients[..i].iter().any(|o| o.id == c.id) {
                        return Err(format!("Two clients have the id {}.", c.id));
                    }
                }
                // A removed client can't stay the default.
                if !self.default_client.is_empty() && self.client(&self.default_client).is_none() {
                    return Err("The default client isn't in the list any more. \
                                Choose another default first."
                        .into());
                }
                Ok(())
            }
            "default_client" => {
                if self.default_client.is_empty() || self.client(&self.default_client).is_some() {
                    Ok(())
                } else {
                    Err("Must be the id of a client in the list, or empty.".into())
                }
            }
            "torznab_url" if self.torznab_url.is_empty() => Ok(()),
            "torznab_url" => check_url(&self.torznab_url),
            "torznab_api_key" if self.torznab_api_key.0.len() > MAX_TEXT => {
                Err(format!("Must be at most {MAX_TEXT} characters."))
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(id: &str, url: &str) -> DownloadClient {
        DownloadClient {
            id: id.into(),
            name: "Home".into(),
            kind: ClientKind::Qbittorrent,
            url: url.into(),
            username: String::new(),
            password: None,
            category: String::new(),
            save_path: String::new(),
            paused: false,
        }
    }

    #[test]
    fn addresses_are_checked_without_being_quoted() {
        assert!(check_url("http://192.168.1.5:8080/").is_ok());
        assert!(check_url("https://qb.lan/").is_ok());
        for bad in [
            "ftp://x/",
            "nothing",
            "http://admin:hunter2@x/",
            "file:///etc/passwd",
        ] {
            let e = check_url(bad).unwrap_err();
            assert!(!e.contains("hunter2") && !e.contains("passwd"), "{e}");
        }
        assert!(check_url(&format!("http://x/{}", "a".repeat(MAX_URL))).is_err());
    }

    #[test]
    fn the_list_is_checked_whole() {
        let mut s = Integrations {
            clients: vec![client("home", "http://x/"), client("home", "http://y/")],
            ..Default::default()
        };
        assert!(s.check("clients").unwrap_err().contains("Two clients"));
        s.clients[1].id = "Bad Id".into();
        assert!(s.check("clients").is_err());
        s.clients[1].id = "seedbox".into();
        assert!(s.check("clients").is_ok());
        s.default_client = "nas".into();
        assert!(s.check("default_client").is_err());
        assert!(s.check("clients").is_err());
        s.default_client = "seedbox".into();
        assert!(s.check("default_client").is_ok());
        s.clients = (0..=MAX_CLIENTS)
            .map(|i| client(&format!("c{i}"), "http://x/"))
            .collect();
        assert!(s.check("clients").is_err());
    }

    #[test]
    fn a_password_never_prints() {
        let mut c = client("home", "http://x/");
        c.password = Some(SecretText("hunter2".into()));
        assert!(!format!("{c:?}").contains("hunter2"));
    }
}
