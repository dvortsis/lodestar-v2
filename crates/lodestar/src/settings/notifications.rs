//! Notifications (§10.10.8): one of each channel (ntfy, Discord, email,
//! webhook) and which events go where. Stored and shown; delivery reads
//! them, and `POST /settings/test` sends a test with the values on screen.
//!
//! - **Secrets:** the ntfy topic URL (the only thing keeping a public
//!   topic private) and its token, the Discord webhook URL, the SMTP
//!   password and the webhook URL. Sealed at rest, never returned.
//! - **A channel is set up** when its address is set; an event routed to a
//!   channel that isn't is skipped at delivery, never an error.
//! - Private-network addresses (a self-hosted ntfy or SMTP relay on the
//!   LAN) need the channel's `*_allow_private`; loopback, link-local and
//!   metadata addresses stay refused either way (the SSRF guard, §12).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::integrations::{MAX_TEXT, check_url};
use super::{Auto, Field, SecretText, Section};
use crate::config::Config;

/// The most email recipients.
pub const MAX_RECIPIENTS: usize = 10;

/// A channel an event can go to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Ntfy,
    Discord,
    Email,
    Webhook,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SmtpSecurity {
    /// Implicit TLS (usually port 465).
    Tls,
    /// STARTTLS, required (usually port 587).
    #[default]
    Starttls,
    /// None: a LAN relay; credentials only to a plainly local host.
    None,
}

/// The section: every channel off and nothing routed by default. Enum
/// fields are undocumented so their schema stays inline.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Notifications {
    /// The topic URL (`https://ntfy.sh/my-lodestar`). Write-only.
    #[schema(value_type = String)]
    pub ntfy_url: SecretText,
    /// An access token (`tk_…`) for a protected topic. Write-only.
    #[schema(value_type = String)]
    pub ntfy_token: SecretText,
    /// 1 (min) to 5 (max); 0 leaves ntfy's default.
    pub ntfy_priority: u8,
    /// Allow a self-hosted ntfy server on the local network.
    pub ntfy_allow_private: bool,
    /// The channel's webhook URL from Discord. Write-only.
    #[schema(value_type = String)]
    pub discord_url: SecretText,
    /// The SMTP server's host name; empty for no email.
    pub email_host: String,
    /// 0 is the usual port for the security mode.
    pub email_port: u16,
    #[schema(inline)]
    pub email_security: SmtpSecurity,
    /// The SMTP user name; empty to send without logging in.
    pub email_username: String,
    /// Write-only.
    #[schema(value_type = String)]
    pub email_password: SecretText,
    /// The From address.
    pub email_from: String,
    /// Who receives it, at most ten addresses.
    pub email_to: Vec<String>,
    /// Allow an SMTP relay on the local network.
    pub email_allow_private: bool,
    /// Where the JSON payload is posted. Write-only.
    #[schema(value_type = String)]
    pub webhook_url: SecretText,
    /// Allow a receiver on the local network.
    pub webhook_allow_private: bool,
    /// Where a saved search's new results go (§10.8).
    #[schema(inline)]
    pub route_alerts: Vec<Channel>,
    /// Where update news goes: before, after and failures, as Updates'
    /// "Notify me" has them (§12.2).
    #[schema(inline)]
    pub route_updates: Vec<Channel>,
    /// Where problems go: a stalled crawler, a full disk, a failed backup.
    #[schema(inline)]
    pub route_health: Vec<Channel>,
}

/// A plausible email address: one `@`, something on each side, no spaces
/// or angle brackets (it goes into a header). The message never quotes it.
pub fn check_email(s: &str) -> Result<(), String> {
    let ok = s.len() <= 254
        && s.split('@').count() == 2
        && s.split('@').all(|p| !p.is_empty())
        && !s
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | ',' | ';'));
    if ok {
        Ok(())
    } else {
        Err("Must be an email address, such as me@example.com.".into())
    }
}

/// A host name or IP address, as an SMTP server's. Never quoted.
fn check_host(s: &str) -> Result<(), String> {
    let ok = !s.is_empty()
        && s.len() <= 253
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'));
    if ok {
        Ok(())
    } else {
        Err("Must be a host name or IP address, such as smtp.example.com.".into())
    }
}

