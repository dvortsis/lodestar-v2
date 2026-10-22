//! The email sender (SMTP).
//!
//! Each attempt opens one SMTP session, sends one message to the configured
//! recipients and quits:
//!
//! - **The connection goes through the SSRF guard.** The host is resolved
//!   once and checked like an HTTP host ([`resolve_tcp`]), with
//!   `NetPolicy::notification(allow_private)`, so loopback, link-local and
//!   metadata addresses are never reached and a refused literal fails when
//!   the channel is saved. Plain and STARTTLS sessions run over the stream
//!   [`connect_tcp_with`] returns. An implicit-TLS session hands lettre the
//!   checked socket addresses instead (lettre can only start TLS before the
//!   greeting on a stream it connected itself); it connects to exactly those
//!   addresses, without resolving anything.
//! - **TLS:** [`SmtpSecurity::Tls`] is implicit TLS (port 465);
//!   [`SmtpSecurity::StartTls`] *requires* STARTTLS: a server that doesn't
//!   offer it fails the attempt, it never falls back to plaintext. The
//!   certificate is verified against the configured host name (SNI), with
//!   the platform verifier reqwest uses, even though the TCP connection went
//!   to a checked IP.
//! - **Credentials** are [`Secret`]s and are sent only over TLS, or in
//!   plaintext to a host that is plainly local (a private IP literal, a
//!   single-label name, `.lan`, `.local` or `.home.arpa`) when the channel
//!   may reach private addresses, and only when the address actually
//!   connected to is private too. AUTH PLAIN or LOGIN, whichever the server
//!   offers. A 535 at login is final and says to check the username and
//!   password, never what they were.
//! - **The message** is `multipart/alternative`: plain text and simple HTML
//!   (§10.9.7: no images, no remote resources, no scripts), both built with
//!   `compose()`, so the withheld-private note (§18.7) survives truncation.
//!   Names are control-stripped in both parts and HTML-escaped in the HTML
//!   one. Both parts are base64, so the whole message stays under
//!   [`MAX_MESSAGE_BYTES`] whatever the text.
//! - **Headers:** the Subject comes from the notification title (names
//!   included) with CR, LF and other controls removed, and `=?` broken up so
//!   a name can't smuggle in an encoded-word; lettre encodes non-ASCII
//!   (RFC 2047). Addresses are lettre [`Address`]es parsed when
//!   the channel is saved, never free-form text, and the envelope comes
//!   from them alone, so nothing in a name can add a recipient.
//! - **Idempotency:** the `Message-ID` is derived from
//!   [`Notification::id`], so every retry of one notification carries the
//!   same ID and a receiving system can drop the duplicate.
//! - **Delivery classes:** a 4xx reply, a connection failure, a connection
//!   dropped mid-session or a timeout is retryable through [`deliver`]; a
//!   5xx reply, a refused login or a certificate that doesn't verify is
//!   final.
//!
//! [`deliver`]: super::deliver

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use lettre::address::Envelope;
use lettre::message::header::{ContentTransferEncoding, ContentType};
use lettre::message::{Body, Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::Error as SmtpError;
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{AsyncSmtpConnection, TlsParameters};
use lettre::transport::smtp::extension::ClientId;
use lettre::{Address, Message};
use reqwest::Url;

use super::text::{clean_line, compose, truncate_bytes, truncate_chars, web_link};
use super::{AttemptError, Channel, DeliveryError, Notification, Notifier};
use crate::BoxFuture;
use crate::net::{
    AddrClass, NetPolicy, Resolve, SystemResolver, check_host, classify as classify_addr,
    connect_tcp_with, is_local_host, resolve_tcp,
};
use crate::secret::Secret;

/// The timeout for one whole attempt: connecting, TLS, login and sending.
pub const EMAIL_TIMEOUT: Duration = Duration::from_secs(60);

/// The timeout for resolving and connecting, within [`EMAIL_TIMEOUT`].
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The largest message sent, headers included. Mail servers commonly accept
/// 10 MB or more; a digest never needs that much, and staying far below
/// keeps a huge one from being refused.
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;

/// The plain-text part before encoding. Base64 adds a third, so the two
/// parts stay within [`MAX_MESSAGE_BYTES`] with room for the headers.
const MAX_TEXT_BYTES: usize = 64 * 1024;

/// The HTML part's composed text before encoding (the markup around it is
/// fixed and small).
const MAX_HTML_BYTES: usize = 96 * 1024;

/// The longest Subject, in characters.
const MAX_SUBJECT_CHARS: usize = 200;

/// The most recipients one channel may have.
pub const MAX_RECIPIENTS: usize = 20;

/// The longest link put in a message.
const MAX_LINK_BYTES: usize = 2048;

/// The display name on the From address.
const FROM_NAME: &str = "Lodestar";

/// How the session is protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmtpSecurity {
    /// Implicit TLS from the first byte (SMTPS, usually port 465).
    Tls,
    /// Plaintext, then STARTTLS before anything else (usually port 587).
    /// Required: a server that doesn't offer it fails the attempt.
    StartTls,
    /// No encryption (port 25 or a LAN relay). Credentials are allowed only
    /// to a plainly local host.
    None,
}

impl SmtpSecurity {
    /// The usual port for this mode.
    pub fn default_port(self) -> u16 {
        match self {
            SmtpSecurity::Tls => 465,
            SmtpSecurity::StartTls => 587,
            SmtpSecurity::None => 25,
        }
    }
}

/// An SMTP login.
#[derive(Debug, Clone)]
pub struct SmtpCredentials {
    pub username: Secret,
    pub password: Secret,
}

/// An email channel's settings.
#[derive(Debug, Clone)]
pub struct EmailConfig {
    /// The SMTP server's host name or IP address.
    pub host: String,
    pub port: u16,
    pub security: SmtpSecurity,
    pub credentials: Option<SmtpCredentials>,
    /// The sender address, such as `lodestar@example.com`.
    pub from: String,
    /// One or more recipient addresses.
    pub to: Vec<String>,
    /// Let this channel reach private-network addresses (a relay on the
    /// LAN). Off by default; loopback, link-local and metadata addresses
    /// stay refused either way.
    pub allow_private: bool,
}

impl EmailConfig {
    /// A channel on `host` with `security` on its usual port, no login and
    /// no private-network access.
    pub fn new(
        host: impl Into<String>,
        security: SmtpSecurity,
        from: impl Into<String>,
        to: Vec<String>,
    ) -> EmailConfig {
        EmailConfig {
            host: host.into(),
            port: security.default_port(),
            security,
            credentials: None,
            from: from.into(),
            to,
            allow_private: false,
        }
    }
}

pub struct Email {
    /// The host as `net::host_url` normalised it.
    host: Url,
    port: u16,
    security: SmtpSecurity,
    credentials: Option<SmtpCredentials>,
    from: Mailbox,
    to: Vec<Mailbox>,
    policy: NetPolicy,
    resolver: Arc<dyn Resolve>,
    /// A test CA, trusted instead of the platform's roots.
    #[cfg(test)]
    test_root: Option<Vec<u8>>,
}

