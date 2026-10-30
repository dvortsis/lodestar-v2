//! The connection to a Bitmagnet PostgreSQL (IMP-3): structured settings,
//! a credential that never comes from argv, TLS with certificate
//! verification for anything that isn't loopback, and a session that is
//! read-only and bounded before its first query.

use std::fmt;
use std::future::Future;
use std::io;
use std::net::{IpAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use rustls::ClientConfig;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_postgres::config::SslMode;
use tokio_postgres::tls::{ChannelBinding, MakeTlsConnect, TlsConnect};
use tokio_postgres::{Client, Config, NoTls};
use zeroize::Zeroizing;

use super::Refusal;

/// The environment variable the password may come from.
pub const PASSWORD_ENV: &str = "LODESTAR_IMPORT_PG_PASSWORD";
/// The largest password file read.
const MAX_PASSWORD: u64 = 4096;
/// The largest CA file read.
const MAX_CA_FILE: u64 = 1 << 20;

/// A PostgreSQL password. Zeroed on drop; `Debug` never prints it.
#[derive(Clone)]
pub struct Password(pub(super) Zeroizing<Vec<u8>>);

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(..)")
    }
}

impl Password {
    pub fn new(bytes: Vec<u8>) -> Password {
        Password(Zeroizing::new(bytes))
    }

    /// Reads a password file: at most 4 KiB, one trailing newline removed,
    /// never empty. The error is a fixed message, never the file's content.
    pub fn from_file(path: &Path) -> Result<Password, &'static str> {
        let mut bytes = Zeroizing::new(Vec::new());
        // Bounded, so a FIFO or a device can't fill memory.
        std::fs::File::open(path)
            .and_then(|f| {
                io::Read::read_to_end(&mut io::Read::take(f, MAX_PASSWORD + 1), &mut bytes)
            })
            .map_err(|_| "the password file can't be read")?;
        if bytes.len() as u64 > MAX_PASSWORD {
            return Err("the password file is over 4 KiB; is it the right file?");
        }
        if bytes.ends_with(b"\n") {
            bytes.pop();
            if bytes.ends_with(b"\r") {
                bytes.pop();
            }
        }
        if bytes.is_empty() {
            return Err("the password file is empty");
        }
        Ok(Password(bytes))
    }

    /// The password from [`PASSWORD_ENV`], if it's set and not empty.
    pub fn from_env() -> Option<Password> {
        std::env::var_os(PASSWORD_ENV)
            .map(|v| v.into_encoded_bytes())
            .filter(|v| !v.is_empty())
            .map(Password::new)
    }
}

/// A warning when the password file's permissions let others read it.
#[cfg(unix)]
pub fn password_file_warning(path: &Path) -> Option<&'static str> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path).ok()?.permissions().mode();
    (mode & 0o077 != 0).then_some("the password file is readable by others; chmod 600 it")
}

#[cfg(not(unix))]
pub fn password_file_warning(_: &Path) -> Option<&'static str> {
    None
}

/// How the connection is protected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tls {
    /// TLS, verifying the server's certificate and name: against the
    /// platform's trust store, or only the CAs in this PEM file.
    Verify { ca_file: Option<PathBuf> },
    /// No TLS. Allowed only to a Unix socket or a loopback address, and
    /// only when asked for explicitly (`--allow-plaintext-loopback`).
    PlaintextLoopback,
}

/// Where and how to connect. Structured, so no free-form URL, `options`,
/// `search_path` or SQL ever reaches the connection.
#[derive(Debug, Clone)]
pub struct Settings {
    /// A host name, an IP address, or (on Unix) a socket directory.
    pub host: String,
    pub port: u16,
    pub dbname: String,
    pub user: String,
    pub password: Option<Password>,
    pub tls: Tls,
    /// The longest one statement may run; the session's
    /// `statement_timeout`.
    pub statement_timeout: Duration,
}

impl Settings {
    /// Settings with the defaults: port 5432, TLS against the platform's
    /// trust store, and a 30 s statement timeout.
    pub fn new(host: &str, dbname: &str, user: &str) -> Settings {
        Settings {
            host: host.into(),
            port: 5432,
            dbname: dbname.into(),
            user: user.into(),
            password: None,
            tls: Tls::Verify { ca_file: None },
            statement_timeout: Duration::from_secs(30),
        }
    }
}

/// Where the settings point, once checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    Socket(PathBuf),
    /// A loopback address, resolved once so the name can't change under
    /// us.
    Loopback(IpAddr),
    /// Anything else: TLS only.
    Remote,
}

