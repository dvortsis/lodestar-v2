//! `POST /settings/test`'s runs (§10.10.8): each kind on the values on
//! screen, through the integrations SSRF guard, saving nothing.
//!
//! - A secret the form leaves out (secrets are write-only, so it never has
//!   the saved one) is the saved one, but only while the address points
//!   where the saved one does ([`SettingsApi::saved_secret`]): a test
//!   can't carry a saved password to another server.
//! - A notification test sends one message, labelled as a test, with one
//!   attempt and no retries.
//! - The result is fixed-shape. Its message is this crate's or the
//!   integrations crate's own words, never the server's answer, the
//!   address or a credential.

use std::sync::Arc;
use std::time::Duration;

use lodestar_integrations::notify::{discord, email, ntfy, webhook};
use lodestar_integrations::{
    ClientError, DeliveryError, GuardedClient, NetPolicy, Notification, NotificationKind, Notifier,
    Resolve, Secret,
};
use serde_json::Value;

use super::{
    DownloadClientDraft, MAX_TEST_FIELD, SettingsApi, SettingsTest, SettingsTestResult,
    client_config, failed, test_failure,
};
use crate::dto::settings::{
    DiscordDraft, EmailDraft, EmailSecurity, NtfyDraft, TorznabDraft, WebhookDraft,
};

/// A notification test's longest wait for its one attempt.
const NOTIFY_TIMEOUT: Duration = Duration::from_secs(15);

/// The test message every channel sends.
fn test_message() -> Notification {
    Notification::new(
        NotificationKind::Test,
        "Lodestar test notification",
        "This is a test from Lodestar's Settings. If you can read it, this channel works.",
    )
}

/// How tests reach the network. The default is the real resolver and
/// each kind's own policy; tests point names at a local fake server.
#[derive(Clone, Default)]
pub(crate) struct Net {
    pub(crate) resolver: Option<Arc<dyn Resolve>>,
    /// Replaces the HTTP notification channels' policy (a fake server on
    /// loopback, which they may never reach otherwise).
    pub(crate) notify_policy: Option<NetPolicy>,
}

impl Net {
    fn http(&self, policy: NetPolicy, timeout: Duration) -> Result<GuardedClient, ClientError> {
        let client = match &self.resolver {
            Some(r) => GuardedClient::with_resolver(policy, timeout, Arc::clone(r)),
            None => GuardedClient::new(policy, timeout),
        };
        Ok(client?)
    }

    fn notify_http(&self, allow_private: bool) -> Result<GuardedClient, DeliveryError> {
        let policy = self
            .notify_policy
            .unwrap_or(NetPolicy::notification(allow_private));
        self.http(policy, NOTIFY_TIMEOUT)
            .map_err(|_| DeliveryError::Failed("couldn't set up the connection"))
    }
}

/// Whether any address, user name or secret is longer than a test takes.
pub(crate) fn too_long(test: &SettingsTest) -> bool {
    let fields: Vec<Option<&String>> = match test {
        SettingsTest::DownloadClient(d) => {
            vec![Some(&d.url), d.username.as_ref(), d.password.as_ref()]
        }
        SettingsTest::Torznab(d) => vec![Some(&d.url), d.api_key.as_ref()],
        SettingsTest::Ntfy(d) => vec![d.url.as_ref(), d.token.as_ref()],
        SettingsTest::Discord(d) => vec![d.url.as_ref()],
        SettingsTest::Webhook(d) => vec![d.url.as_ref()],
        SettingsTest::Email(d) => {
            let mut v = vec![
                Some(&d.host),
                Some(&d.from),
                Some(&d.username),
                d.password.as_ref(),
            ];
            v.extend(d.to.iter().map(Some));
            v
        }
    };
    fields
        .into_iter()
        .flatten()
        .any(|v| v.chars().count() > MAX_TEST_FIELD)
}

/// Runs one test.
pub(crate) async fn run(
    test: SettingsTest,
    api: Option<Arc<dyn SettingsApi>>,
    net: Net,
) -> SettingsTestResult {
    let saved = Saved(api);
    match test {
        SettingsTest::DownloadClient(d) => download_client(d, &saved).await,
        SettingsTest::Torznab(d) => torznab(d, &saved, &net).await,
        SettingsTest::Ntfy(d) => notify(ntfy(d, &saved, &net).await).await,
        SettingsTest::Discord(d) => notify(discord(d, &saved, &net).await).await,
        SettingsTest::Webhook(d) => notify(webhook(d, &saved, &net).await).await,
        SettingsTest::Email(d) => notify(email(d, &saved, &net).await).await,
    }
}