impl fmt::Debug for Email {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Email")
            .field("host", &self.host.host_str().unwrap_or("?"))
            .field("port", &self.port)
            .field("security", &self.security)
            .field("login", &self.credentials.is_some())
            .field("recipients", &self.to.len())
            .finish_non_exhaustive()
    }
}

/// Which step of the session failed, to classify the error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Connect,
    StartTls,
    Login,
    Send,
}

impl Email {
    /// Checks the settings and builds the sender. A refused address, an
    /// invalid email address, or credentials that would cross a plaintext
    /// connection fail here, when the channel is saved.
    pub fn new(config: &EmailConfig) -> Result<Email, DeliveryError> {
        Email::with_resolver(config, Arc::new(SystemResolver))
    }

    /// The same, over a given resolver (tests simulate DNS this way).
    pub fn with_resolver(
        config: &EmailConfig,
        resolver: Arc<dyn Resolve>,
    ) -> Result<Email, DeliveryError> {
        Email::build(
            config,
            NetPolicy::notification(config.allow_private),
            resolver,
        )
    }

    fn build(
        config: &EmailConfig,
        policy: NetPolicy,
        resolver: Arc<dyn Resolve>,
    ) -> Result<Email, DeliveryError> {
        let invalid = |why| Err(DeliveryError::InvalidEndpoint(why));
        let host = check_host(&config.host, config.port, policy)
            .map_err(|e| AttemptError::from(e).error)?;
        if let Some(c) = &config.credentials {
            check_credential(&c.username, "invalid username")?;
            check_credential(&c.password, "invalid password")?;
            if config.security == SmtpSecurity::None
                && !(config.allow_private && is_local_host(&host))
            {
                return invalid(
                    "a login needs TLS or STARTTLS unless the server is a private-network address or a local name",
                );
            }
        }
        let Ok(from) = config.from.trim().parse::<Address>() else {
            return invalid("the sender address isn't valid");
        };
        if config.to.is_empty() {
            return invalid("add at least one recipient");
        }
        if config.to.len() > MAX_RECIPIENTS {
            return invalid("too many recipients");
        }
        let mut to = Vec::with_capacity(config.to.len());
        for raw in &config.to {
            let Ok(address) = raw.trim().parse::<Address>() else {
                return invalid("a recipient address isn't valid");
            };
            to.push(Mailbox::new(None, address));
        }
        Ok(Email {
            host,
            port: config.port,
            security: config.security,
            credentials: config.credentials.clone(),
            from: Mailbox::new(Some(FROM_NAME.to_owned()), from),
            to,
            policy,
            resolver,
            #[cfg(test)]
            test_root: None,
        })
    }

    /// The host name, unbracketed: what DNS and the certificate see.
    fn host_name(&self) -> &str {
        let host = self.host.host_str().unwrap_or_default();
        host.trim_start_matches('[').trim_end_matches(']')
    }

    /// The `Message-ID` for `n`: the same on every retry.
    fn message_id(&self, n: &Notification) -> String {
        let id: String = n
            .id()
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(64)
            .collect();
        format!("<{id}.lodestar@{}>", self.from.email.domain())
    }

    /// The whole message for `n`, as it goes on the wire (useful for a
    /// preview, and for tests).
    pub fn message(&self, n: &Notification) -> Result<Vec<u8>, DeliveryError> {
        Ok(self.build_message(n)?.1)
    }

    /// The message and its wire form, formatted once.
    fn build_message(&self, n: &Notification) -> Result<(Message, Vec<u8>), DeliveryError> {
        let failed = |_| DeliveryError::Failed("couldn't build the message");
        let part = |content_type: ContentType, text: String| {
            let body = Body::new_with_encoding(text, ContentTransferEncoding::Base64)
                .map_err(|_| DeliveryError::Failed("couldn't build the message"))?;
            Ok::<_, DeliveryError>(SinglePart::builder().header(content_type).body(body))
        };
        let mut builder = Message::builder()
            .from(self.from.clone())
            .subject(subject(n))
            .message_id(Some(self.message_id(n)));
        for to in &self.to {
            builder = builder.to(to.clone());
        }
        let body = MultiPart::alternative()
            .singlepart(part(ContentType::TEXT_PLAIN, text_part(n))?)
            .singlepart(part(ContentType::TEXT_HTML, html_part(n))?);
        let message = builder.multipart(body).map_err(failed)?;
        let bytes = message.formatted();
        if bytes.len() > MAX_MESSAGE_BYTES {
            return Err(DeliveryError::Failed("the message is too large"));
        }
        Ok((message, bytes))
    }

    fn tls(&self) -> Result<TlsParameters, AttemptError> {
        let failed = |_| AttemptError::fatal(DeliveryError::Failed("couldn't set up TLS"));
        #[allow(unused_mut)]
        let mut builder = TlsParameters::builder(self.host_name().to_owned());
        #[cfg(test)]
        if let Some(root) = &self.test_root {
            use lettre::transport::smtp::client::{Certificate, CertificateStore};
            builder = builder
                .certificate_store(CertificateStore::None)
                .add_root_certificate(Certificate::from_der(root.clone()).map_err(failed)?);
        }
        builder.build_rustls().map_err(failed)
    }

    /// One SMTP session: connect, secure, log in, send, quit.
    async fn session(&self, envelope: &Envelope, bytes: &[u8]) -> Result<(), AttemptError> {
        let hello = ClientId::default();
        let host = self.host_name();
        let mut conn = match self.security {
            SmtpSecurity::Tls => {
                let resolve = resolve_tcp(&*self.resolver, host, self.port, self.policy);
                let addrs = tokio::time::timeout(CONNECT_TIMEOUT, resolve)
                    .await
                    .map_err(|_| AttemptError::transient(DeliveryError::Timeout))??;
                AsyncSmtpConnection::connect_tokio1(
                    addrs.as_slice(),
                    Some(CONNECT_TIMEOUT),
                    &hello,
                    Some(self.tls()?),
                    None,
                )
                .await
                .map_err(|e| classify(Step::Connect, &e))?
            }
            SmtpSecurity::StartTls | SmtpSecurity::None => {
                let tcp = connect_tcp_with(
                    &*self.resolver,
                    host,
                    self.port,
                    self.policy,
                    CONNECT_TIMEOUT,
                )
                .await?;
                // A local-looking name can still resolve to a public
                // address (a search domain, a `.local` name served by
                // DNS): a plaintext login also needs the connected address
                // itself to be private.
                if self.security == SmtpSecurity::None
                    && self.credentials.is_some()
                    && !tcp
                        .peer_addr()
                        .is_ok_and(|a| classify_addr(a.ip()) == AddrClass::Private)
                {
                    return Err(AttemptError::fatal(DeliveryError::Failed(
                        "a login without TLS needs the mail server on a private-network address",
                    )));
                }
                let mut conn = AsyncSmtpConnection::connect_with_transport(Box::new(tcp), &hello)
                    .await
                    .map_err(|e| classify(Step::Connect, &e))?;
                if self.security == SmtpSecurity::StartTls {
                    if !conn.can_starttls() {
                        conn.abort().await;
                        return Err(AttemptError::fatal(DeliveryError::Failed(
                            "the mail server doesn't offer STARTTLS",
                        )));
                    }
                    conn.starttls(self.tls()?, &hello)
                        .await
                        .map_err(|e| classify(Step::StartTls, &e))?;
                }
                conn
            }
        };
        if self.security != SmtpSecurity::None && !conn.is_encrypted() {
            conn.abort().await;
            return Err(AttemptError::fatal(DeliveryError::Failed(
                "the connection to the mail server isn't encrypted",
            )));
        }
        if let Some(c) = &self.credentials {
            let login = Credentials::new(
                c.username.expose().to_owned(),
                c.password.expose().to_owned(),
            );
            let result = conn
                .auth(&[Mechanism::Plain, Mechanism::Login], &login)
                .await;
            drop(login);
            if let Err(e) = result {
                conn.abort().await;
                return Err(classify(Step::Login, &e));
            }
        }
        conn.send(envelope, bytes)
            .await
            .map_err(|e| classify(Step::Send, &e))?;
        // The message is accepted; a failed goodbye doesn't change that.
        let _ = conn.quit().await;
        Ok(())
    }
}