/// A name PostgreSQL takes as a startup parameter: 1–63 bytes, no NUL.
fn identifier(s: &str) -> bool {
    !s.is_empty() && s.len() <= 63 && !s.contains('\0')
}

/// A host name or an IP literal: no spaces, commas or anything that could
/// be read as another setting.
fn host_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
}

/// Checks the settings and classifies the host, with no I/O beyond
/// resolving `localhost`.
pub(crate) fn target(s: &Settings) -> Result<Target, Refusal> {
    if !identifier(&s.dbname) || !identifier(&s.user) || s.port == 0 {
        return Err(Refusal::BadSettings);
    }
    if s.statement_timeout.is_zero() || s.statement_timeout > Duration::from_secs(3600) {
        return Err(Refusal::BadSettings);
    }
    let t = if s.host.starts_with('/') {
        if s.host.contains('\0') {
            return Err(Refusal::BadSettings);
        }
        Target::Socket(PathBuf::from(&s.host))
    } else if !host_name(&s.host) {
        return Err(Refusal::BadSettings);
    } else if let Ok(ip) = s.host.trim_matches(['[', ']']).parse::<IpAddr>() {
        if ip.is_loopback() {
            Target::Loopback(ip)
        } else {
            Target::Remote
        }
    } else if s.host.eq_ignore_ascii_case("localhost") {
        // Loopback only if every address it resolves to is.
        let addrs: Vec<IpAddr> = (s.host.as_str(), s.port)
            .to_socket_addrs()
            .map_err(|_| Refusal::Unavailable("connect_failed"))?
            .map(|a| a.ip())
            .collect();
        match addrs.first() {
            Some(ip) if addrs.iter().all(IpAddr::is_loopback) => Target::Loopback(*ip),
            _ => Target::Remote,
        }
    } else {
        Target::Remote
    };
    match (&s.tls, &t) {
        (Tls::PlaintextLoopback, Target::Remote) => Err(Refusal::TlsRequired),
        // PostgreSQL has no TLS on a Unix socket.
        (Tls::Verify { .. }, Target::Socket(_)) => Err(Refusal::BadSettings),
        _ => Ok(t),
    }
}

/// The session settings every connection runs with. Only these constants
/// and the configured timeout go in; nothing from the source or the user.
pub(crate) fn session_sql(statement_timeout: Duration) -> String {
    let ms = statement_timeout.as_millis();
    // search_path is pg_catalog alone: the source is untrusted, and a
    // function it put in `public` would otherwise win overload resolution
    // (IMP-0). Every table is named `public.<table>`. row_security=off: a
    // policy would otherwise hide rows silently; with it off, such a read
    // is an error. Seqscan and sort are discouraged so a keyset read uses
    // its index even when the planner's statistics are stale.
    format!(
        "SET default_transaction_read_only = on; \
         SET statement_timeout = {ms}; \
         SET lock_timeout = 10000; \
         SET idle_in_transaction_session_timeout = {}; \
         SET search_path = pg_catalog; \
         SET row_security = off; \
         SET enable_seqscan = off; \
         SET enable_sort = off",
        ms + 60_000
    )
}

/// Connects and sets the session up. Any failure is a fixed refusal: the
/// server's own text never leaves this function.
pub(crate) async fn connect(s: &Settings) -> Result<Client, Refusal> {
    let target = target(s)?;
    let mut cfg = Config::new();
    cfg.user(&s.user)
        .dbname(&s.dbname)
        .port(s.port)
        .application_name("lodestar-import-bitmagnet")
        .connect_timeout(Duration::from_secs(10))
        // Before the first statement too, in case a role default differs.
        .options(format!(
            "-c default_transaction_read_only=on -c statement_timeout={}",
            s.statement_timeout.as_millis()
        ));
    if let Some(p) = &s.password {
        cfg.password(&p.0[..]);
    }
    let deadline = Duration::from_secs(20);
    let client = match (&target, &s.tls) {
        #[cfg(unix)]
        (Target::Socket(dir), _) => {
            cfg.host_path(dir).ssl_mode(SslMode::Disable);
            plain(&cfg, deadline).await?
        }
        #[cfg(not(unix))]
        (Target::Socket(_), _) => return Err(Refusal::BadSettings),
        (Target::Loopback(ip), Tls::PlaintextLoopback) => {
            cfg.host(ip.to_string())
                .hostaddr(*ip)
                .ssl_mode(SslMode::Disable);
            plain(&cfg, deadline).await?
        }
        (_, Tls::Verify { ca_file }) => {
            cfg.host(s.host.trim_matches(['[', ']']))
                .ssl_mode(SslMode::Require);
            let tls = MakeRustls(Arc::new(client_config(ca_file.as_deref())?));
            let (client, connection) = tokio::time::timeout(deadline, cfg.connect(tls))
                .await
                .map_err(|_| Refusal::Unavailable("connect_failed"))?
                .map_err(|e| connect_error(&e))?;
            tokio::spawn(connection);
            client
        }
        (_, Tls::PlaintextLoopback) => return Err(Refusal::TlsRequired),
    };
    tokio::time::timeout(
        s.statement_timeout,
        client.batch_execute(&session_sql(s.statement_timeout)),
    )
    .await
    .map_err(|_| Refusal::StatementTimeout)?
    .map_err(|_| Refusal::Unavailable("session_setup"))?;
    Ok(client)
}