/// The registry's saved secrets, read off the async runtime.
struct Saved(Option<Arc<dyn SettingsApi>>);

impl Saved {
    async fn secret(
        &self,
        section: &'static str,
        key: String,
        endpoints: Vec<(&'static str, Value)>,
    ) -> Option<Secret> {
        let api = self.0.clone()?;
        tokio::task::spawn_blocking(move || api.saved_secret(section, &key, &endpoints))
            .await
            .ok()
            .flatten()
            .filter(|s| !s.is_empty())
            .map(Secret::new)
    }
}

async fn download_client(mut d: DownloadClientDraft, saved: &Saved) -> SettingsTestResult {
    if d.password.is_none()
        && let Some(id) = d.id.clone()
    {
        let kind = serde_json::to_value(d.client).unwrap_or(Value::Null);
        d.password = saved
            .secret(
                "integrations",
                format!("clients.{id}.password"),
                vec![("url", Value::String(d.url.clone())), ("kind", kind)],
            )
            .await
            .map(|s| s.expose().to_string());
    }
    let run = async {
        let client = lodestar_integrations::clients::connect(client_config(d))?;
        client.test().await
    };
    match run.await {
        Ok(info) => SettingsTestResult {
            api_version: info.api_version,
            error: None,
            ok: true,
            version: Some(info.version),
        },
        Err(e) => test_failure(&e),
    }
}

async fn torznab(d: TorznabDraft, saved: &Saved, net: &Net) -> SettingsTestResult {
    let key = match d.api_key {
        Some(k) => Some(Secret::new(k)),
        None => {
            saved
                .secret(
                    "integrations",
                    "torznab_api_key".into(),
                    vec![("torznab_url", Value::String(d.url.clone()))],
                )
                .await
        }
    };
    let run = async {
        let http = net.http(
            NetPolicy::download_client(),
            lodestar_integrations::torznab::TORZNAB_TIMEOUT,
        )?;
        lodestar_integrations::torznab::caps(&http, &d.url, key.as_ref()).await
    };
    match run.await {
        Ok(info) => SettingsTestResult {
            api_version: None,
            error: None,
            ok: true,
            version: info.version,
        },
        Err(e) => torznab_failure(&e),
    }
}

/// [`test_failure`]'s codes, in an indexer's words rather than a client's.
fn torznab_failure(e: &ClientError) -> SettingsTestResult {
    let mut r = test_failure(e);
    let message = match e {
        ClientError::Auth(_) => Some("The endpoint refused the API key.".to_string()),
        ClientError::Rejected(why) => Some(match why.strip_prefix("HTTP ") {
            Some(status) => format!(
                "The address answered HTTP {}: it isn't a Torznab endpoint.",
                status.chars().take_while(char::is_ascii_digit).collect::<String>()
            ),
            None => "The endpoint refused the request.".to_string(),
        }),
        ClientError::Protocol(_) => {
            Some("The address didn't answer like a Torznab endpoint.".to_string())
        }
        _ => None,
    };
    if let (Some(m), Some(err)) = (message, r.error.as_mut()) {
        err.message = m;
    }
    r
}

/// A secret on screen, else the saved one with no endpoint to match (a
/// URL that is itself the secret).
async fn or_saved(value: Option<String>, saved: &Saved, field: &'static str) -> Option<Secret> {
    match value {
        Some(v) => Some(Secret::new(v)),
        None => {
            saved
                .secret("notifications", field.into(), Vec::new())
                .await
        }
    }
}

fn missing(what: &'static str) -> DeliveryError {
    DeliveryError::InvalidEndpoint(what)
}

async fn ntfy(d: NtfyDraft, saved: &Saved, net: &Net) -> Result<Box<dyn Notifier>, DeliveryError> {
    let url = or_saved(d.url, saved, "ntfy_url")
        .await
        .ok_or(missing("enter the topic URL"))?;
    let token = match d.token {
        Some(t) => Some(Secret::new(t)),
        // The saved token goes only to the saved topic's server.
        None => {
            let endpoint = Value::String(url.expose().to_string());
            saved
                .secret(
                    "notifications",
                    "ntfy_token".into(),
                    vec![("ntfy_url", endpoint)],
                )
                .await
        }
    };
    let mut config = ntfy::NtfyConfig::new(url);
    config.allow_private = d.allow_private;
    config.priority = (d.priority != 0).then_some(d.priority);
    if let Some(t) = token.filter(|t| !t.is_empty()) {
        config.auth = ntfy::NtfyAuth::Bearer(t);
    }
    let http = net.notify_http(d.allow_private)?;
    Ok(Box::new(ntfy::Ntfy::with_client(&config, http)?))
}

async fn discord(
    d: DiscordDraft,
    saved: &Saved,
    net: &Net,
) -> Result<Box<dyn Notifier>, DeliveryError> {
    let url = or_saved(d.url, saved, "discord_url")
        .await
        .ok_or(missing("enter the webhook URL"))?;
    let http = net.notify_http(false)?;
    Ok(Box::new(discord::Discord::with_client(
        &discord::DiscordConfig { webhook_url: url },
        http,
    )?))
}

async fn webhook(
    d: WebhookDraft,
    saved: &Saved,
    net: &Net,
) -> Result<Box<dyn Notifier>, DeliveryError> {
    let url = or_saved(d.url, saved, "webhook_url")
        .await
        .ok_or(missing("enter the webhook URL"))?;
    let http = net.notify_http(d.allow_private)?;
    Ok(Box::new(webhook::Webhook::with_client(
        &webhook::WebhookConfig {
            url,
            allow_private: d.allow_private,
        },
        http,
    )?))
}

async fn email(
    d: EmailDraft,
    saved: &Saved,
    net: &Net,
) -> Result<Box<dyn Notifier>, DeliveryError> {
    let security = match d.security {
        EmailSecurity::Tls => email::SmtpSecurity::Tls,
        EmailSecurity::Starttls => email::SmtpSecurity::StartTls,
        EmailSecurity::None => email::SmtpSecurity::None,
    };
    let mut config = email::EmailConfig::new(d.host.clone(), security, d.from, d.to);
    if d.port != 0 {
        config.port = d.port;
    }
    config.allow_private = d.allow_private;
    if !d.username.is_empty() {
        let password = match d.password {
            Some(p) => Some(Secret::new(p)),
            None => {
                saved
                    .secret(
                        "notifications",
                        "email_password".into(),
                        vec![
                            ("email_host", Value::String(d.host)),
                            ("email_port", Value::from(d.port)),
                            (
                                "email_security",
                                serde_json::to_value(d.security).unwrap_or(Value::Null),
                            ),
                        ],
                    )
                    .await
            }
        };
        config.credentials = Some(email::SmtpCredentials {
            username: Secret::new(d.username),
            password: password.ok_or(missing("enter the password"))?,
        });
    }
    let sender = match (&net.resolver, net.notify_policy) {
        (Some(r), Some(policy)) => email::Email::with_policy(&config, policy, Arc::clone(r))?,
        (Some(r), None) => email::Email::with_resolver(&config, Arc::clone(r))?,
        (None, _) => email::Email::new(&config)?,
    };
    Ok(Box::new(sender))
}

/// Sends the test message once through a built sender.
async fn notify(sender: Result<Box<dyn Notifier>, DeliveryError>) -> SettingsTestResult {
    let result = match sender {
        Ok(s) => s.attempt(&test_message()).await.map_err(|e| e.error),
        Err(e) => Err(e),
    };
    match result {
        Ok(()) => SettingsTestResult {
            api_version: None,
            error: None,
            ok: true,
            version: None,
        },
        Err(e) => delivery_failure(&e),
    }
}

/// A failed send, in words. Its text is the integrations crate's own
/// (static reasons, a status, an address class), never the server's.
pub(crate) fn delivery_failure(e: &DeliveryError) -> SettingsTestResult {
    let code = match e {
        DeliveryError::Blocked(_) => "blocked",
        DeliveryError::InvalidEndpoint(_) => "invalid",
        DeliveryError::Rejected { status: 401 | 403 } | DeliveryError::SmtpRejected { code: 535 } => {
            "bad_credentials"
        }
        DeliveryError::Rejected { .. } | DeliveryError::SmtpRejected { .. } => "rejected",
        DeliveryError::Unreachable => "unreachable",
        DeliveryError::Timeout => "timeout",
        DeliveryError::Failed(_) => "protocol",
    };
    let text = e.to_string();
    let mut chars = text.chars();
    let message = match chars.next() {
        Some(first) => format!("{}{}.", first.to_uppercase(), chars.as_str()),
        None => "The test failed.".to_string(),
    };
    failed(code, message)
}

#[cfg(test)]
mod tests;