fn check_credential(secret: &Secret, why: &'static str) -> Result<(), DeliveryError> {
    let v = secret.expose();
    if v.is_empty() || v.chars().any(char::is_control) {
        return Err(DeliveryError::InvalidEndpoint(why));
    }
    Ok(())
}

/// Turns a lettre error into a delivery decision, keeping none of its text
/// (a server's reply can echo what it was sent).
fn classify(step: Step, e: &SmtpError) -> AttemptError {
    if let Some(code) = e.status() {
        let code = u16::from(code);
        // 535: authentication credentials invalid (RFC 4954).
        if step == Step::Login && code == 535 {
            return AttemptError::fatal(DeliveryError::Failed(
                "the mail server refused the login; check the username and password",
            ));
        }
        let error = DeliveryError::SmtpRejected { code };
        return if code < 500 {
            AttemptError::transient(error)
        } else {
            AttemptError::fatal(error)
        };
    }
    if is_certificate_error(e) {
        return AttemptError::fatal(DeliveryError::Failed(
            "the mail server's certificate didn't verify for its host name",
        ));
    }
    if e.is_timeout() {
        return AttemptError::transient(DeliveryError::Timeout);
    }
    if e.is_client() {
        return AttemptError::fatal(DeliveryError::Failed(match step {
            Step::Login => "the mail server offers no login method Lodestar supports",
            Step::StartTls => "the mail server doesn't offer STARTTLS",
            Step::Connect | Step::Send => "the mail server can't take this message",
        }));
    }
    // Everything else, a truncated or malformed reply included (lettre
    // reports a connection closed mid-session as an "incomplete response"),
    // is a connection problem worth retrying; the stable Message-ID lets
    // the receiver drop a duplicate if the message did get through.
    AttemptError::transient(DeliveryError::Unreachable)
}