fn check_routes(routes: &[Channel]) -> Result<(), String> {
    let mut seen = routes.to_vec();
    seen.sort();
    seen.dedup();
    if seen.len() != routes.len() {
        return Err("Lists a channel twice.".into());
    }
    Ok(())
}

fn check_secret_url(s: &SecretText) -> Result<(), String> {
    if s.is_empty() {
        Ok(())
    } else {
        check_url(&s.0)
    }
}

impl Section for Notifications {
    const SECTION: &'static str = "notifications";
    const FIELDS: &'static [Field] = &[
        Field::new("ntfy_url").group("ntfy").secret(),
        Field::new("ntfy_token").group("ntfy").secret().advanced(),
        Field::new("ntfy_priority")
            .group("ntfy")
            .range(0, 5)
            .auto(Auto::Int(0))
            .advanced(),
        Field::new("ntfy_allow_private").group("ntfy").advanced(),
        Field::new("discord_url").group("discord").secret(),
        Field::new("email_host").group("email"),
        Field::new("email_port")
            .group("email")
            .range(0, 65535)
            .auto(Auto::Int(0))
            .advanced(),
        Field::new("email_security").group("email"),
        Field::new("email_username").group("email"),
        Field::new("email_password").group("email").secret(),
        Field::new("email_from").group("email"),
        Field::new("email_to").group("email"),
        Field::new("email_allow_private").group("email").advanced(),
        Field::new("webhook_url").group("webhook").secret(),
        Field::new("webhook_allow_private")
            .group("webhook")
            .advanced(),
        Field::new("route_alerts").group("routing"),
        Field::new("route_updates").group("routing"),
        Field::new("route_health").group("routing"),
    ];

    /// No `config.toml` key or variable backs these: they hold credentials.
    fn from_config(_: &Config) -> Self {
        Notifications::default()
    }

    fn apply(&self, _: &str, _: &mut Config) {}

    fn check(&self, field: &str) -> Result<(), String> {
        match field {
            "ntfy_url" => check_secret_url(&self.ntfy_url),
            "discord_url" => check_secret_url(&self.discord_url),
            "webhook_url" => check_secret_url(&self.webhook_url),
            "ntfy_token" | "email_password" | "email_username" => {
                let v = match field {
                    "ntfy_token" => &self.ntfy_token.0,
                    "email_password" => &self.email_password.0,
                    _ => &self.email_username,
                };
                if v.len() > MAX_TEXT || v.chars().any(char::is_control) {
                    Err(format!(
                        "Must be at most {MAX_TEXT} characters, with no control characters."
                    ))
                } else {
                    Ok(())
                }
            }
            "email_host" if self.email_host.is_empty() => Ok(()),
            "email_host" => check_host(&self.email_host),
            "email_from" if self.email_from.is_empty() => Ok(()),
            "email_from" => check_email(&self.email_from),
            "email_to" => {
                if self.email_to.len() > MAX_RECIPIENTS {
                    return Err(format!("Must be at most {MAX_RECIPIENTS} addresses."));
                }
                self.email_to.iter().try_for_each(|a| check_email(a))
            }
            "route_alerts" => check_routes(&self.route_alerts),
            "route_updates" => check_routes(&self.route_updates),
            "route_health" => check_routes(&self.route_health),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_never_quote_the_value() {
        let mut n = Notifications {
            ntfy_url: SecretText("ftp://hunter2/".into()),
            ..Default::default()
        };
        let e = n.check("ntfy_url").unwrap_err();
        assert!(!e.contains("hunter2"), "{e}");
        n.email_to = vec!["a@b.c".into(), "not an address".into()];
        assert!(!n.check("email_to").unwrap_err().contains("not an"));
        n.email_to = vec!["a@b.c".into()];
        assert!(n.check("email_to").is_ok());
        n.email_from = "Me <me@x.y>".into();
        assert!(n.check("email_from").is_err());
        n.route_alerts = vec![Channel::Ntfy, Channel::Ntfy];
        assert!(n.check("route_alerts").is_err());
        n.email_host = "smtp.example.com".into();
        assert!(n.check("email_host").is_ok());
        n.email_host = "smtp example".into();
        assert!(n.check("email_host").is_err());
    }

    #[test]
    fn secrets_never_print() {
        let n = Notifications {
            ntfy_url: SecretText("https://ntfy.sh/x".into()),
            ..Default::default()
        };
        assert!(!format!("{n:?}").contains("ntfy.sh"));
    }
}