async fn plain(cfg: &Config, deadline: Duration) -> Result<Client, Refusal> {
    let (client, connection) = tokio::time::timeout(deadline, cfg.connect(NoTls))
        .await
        .map_err(|_| Refusal::Unavailable("connect_failed"))?
        .map_err(|e| connect_error(&e))?;
    tokio::spawn(connection);
    Ok(client)
}

fn connect_error(e: &tokio_postgres::Error) -> Refusal {
    if let Some(db) = e.as_db_error() {
        return match db.code().code() {
            "28P01" | "28000" => Refusal::Unavailable("auth_failed"),
            "3D000" => Refusal::Unavailable("no_database"),
            "42501" => Refusal::Unavailable("permission_denied"),
            _ => Refusal::Unavailable("connect_failed"),
        };
    }
    // tokio-postgres reports TLS problems (no TLS on the server, a
    // certificate that doesn't verify) in its own fixed wording.
    let text = e.to_string();
    if text.contains("TLS") || text.contains("SSL") {
        Refusal::Unavailable("tls_failed")
    } else {
        Refusal::Unavailable("connect_failed")
    }
}

/// rustls on ring, TLS 1.2 or 1.3, verifying the server's certificate and
/// name. With a CA file, only its certificates are trusted.
fn client_config(ca_file: Option<&Path>) -> Result<ClientConfig, Refusal> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|_| Refusal::Unavailable("tls_failed"))?;
    let config = match ca_file {
        Some(path) => {
            let mut pem = Vec::new();
            std::fs::File::open(path)
                .and_then(|f| io::Read::read_to_end(&mut io::Read::take(f, MAX_CA_FILE), &mut pem))
                .map_err(|_| Refusal::Unavailable("ca_file_unreadable"))?;
            let mut roots = rustls::RootCertStore::empty();
            for cert in CertificateDer::pem_slice_iter(&pem) {
                let cert = cert.map_err(|_| Refusal::Unavailable("ca_file_invalid"))?;
                roots
                    .add(cert)
                    .map_err(|_| Refusal::Unavailable("ca_file_invalid"))?;
            }
            if roots.is_empty() {
                return Err(Refusal::Unavailable("ca_file_invalid"));
            }
            builder.with_root_certificates(roots).with_no_client_auth()
        }
        None => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(
                rustls_platform_verifier::Verifier::new(provider)
                    .map_err(|_| Refusal::Unavailable("tls_failed"))?,
            ))
            .with_no_client_auth(),
    };
    Ok(config)
}

/// tokio-postgres's TLS hooks over tokio-rustls. No channel binding: the
/// certificate check is what authenticates the server.
#[derive(Clone)]
struct MakeRustls(Arc<ClientConfig>);

struct RustlsConnect {
    config: Arc<ClientConfig>,
    name: ServerName<'static>,
}

struct RustlsStream<S>(tokio_rustls::client::TlsStream<S>);

impl<S> MakeTlsConnect<S> for MakeRustls
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Stream = RustlsStream<S>;
    type TlsConnect = RustlsConnect;
    type Error = io::Error;

    fn make_tls_connect(&mut self, domain: &str) -> io::Result<RustlsConnect> {
        let name = ServerName::try_from(domain.to_owned())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "TLS: bad server name"))?;
        Ok(RustlsConnect {
            config: self.0.clone(),
            name,
        })
    }
}

impl<S> TlsConnect<S> for RustlsConnect
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Stream = RustlsStream<S>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = io::Result<RustlsStream<S>>> + Send>>;

    fn connect(self, stream: S) -> Self::Future {
        Box::pin(async move {
            tokio_rustls::TlsConnector::from(self.config)
                .connect(self.name, stream)
                .await
                .map(RustlsStream)
        })
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> tokio_postgres::tls::TlsStream for RustlsStream<S> {
    fn channel_binding(&self) -> ChannelBinding {
        ChannelBinding::none()
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for RustlsStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for RustlsStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}