/// Whether a TLS handshake failed on the certificate (a setting to fix,
/// not something a retry helps).
fn is_certificate_error(e: &SmtpError) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(e);
    while let Some(err) = source {
        // An `io::Error`'s `source` skips the error it wraps, so look inside.
        let inner = err
            .downcast_ref::<std::io::Error>()
            .and_then(|io| io.get_ref())
            .map(|e| e as &(dyn std::error::Error + 'static));
        for candidate in [Some(err), inner].into_iter().flatten() {
            if let Some(tls) = candidate.downcast_ref::<rustls::Error>() {
                return matches!(
                    tls,
                    rustls::Error::InvalidCertificate(_) | rustls::Error::NoCertificatesPresented
                );
            }
        }
        source = err.source();
    }
    false
}

/// The Subject: the title on one line, without controls, or "Lodestar".
/// `=?` is broken up, so text from a name can't be an RFC 2047
/// encoded-word that a mail reader decodes into a bidi override or a line
/// break after the cleaning.
fn subject(n: &Notification) -> String {
    let title = clean_line(&n.title).replace("=?", "= ?");
    let title = truncate_chars(&title, MAX_SUBJECT_CHARS);
    if title.is_empty() {
        FROM_NAME.to_owned()
    } else {
        title
    }
}

fn link(n: &Notification) -> Option<String> {
    n.link
        .as_deref()
        .and_then(web_link)
        .filter(|l| l.len() <= MAX_LINK_BYTES)
}

/// The plain-text part: the body, the shareable items and the notes, then
/// the link back into Lodestar.
fn text_part(n: &Notification) -> String {
    let mut text = compose(
        n,
        |s| s.to_owned(),
        MAX_TEXT_BYTES,
        str::len,
        truncate_bytes,
    );
    if let Some(link) = link(n) {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str("Open in Lodestar: ");
        text.push_str(&link);
    }
    text.push('\n');
    text
}

/// The HTML part: the same text, escaped, in one pre-wrapped block, and the
/// link. No images, remote resources, scripts or external styles.
fn html_part(n: &Notification) -> String {
    let text = compose(n, escape_html, MAX_HTML_BYTES, str::len, truncate_html);
    let mut html = String::with_capacity(text.len() + 512);
    html.push_str("<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"><title>");
    html.push_str(&escape_html(&subject(n)));
    html.push_str("</title></head>\n<body>\n<div style=\"white-space:pre-wrap\">");
    html.push_str(&text);
    html.push_str("</div>\n");
    if let Some(link) = link(n) {
        let link = escape_html(&link);
        html.push_str("<p><a href=\"");
        html.push_str(&link);
        html.push_str("\">Open in Lodestar</a></p>\n");
    }
    html.push_str("</body></html>\n");
    html
}

/// `s` with the characters HTML treats as markup written as entities.
fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// [`truncate_bytes`] for escaped HTML: an entity cut in half is dropped
/// rather than left as stray text.
fn truncate_html(s: &str, max: usize) -> String {
    let mut out = truncate_bytes(s, max);
    if out.len() < s.len() {
        let kept = out.trim_end_matches('…');
        if let Some(amp) = kept.rfind('&')
            && !kept[amp..].contains(';')
        {
            out.truncate(amp);
            out.push('…');
        }
    }
    out
}

impl Notifier for Email {
    fn channel(&self) -> Channel {
        Channel::Email
    }

    fn attempt<'a>(&'a self, n: &'a Notification) -> BoxFuture<'a, Result<(), AttemptError>> {
        Box::pin(async move {
            let (message, bytes) = self.build_message(n).map_err(AttemptError::fatal)?;
            let session = self.session(message.envelope(), &bytes);
            let result = tokio::time::timeout(EMAIL_TIMEOUT, session)
                .await
                .unwrap_or(Err(AttemptError::transient(DeliveryError::Timeout)));
            tracing::debug!(
                host = self.host.host_str().unwrap_or("?"),
                port = self.port,
                ok = result.is_ok(),
                "email attempt"
            );
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::tests::ScriptedResolver;
    use crate::notify::{NotificationItem, NotificationKind, RetryPolicy, deliver};
    use crate::{AddrClass, NetError};
    use std::sync::Mutex;
    use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

    const USER: &str = "USERSECRETname";
    const PASS: &str = "PASSSECRETword";
    /// base64("\0USERSECRETname\0PASSSECRETword"), base64 of each alone.
    const PLAIN: &str = "AFVTRVJTRUNSRVRuYW1lAFBBU1NTRUNSRVR3b3Jk";
    const USER_B64: &str = "VVNFUlNFQ1JFVG5hbWU=";
    const PASS_B64: &str = "UEFTU1NFQ1JFVHdvcmQ=";

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!("{}/tests/fixtures/email/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    /// How the test server behaves on one connection.
    #[derive(Debug, Clone)]
    struct Script {
        implicit_tls: bool,
        starttls: bool,
        /// The AUTH mechanisms advertised, or empty for none.
        auth: &'static str,
        accept_login: bool,
        mail_reply: &'static str,
        data_reply: &'static str,
        /// The reply to AUTH PLAIN in place of the credential check.
        auth_reply: Option<&'static str>,
        /// Close the connection at this point without a reply.
        drop: Drop,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Drop {
        Never,
        BeforeGreeting,
        AfterEhlo,
        AfterData,
    }

    impl Default for Script {
        fn default() -> Script {
            Script {
                implicit_tls: false,
                starttls: true,
                auth: "PLAIN LOGIN",
                accept_login: true,
                mail_reply: "250 2.1.0 OK",
                data_reply: "250 2.0.0 queued",
                auth_reply: None,
                drop: Drop::Never,
            }
        }
    }

    /// What arrived on one connection.
    #[derive(Debug, Default, Clone)]
    struct Session {
        commands: Vec<String>,
        /// Which commands arrived after TLS was up.
        encrypted: Vec<bool>,
        data: String,
    }

    impl Session {
        fn verbs(&self) -> Vec<String> {
            self.commands
                .iter()
                .map(|c| c.split([' ', ':']).next().unwrap().to_owned())
                .collect()
        }

        fn rcpts(&self) -> Vec<&str> {
            self.commands
                .iter()
                .filter_map(|c| c.strip_prefix("RCPT TO:"))
                .collect()
        }
    }

    type Log = Arc<Mutex<Vec<Session>>>;

    /// A scripted SMTP server on loopback: connection `i` follows
    /// `scripts[i]` (the last one repeats).
    async fn smtp_server(scripts: Vec<Script>) -> (u16, Log) {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(fixture("server.der"))],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(fixture("server.key.der"))),
            )
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let log: Log = Arc::default();
        let sessions = Arc::clone(&log);
        tokio::spawn(async move {
            for i in 0.. {
                let Ok((tcp, _)) = listener.accept().await else {
                    return;
                };
                let script = scripts[i.min(scripts.len() - 1)].clone();
                let acceptor = acceptor.clone();
                let index = {
                    let mut s = sessions.lock().unwrap();
                    s.push(Session::default());
                    s.len() - 1
                };
                let sessions = Arc::clone(&sessions);
                tokio::spawn(async move {
                    let mut session = Session::default();
                    if script.drop == Drop::BeforeGreeting {
                        drop(tcp);
                    } else if script.implicit_tls {
                        if let Ok(tls) = acceptor.accept(tcp).await {
                            let mut io = BufReader::new(tls);
                            if greet(&mut io).await {
                                run(&mut io, &script, &mut session, true).await;
                            }
                        }
                    } else {
                        let mut io = BufReader::new(tcp);
                        if greet(&mut io).await
                            && run(&mut io, &script, &mut session, false).await
                            && let Ok(tls) = acceptor.accept(io.into_inner()).await
                        {
                            let mut io = BufReader::new(tls);
                            run(&mut io, &script, &mut session, true).await;
                        }
                    }
                    sessions.lock().unwrap()[index] = session;
                });
            }
        });
        (port, log)
    }

    async fn greet<S: AsyncRead + AsyncWrite + Unpin>(io: &mut BufReader<S>) -> bool {
        io.write_all(b"220 mail.test ESMTP test\r\n").await.is_ok()
    }

    async fn reply<S: AsyncRead + AsyncWrite + Unpin>(io: &mut BufReader<S>, text: &str) -> bool {
        io.write_all(format!("{text}\r\n").as_bytes()).await.is_ok()
    }

    async fn line<S: AsyncRead + AsyncWrite + Unpin>(io: &mut BufReader<S>) -> Option<String> {
        let mut buf = String::new();
        match io.read_line(&mut buf).await {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(buf.trim_end_matches(['\r', '\n']).to_owned()),
        }
    }

    /// Plays one script until QUIT or a closed connection. Returns true
    /// when the client asked for STARTTLS (and was told to go ahead).
    async fn run<S: AsyncRead + AsyncWrite + Unpin>(
        io: &mut BufReader<S>,
        script: &Script,
        session: &mut Session,
        tls: bool,
    ) -> bool {
        while let Some(cmd) = line(io).await {
            session.commands.push(cmd.clone());
            session.encrypted.push(tls);
            let upper = cmd.to_ascii_uppercase();
            if script.drop == Drop::AfterEhlo && upper.starts_with("EHLO") {
                return false;
            }
            let ok = if upper.starts_with("EHLO") {
                let mut lines = vec!["250-mail.test".to_owned()];
                if script.starttls && !tls {
                    lines.push("250-STARTTLS".into());
                }
                if !script.auth.is_empty() {
                    lines.push(format!("250-AUTH {}", script.auth));
                }
                lines.push("250 8BITMIME".into());
                reply(io, &lines.join("\r\n")).await
            } else if upper == "STARTTLS" {
                return reply(io, "220 2.0.0 go ahead").await;
            } else if let (Some(text), true) = (script.auth_reply, upper.starts_with("AUTH")) {
                reply(io, text).await
            } else if let Some(given) = cmd.strip_prefix("AUTH PLAIN ") {
                let good = script.accept_login && given == PLAIN;
                reply(
                    io,
                    if good {
                        "235 2.7.0 OK"
                    } else {
                        "535 5.7.8 bad credentials"
                    },
                )
                .await
            } else if upper.starts_with("AUTH LOGIN") {
                // A username may come with the command or after a prompt.
                let user = match cmd.strip_prefix("AUTH LOGIN ") {
                    Some(u) => Some(u.to_owned()),
                    None => {
                        reply(io, "334 VXNlcm5hbWU6").await;
                        line(io).await
                    }
                };
                reply(io, "334 UGFzc3dvcmQ6").await;
                let pass = line(io).await;
                let good = script.accept_login
                    && user.as_deref() == Some(USER_B64)
                    && pass.as_deref() == Some(PASS_B64);
                reply(
                    io,
                    if good {
                        "235 2.7.0 OK"
                    } else {
                        "535 5.7.8 bad credentials"
                    },
                )
                .await
            } else if upper.starts_with("MAIL FROM:") {
                reply(io, script.mail_reply).await
            } else if upper.starts_with("RCPT TO:") {
                reply(io, "250 2.1.5 OK").await
            } else if upper == "DATA" {
                reply(io, "354 go ahead").await;
                while let Some(l) = line(io).await {
                    if l == "." {
                        break;
                    }
                    session.data.push_str(&l);
                    session.data.push('\n');
                }
                if script.drop == Drop::AfterData {
                    return false;
                }
                reply(io, script.data_reply).await
            } else if upper == "QUIT" {
                reply(io, "221 2.0.0 bye").await;
                return false;
            } else {
                reply(io, "502 5.5.2 unknown command").await
            };
            if !ok {
                break;
            }
        }
        false
    }

    fn resolver() -> Arc<ScriptedResolver> {
        ScriptedResolver::new(&[
            ("mail.test", &[&["127.0.0.1"]]),
            ("other.test", &[&["127.0.0.1"]]),
            ("metadata.test", &[&["169.254.169.254"]]),
        ])
    }

    fn config(port: u16, security: SmtpSecurity) -> EmailConfig {
        let mut c = EmailConfig::new(
            "mail.test",
            security,
            "lodestar@example.com",
            vec!["me@example.com".into(), "you@example.org".into()],
        );
        c.port = port;
        c.credentials = Some(SmtpCredentials {
            username: Secret::new(USER),
            password: Secret::new(PASS),
        });
        c
    }

    /// A sender that may reach loopback and trusts the test CA.
    fn email(config: &EmailConfig) -> Email {
        let mut e = Email::build(config, NetPolicy::loopback_for_tests(), resolver()).unwrap();
        e.test_root = Some(fixture("ca.der"));
        e
    }

    fn alert() -> Notification {
        let mut n = Notification::new(
            NotificationKind::Alert,
            "4K nature docs: 2 new",
            "Two new results.",
        );
        n.link = Some("http://lodestar.lan/alerts/7".into());
        n.items = vec![
            NotificationItem {
                name: "Planet.Earth.III.2160p".into(),
                info_hash: "a".repeat(40),
                size_bytes: Some(4_692_251_770),
                private_source: false,
            },
            NotificationItem {
                name: "Private.Tracker.Exclusive".into(),
                info_hash: "b".repeat(40),
                size_bytes: Some(7),
                private_source: true,
            },
        ];
        n
    }

    const FAST: RetryPolicy = RetryPolicy {
        max_attempts: 3,
        base_delay: Duration::from_millis(2),
        max_delay: Duration::from_millis(20),
    };

    /// The decoded parts of a message: headers, then each base64 part.
    fn decode_parts(data: &str) -> (String, Vec<String>) {
        let (headers, rest) = data.split_once("\n\n").unwrap();
        let mut parts = Vec::new();
        for chunk in rest.split("Content-Transfer-Encoding: base64\n\n").skip(1) {
            let b64: String = chunk
                .lines()
                .take_while(|l| !l.is_empty() && !l.starts_with("--"))
                .collect();
            parts.push(String::from_utf8(base64_decode(&b64)).unwrap());
        }
        (headers.to_owned(), parts)
    }

    fn base64_decode(s: &str) -> Vec<u8> {
        let value = |c: u8| match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => panic!("not base64: {c}"),
        };
        let bytes: Vec<u8> = s.bytes().filter(|&b| b != b'=').map(value).collect();
        let mut out = Vec::new();
        for chunk in bytes.chunks(4) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |acc, (i, &v)| acc | u32::from(v) << (18 - 6 * i));
            for i in 0..chunk.len() - 1 {
                out.push((n >> (16 - 8 * i)) as u8);
            }
        }
        out
    }

    #[tokio::test]
    async fn starttls_then_auth_plain_then_the_message() {
        let (port, log) = smtp_server(vec![Script::default()]).await;
        let n = alert();
        let out = deliver(&email(&config(port, SmtpSecurity::StartTls)), &n, &FAST).await;
        assert_eq!(out.result, Ok(()));
        assert_eq!(out.attempts, 1);
        assert_eq!(out.channel, Channel::Email);
        let s = log.lock().unwrap()[0].clone();
        assert_eq!(
            s.verbs(),
            [
                "EHLO", "STARTTLS", "EHLO", "AUTH", "MAIL", "RCPT", "RCPT", "DATA", "QUIT"
            ]
        );
        // Nothing but EHLO and STARTTLS crossed the plaintext connection.
        assert_eq!(s.encrypted[..2], [false, false]);
        assert!(s.encrypted[2..].iter().all(|&e| e));
        assert_eq!(s.commands[3], format!("AUTH PLAIN {PLAIN}"));
        assert_eq!(s.commands[4], "MAIL FROM:<lodestar@example.com>");
        assert_eq!(s.rcpts(), ["<me@example.com>", "<you@example.org>"]);

        let (headers, parts) = decode_parts(&s.data);
        assert!(
            headers.contains("From: Lodestar <lodestar@example.com>"),
            "{headers}"
        );
        assert!(
            headers.contains("To: me@example.com, you@example.org"),
            "{headers}"
        );
        assert!(
            headers.contains("Subject: 4K nature docs: 2 new"),
            "{headers}"
        );
        assert!(
            headers.contains(&format!("Message-ID: <{}.lodestar@example.com>", n.id())),
            "{headers}"
        );
        assert!(
            headers.contains("Content-Type: multipart/alternative"),
            "{headers}"
        );
        assert!(headers.contains("Date: "), "{headers}");
        assert!(!s.data.contains("Bcc"));
        assert_eq!(parts.len(), 2);
        assert_eq!(
            parts[0],
            "Two new results.\r\n\r\n• Planet.Earth.III.2160p (4.37\u{a0}GiB)\r\n\r\n\
             1 result from a private source isn't included.\r\n\r\n\
             Open in Lodestar: http://lodestar.lan/alerts/7\r\n"
        );
        assert!(parts[1].starts_with("<!DOCTYPE html>"), "{}", parts[1]);
        assert!(parts[1].contains("• Planet.Earth.III.2160p"));
        assert!(parts[1].contains("<a href=\"http://lodestar.lan/alerts/7\">"));
        assert!(parts[1].contains("1 result from a private source isn't included."));
        assert!(!s.data.contains("Private.Tracker"));
        let html = &parts[1].to_ascii_lowercase();
        for banned in ["<img", "<script", "src=", "<link", "url("] {
            assert!(!html.contains(banned), "{banned}");
        }
    }

    #[tokio::test]
    async fn implicit_tls_with_auth_login() {
        let (port, log) = smtp_server(vec![Script {
            implicit_tls: true,
            starttls: false,
            auth: "LOGIN",
            ..Script::default()
        }])
        .await;
        let out = deliver(&email(&config(port, SmtpSecurity::Tls)), &alert(), &FAST).await;
        assert_eq!(out.result, Ok(()));
        let s = log.lock().unwrap()[0].clone();
        assert_eq!(
            s.verbs(),
            ["EHLO", "AUTH", "MAIL", "RCPT", "RCPT", "DATA", "QUIT"]
        );
        assert!(s.encrypted.iter().all(|&e| e));
        assert!(s.commands[1].starts_with("AUTH LOGIN"));
    }

    #[tokio::test]
    async fn starttls_is_required_never_opportunistic() {
        let (port, log) = smtp_server(vec![Script {
            starttls: false,
            ..Script::default()
        }])
        .await;
        let out = deliver(
            &email(&config(port, SmtpSecurity::StartTls)),
            &alert(),
            &FAST,
        )
        .await;
        assert_eq!(
            out.result,
            Err(DeliveryError::Failed(
                "the mail server doesn't offer STARTTLS"
            ))
        );
        assert_eq!(out.attempts, 1);
        let s = log.lock().unwrap()[0].clone();
        assert!(
            !s.verbs().iter().any(|v| v == "AUTH" || v == "MAIL"),
            "{s:?}"
        );
    }

    #[tokio::test]
    async fn the_certificate_must_match_the_configured_host() {
        let (port, log) = smtp_server(vec![Script::default()]).await;
        let mut cfg = config(port, SmtpSecurity::StartTls);
        cfg.host = "other.test".into();
        let out = deliver(&email(&cfg), &alert(), &FAST).await;
        assert_eq!(
            out.result,
            Err(DeliveryError::Failed(
                "the mail server's certificate didn't verify for its host name"
            ))
        );
        assert_eq!(out.attempts, 1);
        let s = log.lock().unwrap()[0].clone();
        assert!(!s.verbs().iter().any(|v| v == "AUTH"), "{s:?}");

        // And with implicit TLS, an untrusted CA is refused the same way.
        let (port, _) = smtp_server(vec![Script {
            implicit_tls: true,
            ..Script::default()
        }])
        .await;
        let mut sender = email(&config(port, SmtpSecurity::Tls));
        sender.test_root = Some(fixture("server.der"));
        let out = sender.attempt(&alert()).await.unwrap_err();
        assert!(!out.retry);
    }

    #[tokio::test]
    async fn a_4xx_is_retried_with_the_same_message_id() {
        let (port, log) = smtp_server(vec![
            Script {
                mail_reply: "451 4.3.0 try again later",
                ..Script::default()
            },
            Script::default(),
        ])
        .await;
        let n = alert();
        let out = deliver(&email(&config(port, SmtpSecurity::StartTls)), &n, &FAST).await;
        assert_eq!(out.result, Ok(()));
        assert_eq!(out.attempts, 2);
        let sessions = log.lock().unwrap().clone();
        assert!(sessions[0].data.is_empty());
        let id = format!("Message-ID: <{}.lodestar@example.com>", n.id());
        assert!(sessions[1].data.contains(&id));

        let (port, _) = smtp_server(vec![Script {
            data_reply: "452 4.3.1 insufficient storage",
            ..Script::default()
        }])
        .await;
        let out = deliver(&email(&config(port, SmtpSecurity::StartTls)), &n, &FAST).await;
        assert_eq!(out.result, Err(DeliveryError::SmtpRejected { code: 452 }));
        assert_eq!(out.attempts, 3);
    }

    #[tokio::test]
    async fn a_5xx_is_final() {
        let (port, log) = smtp_server(vec![Script {
            data_reply: "554 5.7.1 message rejected",
            ..Script::default()
        }])
        .await;
        let out = deliver(
            &email(&config(port, SmtpSecurity::StartTls)),
            &alert(),
            &FAST,
        )
        .await;
        assert_eq!(out.result, Err(DeliveryError::SmtpRejected { code: 554 }));
        assert_eq!(out.attempts, 1);
        assert_eq!(log.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_refused_login_is_final_and_names_no_credential() {
        for auth in ["PLAIN", "LOGIN"] {
            let (port, log) = smtp_server(vec![Script {
                auth,
                accept_login: false,
                ..Script::default()
            }])
            .await;
            let out = deliver(
                &email(&config(port, SmtpSecurity::StartTls)),
                &alert(),
                &FAST,
            )
            .await;
            let err = out.result.unwrap_err();
            assert_eq!(
                err,
                DeliveryError::Failed(
                    "the mail server refused the login; check the username and password"
                )
            );
            assert_eq!(out.attempts, 1, "{auth}");
            let text = format!("{err} {err:?}");
            assert!(!text.contains(USER) && !text.contains(PASS));
            let s = log.lock().unwrap()[0].clone();
            assert!(!s.verbs().iter().any(|v| v == "MAIL"), "{s:?}");
        }
    }

    #[tokio::test]
    async fn no_login_method_in_common_is_final() {
        let (port, _) = smtp_server(vec![Script {
            auth: "CRAM-MD5",
            ..Script::default()
        }])
        .await;
        let out = deliver(
            &email(&config(port, SmtpSecurity::StartTls)),
            &alert(),
            &FAST,
        )
        .await;
        assert_eq!(
            out.result,
            Err(DeliveryError::Failed(
                "the mail server offers no login method Lodestar supports"
            ))
        );
        assert_eq!(out.attempts, 1);
    }

    #[tokio::test]
    async fn names_cant_add_headers_or_recipients() {
        let (port, log) = smtp_server(vec![Script::default()]).await;
        let mut n = alert();
        n.title = "Alert\r\nBcc: attacker@example.com\r\nX-Evil: 1".into();
        n.items[0].name = "Name\r\nBcc: attacker@example.com\n<b>bold</b>".into();
        n.body = "Body\r\n\r\nTo: attacker@example.com".into();
        let out = deliver(&email(&config(port, SmtpSecurity::StartTls)), &n, &FAST).await;
        assert_eq!(out.result, Ok(()));
        let s = log.lock().unwrap()[0].clone();
        assert_eq!(s.rcpts(), ["<me@example.com>", "<you@example.org>"]);
        let (headers, parts) = decode_parts(&s.data);
        for line in headers.lines() {
            let name = line.split(':').next().unwrap().to_ascii_lowercase();
            assert!(
                !matches!(name.as_str(), "bcc" | "x-evil" | "cc"),
                "an injected header: {headers}"
            );
            assert!(!line.starts_with("To: attacker"), "{headers}");
        }
        assert_eq!(
            headers.matches("\nTo: ").count() + usize::from(headers.starts_with("To: ")),
            1
        );
        assert!(
            headers.contains("Subject: Alert  Bcc: attacker@example.com  X-Evil: 1"),
            "{headers}"
        );
        assert!(parts[0].contains("• Name  Bcc: attacker@example.com <b>bold</b>"));
        assert!(parts[1].contains("• Name  Bcc: attacker@example.com &lt;b&gt;bold&lt;/b&gt;"));
        assert!(!parts[1].contains("<b>"));
    }

    #[tokio::test]
    async fn non_ascii_subjects_are_encoded() {
        let (port, log) = smtp_server(vec![Script::default()]).await;
        let mut n = alert();
        n.title = "Ночь: 日本語 \u{202e}gpj.exe".into();
        let out = deliver(&email(&config(port, SmtpSecurity::StartTls)), &n, &FAST).await;
        assert_eq!(out.result, Ok(()));
        let data = log.lock().unwrap()[0].data.clone();
        assert!(data.is_ascii());
        let (headers, _) = decode_parts(&data);
        assert!(headers.contains("Subject: =?utf-8?b?"), "{headers}");
    }

    #[tokio::test]
    async fn plaintext_without_a_login_works_and_connection_failures_are_retried() {
        let (port, log) = smtp_server(vec![Script {
            starttls: false,
            auth: "",
            ..Script::default()
        }])
        .await;
        let mut cfg = config(port, SmtpSecurity::None);
        cfg.credentials = None;
        let out = deliver(&email(&cfg), &alert(), &FAST).await;
        assert_eq!(out.result, Ok(()));
        assert_eq!(
            log.lock().unwrap()[0].verbs(),
            ["EHLO", "MAIL", "RCPT", "RCPT", "DATA", "QUIT"]
        );

        // Nothing listening.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let out = deliver(
            &email(&config(port, SmtpSecurity::StartTls)),
            &alert(),
            &FAST,
        )
        .await;
        assert_eq!(out.result, Err(DeliveryError::Unreachable));
        assert_eq!(out.attempts, 3);
    }

    #[tokio::test]
    async fn a_name_resolving_to_metadata_is_never_connected() {
        let mut cfg = config(25, SmtpSecurity::StartTls);
        cfg.host = "metadata.test".into();
        let r = resolver();
        let sender = Email::build(&cfg, NetPolicy::notification(true), r.clone()).unwrap();
        let out = deliver(&sender, &alert(), &FAST).await;
        assert_eq!(out.result, Err(DeliveryError::Blocked(AddrClass::Metadata)));
        assert_eq!(out.attempts, 1);

        // The production policy never reaches loopback, even with private
        // ranges allowed, for implicit TLS too.
        let (port, log) = smtp_server(vec![Script::default()]).await;
        for security in [SmtpSecurity::StartTls, SmtpSecurity::Tls] {
            let mut cfg = config(port, security);
            cfg.allow_private = true;
            let sender = Email::with_resolver(&cfg, resolver()).unwrap();
            let out = deliver(&sender, &alert(), &FAST).await;
            assert_eq!(out.result, Err(DeliveryError::Blocked(AddrClass::Loopback)));
        }
        assert!(log.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_dropped_connection_is_retried() {
        for point in [Drop::BeforeGreeting, Drop::AfterEhlo, Drop::AfterData] {
            let (port, log) = smtp_server(vec![
                Script {
                    drop: point,
                    ..Script::default()
                },
                Script::default(),
            ])
            .await;
            let n = alert();
            let out = deliver(&email(&config(port, SmtpSecurity::StartTls)), &n, &FAST).await;
            assert_eq!(out.result, Ok(()), "{point:?}");
            assert_eq!(out.attempts, 2, "{point:?}");
            let sessions = log.lock().unwrap().clone();
            let id = format!("Message-ID: <{}.lodestar@example.com>", n.id());
            assert!(sessions[1].data.contains(&id), "{point:?}");
            if point == Drop::AfterData {
                // The retry carries the same ID, so a receiver that did
                // take the first copy can drop the second.
                assert!(sessions[0].data.contains(&id));
            }
        }
        // Every attempt dropped: retried to the limit, then unreachable.
        let (port, _) = smtp_server(vec![Script {
            drop: Drop::AfterEhlo,
            ..Script::default()
        }])
        .await;
        let out = deliver(
            &email(&config(port, SmtpSecurity::StartTls)),
            &alert(),
            &FAST,
        )
        .await;
        assert_eq!(out.result, Err(DeliveryError::Unreachable));
        assert_eq!(out.attempts, 3);
    }

    #[tokio::test]
    async fn encoded_words_in_a_title_stay_text() {
        let (port, log) = smtp_server(vec![Script::default()]).await;
        let mut n = alert();
        for title in [
            "Movie =?utf-8?b?4oCu?=vkm.exe",
            "x =?utf-8?b?DQpCY2M6IGF0dGFja2VyQGV4YW1wbGUuY29t?=",
            "=?UTF-8?Q?=E2=80=AE?=",
        ] {
            n.title = title.into();
            assert!(!subject(&n).contains("=?"), "{title}");
            let out = deliver(&email(&config(port, SmtpSecurity::StartTls)), &n, &FAST).await;
            assert_eq!(out.result, Ok(()));
        }
        assert_eq!(subject(&n), "= ?UTF-8?Q?=E2=80=AE?=");
        for s in log.lock().unwrap().iter() {
            let (headers, _) = decode_parts(&s.data);
            let subject = headers
                .lines()
                .skip_while(|l| !l.starts_with("Subject:"))
                .take_while(|l| l.starts_with("Subject:") || l.starts_with(' '))
                .collect::<String>();
            assert!(!subject.contains("=?utf-8?b?4oCu"), "{subject}");
            assert!(!subject.contains("=?utf-8?b?DQpC"), "{subject}");
            assert!(!subject.contains("=?UTF-8?Q?=E2"), "{subject}");
        }
    }

    #[tokio::test]
    async fn a_plaintext_login_needs_a_private_peer() {
        // `relay.lan` looks local but resolves elsewhere (here loopback,
        // the only address a test can reach): no AUTH is sent.
        let (port, log) = smtp_server(vec![Script {
            starttls: false,
            ..Script::default()
        }])
        .await;
        let mut cfg = config(port, SmtpSecurity::None);
        cfg.host = "relay.lan".into();
        cfg.allow_private = true;
        let r = ScriptedResolver::new(&[("relay.lan", &[&["127.0.0.1"]])]);
        let sender = Email::build(&cfg, NetPolicy::loopback_for_tests(), r).unwrap();
        let out = deliver(&sender, &alert(), &FAST).await;
        assert_eq!(
            out.result,
            Err(DeliveryError::Failed(
                "a login without TLS needs the mail server on a private-network address"
            ))
        );
        assert_eq!(out.attempts, 1);
        for s in log.lock().unwrap().iter() {
            assert!(!s.verbs().iter().any(|v| v == "AUTH"), "{s:?}");
        }
    }

    #[tokio::test]
    async fn only_a_535_blames_the_credentials() {
        for (text, want) in [
            (
                "535 5.7.8 bad credentials",
                DeliveryError::Failed(
                    "the mail server refused the login; check the username and password",
                ),
            ),
            (
                "530 5.7.0 must issue STARTTLS first",
                DeliveryError::SmtpRejected { code: 530 },
            ),
            (
                "534 5.7.9 mechanism too weak",
                DeliveryError::SmtpRejected { code: 534 },
            ),
        ] {
            let (port, _) = smtp_server(vec![Script {
                auth_reply: Some(text),
                ..Script::default()
            }])
            .await;
            let out = deliver(
                &email(&config(port, SmtpSecurity::StartTls)),
                &alert(),
                &FAST,
            )
            .await;
            assert_eq!(out.result, Err(want), "{text}");
            assert_eq!(out.attempts, 1, "{text}");
        }
        let (port, _) = smtp_server(vec![
            Script {
                auth_reply: Some("454 4.7.0 temporary failure"),
                ..Script::default()
            },
            Script::default(),
        ])
        .await;
        let out = deliver(
            &email(&config(port, SmtpSecurity::StartTls)),
            &alert(),
            &FAST,
        )
        .await;
        assert_eq!(out.result, Ok(()));
        assert_eq!(out.attempts, 2);
    }

    #[test]
    fn settings_are_checked_when_saved() {
        let saved = |edit: &dyn Fn(&mut EmailConfig)| {
            let mut cfg = config(587, SmtpSecurity::StartTls);
            edit(&mut cfg);
            Email::with_resolver(&cfg, resolver())
        };
        let invalid = |edit: &dyn Fn(&mut EmailConfig)| {
            matches!(saved(edit), Err(DeliveryError::InvalidEndpoint(_)))
        };
        assert!(saved(&|_| {}).is_ok());
        assert!(invalid(&|c| c.from = "not an address".into()));
        assert!(invalid(&|c| c.from = "Name <a@example.com>".into()));
        assert!(invalid(
            &|c| c.from = "a@example.com\r\nBcc: x@example.com".into()
        ));
        assert!(invalid(&|c| c.to = vec![]));
        assert!(invalid(
            &|c| c.to = vec!["a@example.com, b@example.com".into()]
        ));
        assert!(invalid(
            &|c| c.to = vec!["a@example.com\nBcc: b@example.com".into()]
        ));
        assert!(invalid(
            &|c| c.to = vec!["x@example.com".into(); MAX_RECIPIENTS + 1]
        ));
        assert!(invalid(&|c| c.host = String::new()));
        assert!(invalid(&|c| c.host = "mail.test/x".into()));
        assert!(invalid(&|c| c.host = "user@mail.test".into()));
        assert!(invalid(&|c| c.port = 0));
        assert!(invalid(&|c| {
            c.credentials = Some(SmtpCredentials {
                username: Secret::new(""),
                password: Secret::new("p"),
            })
        }));
        assert!(invalid(&|c| {
            c.credentials = Some(SmtpCredentials {
                username: Secret::new("u"),
                password: Secret::new("p\r\nQUIT"),
            })
        }));
        for (host, private, want) in [
            ("127.0.0.1", true, AddrClass::Loopback),
            ("0x7f.1", true, AddrClass::Loopback),
            ("2130706433", true, AddrClass::Loopback),
            ("::1", true, AddrClass::Loopback),
            ("[::ffff:127.0.0.1]", true, AddrClass::Loopback),
            ("169.254.169.254", true, AddrClass::Metadata),
            ("fd00:ec2::254", true, AddrClass::Metadata),
            ("192.168.1.5", false, AddrClass::Private),
        ] {
            let mut cfg = config(587, SmtpSecurity::StartTls);
            cfg.host = host.into();
            cfg.allow_private = private;
            assert_eq!(
                Email::new(&cfg).unwrap_err(),
                DeliveryError::Blocked(want),
                "{host}"
            );
        }
    }

    #[test]
    fn a_login_needs_tls_unless_the_host_is_plainly_local() {
        let saved = |host: &str, private: bool, security: SmtpSecurity| {
            let mut cfg = config(25, security);
            cfg.host = host.into();
            cfg.allow_private = private;
            Email::with_resolver(&cfg, resolver()).is_ok()
        };
        for local in [
            "192.168.1.5",
            "10.0.0.2",
            "relay",
            "relay.lan",
            "Relay.Local",
            "relay.home.arpa",
        ] {
            assert!(saved(local, true, SmtpSecurity::None), "{local}");
            assert!(
                !saved(local, false, SmtpSecurity::None),
                "{local} without allow_private"
            );
        }
        for remote in ["smtp.example.com", "relay.lan.example.com", "93.184.216.34"] {
            assert!(!saved(remote, true, SmtpSecurity::None), "{remote}");
            assert!(saved(remote, false, SmtpSecurity::StartTls), "{remote}");
            assert!(saved(remote, false, SmtpSecurity::Tls), "{remote}");
        }
        // Without a login, plaintext is allowed anywhere the policy is.
        let mut cfg = config(25, SmtpSecurity::None);
        cfg.host = "smtp.example.com".into();
        cfg.credentials = None;
        assert!(Email::with_resolver(&cfg, resolver()).is_ok());
    }

    #[test]
    fn huge_digests_stay_under_the_cap_and_keep_the_private_note() {
        let mut n = Notification::new(NotificationKind::Digest, "Digest", "é".repeat(200_000));
        n.items = (0..40)
            .map(|i| NotificationItem {
                name: format!("{i:02} {}", "<&>\"'".repeat(2000)),
                info_hash: format!("{i:040x}"),
                size_bytes: Some(1 << 30),
                private_source: i % 5 == 0,
            })
            .collect();
        let sender = email(&config(587, SmtpSecurity::StartTls));
        let message = sender.message(&n).unwrap();
        assert!(message.len() <= MAX_MESSAGE_BYTES, "{}", message.len());
        let note = "8 results from private sources aren't included.";
        let text = text_part(&n);
        assert!(text.len() <= MAX_TEXT_BYTES + 1);
        assert!(
            text.ends_with(&format!("{note}\n")),
            "{}",
            &text[text.len() - 200..]
        );
        let html = html_part(&n);
        assert!(html.contains(note));
        assert!(html.contains('…'));
    }

    #[test]
    fn truncated_html_never_ends_inside_an_entity() {
        let escaped = escape_html("a<b>&c");
        assert_eq!(escaped, "a&lt;b&gt;&amp;c");
        assert_eq!(truncate_html(&escaped, 5), "a…");
        assert_eq!(truncate_html(&escaped, 9), "a&lt;b…");
        assert_eq!(truncate_html(&escaped, 100), escaped);
        assert_eq!(
            subject(&Notification::new(NotificationKind::Test, " \r\n", "")),
            "Lodestar"
        );
        assert_eq!(
            subject(&Notification::new(
                NotificationKind::Test,
                "x".repeat(500),
                ""
            ))
            .chars()
            .count(),
            MAX_SUBJECT_CHARS
        );
    }

    #[test]
    fn links_must_be_web_urls() {
        let mut n = alert();
        n.link = Some("javascript:alert(1)".into());
        assert!(!text_part(&n).contains("javascript") && !html_part(&n).contains("javascript"));
        n.link = Some(format!("https://l.lan/{}", "a".repeat(MAX_LINK_BYTES)));
        assert!(!html_part(&n).contains("<a "));
        n.link = Some("https://l.lan/a?b=\"><script>".into());
        let html = html_part(&n);
        assert!(
            !html.contains("<script>")
                && html.contains("<a href=\"https://l.lan/a?b=%22%3E%3Cscript%3E\">"),
            "{html}"
        );
    }

    #[tokio::test]
    async fn secrets_never_appear_in_debug_errors_or_logs() {
        let logs = crate::test_support::capture_logs();
        let (port, _) = smtp_server(vec![
            Script::default(),
            Script {
                accept_login: false,
                ..Script::default()
            },
            Script {
                auth: "LOGIN",
                mail_reply: "451 4.3.0 later",
                ..Script::default()
            },
        ])
        .await;
        let cfg = config(port, SmtpSecurity::StartTls);
        let mut texts = String::new();
        for _ in 0..3 {
            let sender = email(&cfg);
            let out = deliver(&sender, &alert(), &FAST).await;
            texts.push_str(&format!("{sender:?} {cfg:?} {out:?}"));
            if let Err(e) = &out.result {
                texts.push_str(&e.to_string());
            }
            let boxed: Box<dyn Notifier> = Box::new(sender);
            texts.push_str(&format!("{boxed:?}"));
        }
        let mut bad = cfg.clone();
        bad.security = SmtpSecurity::None;
        bad.host = "smtp.example.com".into();
        let err = Email::new(&bad).unwrap_err();
        texts.push_str(&format!("{err:?} {err} {}", NetError::Connect));

        let logged = logs.contents();
        assert!(
            logged.contains("email attempt"),
            "the capture works: {logged}"
        );
        for s in [USER, PASS, PLAIN, USER_B64, PASS_B64] {
            assert!(!texts.contains(s), "{s} in debug or error text: {texts}");
            assert!(!logged.contains(s), "{s} in the logs: {logged}");
        }
        assert!(texts.contains("mail.test"), "{texts}");
    }
}
