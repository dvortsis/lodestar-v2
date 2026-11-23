//! The IRC client: one task per server connection, which connects through
//! the plugin's egress, registers (optionally with SASL PLAIN or NickServ),
//! joins the configured channels, and hands every message to a bounded
//! queue until it's shut down.
//!
//! - **Egress only.** Every connection is [`connect_tcp_routed`] over the
//!   [`Egress`] the plugin gives it: the SSRF guard, the route (direct,
//!   bound, interface, SOCKS5) and the kill-switch. The kill-switch also
//!   races each live connection, which is dropped the moment it goes down.
//! - **TLS by default,** rustls on ring with the platform verifier (or only
//!   the roots given). Credentials (server password, SASL, NickServ,
//!   channel keys) are refused in a plaintext [`Config`] and, again, by the
//!   protocol state machine before any is written.
//! - **SASL PLAIN** is negotiated with `CAP LS 302`; a server that doesn't
//!   offer it, refuses it or registers the client without it stops the
//!   client (fail closed). A failed login stops it too, instead of retrying
//!   into an account lock.
//! - **Bounded everywhere:** one line's worth of read buffer
//!   ([`LineBuffer`]), [`EVENT_QUEUE`] messages waiting for the plugin
//!   (more are dropped and counted), [`COMMAND_QUEUE`] lines waiting to be
//!   sent, [`OUT_QUEUE`] protocol lines, and at most one pending `PONG`.
//! - **Flood control:** every line goes through a token bucket ([`Flood`]).
//! - **Reconnect** waits with exponential back-off, jitter and a cap
//!   ([`Reconnect`]); the count resets after a connection stays up.
//! - **Ignored:** CTCP (and so DCC), and invitations to any channel that
//!   isn't configured.
//! - **No logging.** The client reports through [`State`] and [`Stats`];
//!   no variant of either carries server text or a credential.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lodestar_integrations::Secret;
use lodestar_integrations::net::{
    KillSwitch, NetError, NetPolicy, Resolve, Route, TcpRouteError, connect_tcp_routed,
};
use rustls::pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use zeroize::Zeroizing;

use crate::line::{self, EncodeError, Framed, LineBuffer, Message, irc_eq};

/// Messages waiting for the plugin; more are dropped and counted.
pub const EVENT_QUEUE: usize = 256;
/// Lines from [`Handle`] waiting to be sent.
pub const COMMAND_QUEUE: usize = 64;
/// Protocol lines (registration, joins) waiting to be sent. A server that
/// makes the client queue more is flooding it, and is disconnected.
pub const OUT_QUEUE: usize = 64;
/// The most configured channels.
pub const MAX_CHANNELS: usize = 32;
/// The most `CAP LS` lines read before giving up.
pub const MAX_CAP_LS_LINES: u8 = 16;
/// Alternative nicks tried while registering when the nick is taken.
pub const NICK_RETRIES: u8 = 3;
/// Bytes of the SASL payload per `AUTHENTICATE` line (IRCv3).
const SASL_CHUNK: usize = 400;
/// The read size per wake-up.
const READ_CHUNK: usize = 4096;

/// How the connection is carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Transport {
    /// TLS from the first byte, the server's certificate and name verified.
    #[default]
    Tls,
    /// Plaintext. Allowed only with no credentials of any kind.
    Plaintext,
}

/// Which certificates the TLS connection trusts.
#[derive(Clone, Default)]
pub enum Roots {
    /// The operating system's (rustls-platform-verifier).
    #[default]
    Platform,
    /// Only these (a server with its own CA, and the tests).
    Only(Vec<CertificateDer<'static>>),
}

impl fmt::Debug for Roots {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Roots::Platform => f.write_str("Platform"),
            Roots::Only(c) => write!(f, "Only({} certificates)", c.len()),
        }
    }
}

/// How the client identifies itself to services.
#[derive(Clone, Default)]
pub enum Auth {
    #[default]
    None,
    /// SASL PLAIN during registration (needs TLS).
    SaslPlain { account: String, password: Secret },
    /// `PRIVMSG NickServ :IDENTIFY <account> <password>` after the welcome
    /// (needs TLS). Channels are joined once services confirm (numeric
    /// 900) or after [`Timeouts::nickserv`].
    NickServ { account: String, password: Secret },
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Auth::None => "None",
            Auth::SaslPlain { .. } => "SaslPlain(…)",
            Auth::NickServ { .. } => "NickServ(…)",
        })
    }
}

/// A channel to join, with its key if it has one (needs TLS).
#[derive(Clone)]
pub struct Channel {
    pub name: String,
    pub key: Option<Secret>,
}

impl Channel {
    pub fn new(name: impl Into<String>) -> Channel {
        Channel {
            name: name.into(),
            key: None,
        }
    }
}

impl fmt::Debug for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Channel")
            .field("name", &self.name)
            .field("key", &self.key.as_ref().map(|_| "…"))
            .finish()
    }
}

/// Outgoing flood control: a bucket of `burst` lines, refilled one line
/// per `interval`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flood {
    pub burst: u32,
    pub interval: Duration,
}

impl Flood {
    pub const MAX_BURST: u32 = 10;
    pub const MIN_INTERVAL: Duration = Duration::from_millis(500);
}

impl Default for Flood {
    fn default() -> Self {
        Flood {
            burst: 4,
            interval: Duration::from_secs(2),
        }
    }
}

/// Reconnect back-off: the wait after the `n`th failure in a row is
/// `min(cap, base × 2ⁿ⁻¹)`, jittered uniformly into its upper half. The
/// count resets once a connection has been registered for `stable_after`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reconnect {
    pub base: Duration,
    pub cap: Duration,
    pub stable_after: Duration,
}

impl Reconnect {
    pub const MIN_BASE: Duration = Duration::from_secs(1);
    pub const MAX_CAP: Duration = Duration::from_secs(3600);
}

impl Default for Reconnect {
    fn default() -> Self {
        Reconnect {
            base: Duration::from_secs(5),
            cap: Duration::from_secs(600),
            stable_after: Duration::from_secs(300),
        }
    }
}

/// The client's timers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// Resolving, connecting, the proxy handshake, and then TLS.
    pub connect: Duration,
    /// From connecting to the welcome (001).
    pub register: Duration,
    /// Silence from the server before the client sends a `PING`.
    pub ping_after: Duration,
    /// Further silence after that `PING` before the connection is dropped.
    pub ping_timeout: Duration,
    /// One line's write.
    pub write: Duration,
    /// How long joins wait for NickServ's confirmation.
    pub nickserv: Duration,
}

impl Timeouts {
    pub const MIN: Duration = Duration::from_secs(1);
    pub const MAX: Duration = Duration::from_secs(600);
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            connect: Duration::from_secs(30),
            register: Duration::from_secs(60),
            ping_after: Duration::from_secs(120),
            ping_timeout: Duration::from_secs(60),
            write: Duration::from_secs(30),
            nickserv: Duration::from_secs(10),
        }
    }
}

/// One server connection's settings.
#[derive(Clone)]
pub struct Config {
    /// A host name or literal address, checked by the egress guard.
    pub host: String,
    pub port: u16,
    pub transport: Transport,
    pub roots: Roots,
    pub nick: String,
    /// The user name (ident) sent in `USER`.
    pub username: String,
    pub realname: String,
    /// `PASS`, for servers and bouncers that want one (needs TLS).
    pub server_password: Option<Secret>,
    pub auth: Auth,
    /// Joined after registration, and the only channels an invitation may
    /// make the client join. At most [`MAX_CHANNELS`].
    pub channels: Vec<Channel>,
    pub flood: Flood,
    pub reconnect: Reconnect,
    pub timeouts: Timeouts,
}

impl Config {
    /// TLS on port 6697, no credentials, no channels, default limits.
    pub fn new(host: impl Into<String>, nick: impl Into<String>) -> Config {
        Config {
            host: host.into(),
            port: 6697,
            transport: Transport::Tls,
            roots: Roots::Platform,
            nick: nick.into(),
            username: "lodestar".to_owned(),
            realname: "Lodestar".to_owned(),
            server_password: None,
            auth: Auth::None,
            channels: Vec::new(),
            flood: Flood::default(),
            reconnect: Reconnect::default(),
            timeouts: Timeouts::default(),
        }
    }

    /// Checks every field. [`spawn`] calls it; a plugin can call it when
    /// settings are saved.
    pub fn validate(&self) -> Result<(), ConfigError> {
        use ConfigError::Invalid;
        let host = self.host.trim_start_matches('[').trim_end_matches(']');
        if host.is_empty()
            || host.len() > 253
            || host.bytes().any(|b| b <= b' ' || b == 0x7f || b == b'/')
        {
            return Err(Invalid("host"));
        }
        if self.port == 0 {
            return Err(Invalid("port"));
        }
        if !valid_nick(&self.nick) {
            return Err(Invalid("nick"));
        }
        if self.username.is_empty()
            || self.username.len() > 32
            || !self
                .username
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        {
            return Err(Invalid("username"));
        }
        if self.realname.is_empty() || self.realname.len() > 128 || has_control(&self.realname) {
            return Err(Invalid("realname"));
        }
        if let Some(pw) = &self.server_password
            && !valid_word(pw.expose(), 256)
        {
            return Err(Invalid("server password"));
        }
        match &self.auth {
            Auth::None => {}
            Auth::SaslPlain { account, password } | Auth::NickServ { account, password } => {
                if !valid_word(account, 64) {
                    return Err(Invalid("account"));
                }
                if !valid_word(password.expose(), 256) {
                    return Err(Invalid("password"));
                }
            }
        }
        if self.channels.len() > MAX_CHANNELS {
            return Err(Invalid("channels"));
        }
        for c in &self.channels {
            if !valid_channel(&c.name) {
                return Err(Invalid("channel"));
            }
            if let Some(k) = &c.key
                && !valid_key(k.expose())
            {
                return Err(Invalid("channel key"));
            }
        }
        let credentials = self.server_password.is_some()
            || !matches!(self.auth, Auth::None)
            || self.channels.iter().any(|c| c.key.is_some());
        if credentials && self.transport != Transport::Tls {
            return Err(ConfigError::CredentialsNeedTls);
        }
        let f = self.flood;
        if f.burst == 0 || f.burst > Flood::MAX_BURST || f.interval < Flood::MIN_INTERVAL {
            return Err(Invalid("flood control"));
        }
        let r = self.reconnect;
        if r.base < Reconnect::MIN_BASE || r.cap < r.base || r.cap > Reconnect::MAX_CAP {
            return Err(Invalid("reconnect"));
        }
        let t = self.timeouts;
        let all = [
            t.connect,
            t.register,
            t.ping_after,
            t.ping_timeout,
            t.write,
            t.nickserv,
        ];
        if all.iter().any(|d| *d < Timeouts::MIN || *d > Timeouts::MAX) {
            return Err(Invalid("timeouts"));
        }
        if self.transport == Transport::Tls {
            server_name(&self.host)?;
            if let Roots::Only(c) = &self.roots
                && c.is_empty()
            {
                return Err(Invalid("trusted certificates"));
            }
        }
        Ok(())
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("transport", &self.transport)
            .field("roots", &self.roots)
            .field("nick", &self.nick)
            .field("username", &self.username)
            .field(
                "server_password",
                &self.server_password.as_ref().map(|_| "…"),
            )
            .field("auth", &self.auth)
            .field("channels", &self.channels)
            .field("flood", &self.flood)
            .field("reconnect", &self.reconnect)
            .field("timeouts", &self.timeouts)
            .finish_non_exhaustive()
    }
}

/// Why a [`Config`] can't be used. No variant carries a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    /// The named field isn't valid.
    Invalid(&'static str),
    /// A server password, login or channel key on a plaintext connection.
    CredentialsNeedTls,
    /// TLS couldn't be set up (the platform verifier or the roots given).
    Tls,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Invalid(what) => write!(f, "the {what} setting isn't valid"),
            ConfigError::CredentialsNeedTls => {
                f.write_str("passwords, logins and channel keys are only sent over TLS")
            }
            ConfigError::Tls => f.write_str("TLS couldn't be set up"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// RFC 2812 nicks, up to 30 characters.
fn valid_nick(n: &str) -> bool {
    let special = |b: u8| {
        matches!(
            b,
            b'[' | b']' | b'\\' | b'`' | b'_' | b'^' | b'{' | b'|' | b'}'
        )
    };
    let b = n.as_bytes();
    !b.is_empty()
        && b.len() <= 30
        && (b[0].is_ascii_alphabetic() || special(b[0]))
        && b[1..]
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || special(c) || c == b'-')
}

/// A channel name: a `#`, `&`, `+` or `!` prefix, 2 to 50 bytes, and none
/// of space, comma, `:`, or a control character (BEL included).
pub fn valid_channel(c: &str) -> bool {
    (2..=50).contains(&c.len())
        && c.starts_with(['#', '&', '+', '!'])
        && !c.contains([' ', ',', ':'])
        && !has_control(c)
}

fn valid_key(k: &str) -> bool {
    (1..=32).contains(&k.len()) && !k.contains([' ', ',', ':']) && !has_control(k)
}

/// A credential sent as one IRC word: no space or control character.
fn valid_word(w: &str, max: usize) -> bool {
    !w.is_empty() && w.len() <= max && !w.contains(' ') && !has_control(w)
}

fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

fn server_name(host: &str) -> Result<ServerName<'static>, ConfigError> {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    ServerName::try_from(host.to_owned()).map_err(|_| ConfigError::Invalid("host"))
}

fn tls_connector(roots: &Roots) -> Result<tokio_rustls::TlsConnector, ConfigError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|_| ConfigError::Tls)?;
    let config = match roots {
        Roots::Only(certs) => {
            let mut store = rustls::RootCertStore::empty();
            for c in certs {
                store.add(c.clone()).map_err(|_| ConfigError::Tls)?;
            }
            builder.with_root_certificates(store).with_no_client_auth()
        }
        Roots::Platform => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(
                rustls_platform_verifier::Verifier::new(provider).map_err(|_| ConfigError::Tls)?,
            ))
            .with_no_client_auth(),
    };
    Ok(tokio_rustls::TlsConnector::from(Arc::new(config)))
}

/// The plugin's egress for this client: the guard's resolver and policy,
/// the route, and the kill-switch. The only way the client opens a
/// connection.
#[derive(Clone)]
pub struct Egress {
    resolver: Arc<dyn Resolve>,
    policy: NetPolicy,
    route: Route,
    kill: Option<Arc<dyn KillSwitch>>,
}

impl Egress {
    pub fn new(resolver: Arc<dyn Resolve>, policy: NetPolicy, route: Route) -> Egress {
        Egress {
            resolver,
            policy,
            route,
            kill: None,
        }
    }

    pub fn with_kill_switch(mut self, kill: Arc<dyn KillSwitch>) -> Egress {
        self.kill = Some(kill);
        self
    }
}

impl fmt::Debug for Egress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Egress")
            .field("policy", &self.policy)
            .field("route", &self.route)
            .field("kill_switch", &self.kill.is_some())
            .finish_non_exhaustive()
    }
}

/// Where the client is. No variant carries server text or a credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Connecting; `attempt` is 1 after a clean start or a stable
    /// connection.
    Connecting { attempt: u32 },
    /// Connected, registering (CAP, SASL, NICK, USER).
    Registering,
    /// Registered and joining or joined; `nick` is the server's view.
    Ready { nick: String },
    /// Disconnected, waiting `retry_in` before the next attempt.
    Waiting {
        attempt: u32,
        after: Disconnect,
        retry_in: Duration,
    },
    /// Stopped for good: shut down, or something a retry can't fix.
    Stopped(Stop),
}

/// Why a connection ended and will be retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disconnect {
    /// Resolving, connecting or the proxy failed.
    Connect(TcpRouteError),
    /// The TLS handshake failed or timed out (not a certificate problem).
    Tls,
    /// The server closed the connection.
    Closed,
    /// The server sent `ERROR`.
    ServerError,
    PingTimeout,
    RegisterTimeout,
    /// Every alternative nick was taken.
    NickInUse,
    /// The kill-switch is down.
    KillSwitch,
    /// Reading or writing failed or timed out.
    Io,
    /// The server broke the protocol.
    Protocol(&'static str),
    /// The server made the client queue more than [`OUT_QUEUE`] lines.
    ServerFlood,
}

/// Why the client stopped for good.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    /// [`Handle::shutdown`], every [`Handle`] dropped, or the event queue
    /// dropped.
    Shutdown,
    /// The egress guard or route refused the destination.
    Refused(TcpRouteError),
    /// The server's certificate didn't verify.
    Certificate,
    /// SASL or NickServ login failed.
    AuthFailed,
    /// SASL is configured but the server doesn't offer PLAIN, refused it,
    /// or registered the client without it.
    SaslUnavailable,
    /// The server refused the nick as invalid (432).
    NickRejected,
    /// The server banned the client (465).
    Banned,
    /// A line the client builds couldn't be encoded, or a credential was
    /// about to go out without TLS. Shouldn't happen after validation.
    Internal,
}

/// Counters, for the plugin's health and metrics.
#[derive(Debug, Default)]
pub struct Stats {
    lines_in: AtomicU64,
    lines_out: AtomicU64,
    overlong: AtomicU64,
    malformed: AtomicU64,
    ctcp_ignored: AtomicU64,
    events_dropped: AtomicU64,
    connections: AtomicU64,
}

/// A copy of [`Stats`] at one moment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatsSnapshot {
    /// Lines read and parsed.
    pub lines_in: u64,
    pub lines_out: u64,
    /// Lines over [`line::MAX_LINE_BYTES`], dropped.
    pub overlong: u64,
    /// Lines that didn't parse, dropped.
    pub malformed: u64,
    pub ctcp_ignored: u64,
    /// Messages dropped because the event queue was full.
    pub events_dropped: u64,
    /// Connections made (TCP, and TLS where used).
    pub connections: u64,
}

impl Stats {
    fn bump(c: &AtomicU64) {
        c.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> StatsSnapshot {
        let g = |c: &AtomicU64| c.load(Ordering::Relaxed);
        StatsSnapshot {
            lines_in: g(&self.lines_in),
            lines_out: g(&self.lines_out),
            overlong: g(&self.overlong),
            malformed: g(&self.malformed),
            ctcp_ignored: g(&self.ctcp_ignored),
            events_dropped: g(&self.events_dropped),
            connections: g(&self.connections),
        }
    }
}

/// The messages the server sends, in order, except `PING`, CTCP, `CAP`,
/// `AUTHENTICATE` and the SASL numerics (900–908), which the client
/// handles itself. Ends when the client stops.
pub type Events = mpsc::Receiver<Message>;

/// Why [`Handle`] couldn't queue a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// The target or channel isn't valid.
    BadTarget,
    /// The text has a CTCP marker (`\x01`).
    Ctcp,
    /// A channel key on a plaintext connection.
    KeyNeedsTls,
    /// The line can't be encoded.
    Encode(EncodeError),
    /// [`COMMAND_QUEUE`] lines are waiting already.
    Full,
    /// The client has stopped.
    Closed,
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SendError::BadTarget => f.write_str("the target isn't a valid nick or channel"),
            SendError::Ctcp => f.write_str("CTCP isn't sent"),
            SendError::KeyNeedsTls => f.write_str("channel keys are only sent over TLS"),
            SendError::Encode(e) => e.fmt(f),
            SendError::Full => f.write_str("too many lines are waiting to be sent"),
            SendError::Closed => f.write_str("the IRC client has stopped"),
        }
    }
}

impl std::error::Error for SendError {}

/// Controls a running client. Cloneable; when every clone is dropped the
/// client quits. Lines are sent once registered, in order, through flood
/// control; lines queued while disconnected wait for the next connection.
#[derive(Clone)]
pub struct Handle {
    commands: mpsc::Sender<Zeroizing<Vec<u8>>>,
    state: watch::Receiver<State>,
    stats: Arc<Stats>,
    shutdown: Arc<watch::Sender<bool>>,
    tls: bool,
}

impl fmt::Debug for Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Handle")
            .field("state", &*self.state.borrow())
            .finish_non_exhaustive()
    }
}

impl Handle {
    /// Queues `PRIVMSG <target> :<text>`.
    pub fn privmsg(&self, target: &str, text: &str) -> Result<(), SendError> {
        if target.is_empty() || target.len() > 64 || target.contains(',') || has_control(target) {
            return Err(SendError::BadTarget);
        }
        if text.contains('\u{1}') {
            return Err(SendError::Ctcp);
        }
        self.queue(line::encode("PRIVMSG", &[target, text]).map_err(SendError::Encode)?)
    }

    /// Queues `JOIN <channel> [<key>]`. A key goes out only on a TLS
    /// connection: on plaintext it's refused here.
    pub fn join(&self, channel: &str, key: Option<&Secret>) -> Result<(), SendError> {
        if !valid_channel(channel) {
            return Err(SendError::BadTarget);
        }
        let line = match key {
            Some(k) => {
                if !self.tls {
                    return Err(SendError::KeyNeedsTls);
                }
                if !valid_key(k.expose()) {
                    return Err(SendError::BadTarget);
                }
                line::encode("JOIN", &[channel, k.expose()])
            }
            None => line::encode("JOIN", &[channel]),
        };
        self.queue(line.map_err(SendError::Encode)?)
    }

    /// Queues `PART <channel>`.
    pub fn part(&self, channel: &str) -> Result<(), SendError> {
        if !valid_channel(channel) {
            return Err(SendError::BadTarget);
        }
        self.queue(line::encode("PART", &[channel]).map_err(SendError::Encode)?)
    }

    fn queue(&self, line: Zeroizing<Vec<u8>>) -> Result<(), SendError> {
        self.commands.try_send(line).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => SendError::Full,
            mpsc::error::TrySendError::Closed(_) => SendError::Closed,
        })
    }

    /// The client's state, to watch.
    pub fn state(&self) -> watch::Receiver<State> {
        self.state.clone()
    }

    pub fn stats(&self) -> StatsSnapshot {
        self.stats.snapshot()
    }

    /// Sends `QUIT` (best effort) and stops the client.
    pub fn shutdown(&self) {
        self.shutdown.send_replace(true);
    }
}

/// Starts a client on the current Tokio runtime. Fails only on settings
/// that can't work; everything after that is reported through
/// [`Handle::state`].
///
/// A key given to [`Handle::join`] is refused on a plaintext config.
pub fn spawn(config: Config, egress: Egress) -> Result<(Handle, Events), ConfigError> {
    config.validate()?;
    let tls = match config.transport {
        Transport::Tls => Some((tls_connector(&config.roots)?, server_name(&config.host)?)),
        Transport::Plaintext => None,
    };
    let (cmd_tx, cmd_rx) = mpsc::channel(COMMAND_QUEUE);
    let (ev_tx, ev_rx) = mpsc::channel(EVENT_QUEUE);
    let (state_tx, state_rx) = watch::channel(State::Connecting { attempt: 1 });
    let (stop_tx, stop_rx) = watch::channel(false);
    let stats = Arc::new(Stats::default());
    let plaintext = tls.is_none();
    let ctx = Ctx {
        cfg: Arc::new(config),
        egress,
        tls,
        events: ev_tx,
        commands: cmd_rx,
        state: state_tx,
        stats: Arc::clone(&stats),
        shutdown: stop_rx,
    };
    tokio::spawn(run(ctx));
    let handle = Handle {
        commands: cmd_tx,
        state: state_rx,
        stats,
        shutdown: Arc::new(stop_tx),
        tls: !plaintext,
    };
    Ok((handle, ev_rx))
}

struct Ctx {
    cfg: Arc<Config>,
    egress: Egress,
    tls: Option<(tokio_rustls::TlsConnector, ServerName<'static>)>,
    events: mpsc::Sender<Message>,
    commands: mpsc::Receiver<Zeroizing<Vec<u8>>>,
    state: watch::Sender<State>,
    stats: Arc<Stats>,
    shutdown: watch::Receiver<bool>,
}

/// How a session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum End {
    Retry(Disconnect),
    Stop(Stop),
}

/// The wait after `failures` failures in a row (1 or more), given a random
/// number for the jitter.
pub fn backoff(r: &Reconnect, failures: u32, random: u64) -> Duration {
    let shift = failures.saturating_sub(1).min(20);
    let d = r.base.saturating_mul(1u32 << shift).min(r.cap);
    let half = d / 2;
    let span = u64::try_from((d - half).as_millis()).unwrap_or(u64::MAX);
    half + Duration::from_millis(random % span.saturating_add(1))
}

/// A random number for jitter, from the standard library's per-instance
/// random hash keys (no extra dependency).
fn random() -> u64 {
    use std::hash::BuildHasher;
    std::collections::hash_map::RandomState::new().hash_one(0u64)
}

/// Resolves once a shutdown is asked for or every [`Handle`] is gone.
async fn stop_asked(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|s| *s).await;
}

async fn run(mut ctx: Ctx) {
    let mut failures: u32 = 0;
    loop {
        ctx.state.send_replace(State::Connecting {
            attempt: failures + 1,
        });
        let mut registered = None;
        let end = session(&mut ctx, &mut registered).await;
        let why = match end {
            End::Stop(stop) => {
                ctx.state.send_replace(State::Stopped(stop));
                return;
            }
            End::Retry(why) => why,
        };
        if registered.is_some_and(|t: Instant| t.elapsed() >= ctx.cfg.reconnect.stable_after) {
            failures = 0;
        }
        failures = failures.saturating_add(1);
        let delay = backoff(&ctx.cfg.reconnect, failures, random());
        ctx.state.send_replace(State::Waiting {
            attempt: failures,
            after: why,
            retry_in: delay,
        });
        tokio::select! {
            biased;
            _ = stop_asked(&mut ctx.shutdown) => {
                ctx.state.send_replace(State::Stopped(Stop::Shutdown));
                return;
            }
            _ = ctx.events.closed() => {
                ctx.state.send_replace(State::Stopped(Stop::Shutdown));
                return;
            }
            _ = tokio::time::sleep(delay) => {}
        }
    }
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

fn classify_connect(e: TcpRouteError) -> End {
    match e {
        TcpRouteError::KillSwitch => End::Retry(Disconnect::KillSwitch),
        TcpRouteError::Net(NetError::Blocked(_) | NetError::InvalidUrl(_))
        | TcpRouteError::Route(_) => End::Stop(Stop::Refused(e)),
        other => End::Retry(Disconnect::Connect(other)),
    }
}

fn is_certificate_error(e: &std::io::Error) -> bool {
    e.get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
        .is_some_and(|r| {
            matches!(
                r,
                rustls::Error::InvalidCertificate(_) | rustls::Error::NoCertificatesPresented
            )
        })
}

/// One connection, from connect to its end.
async fn session(ctx: &mut Ctx, registered: &mut Option<Instant>) -> End {
    let Ctx {
        cfg,
        egress,
        tls,
        events,
        commands,
        state,
        stats,
        shutdown,
    } = ctx;
    let mut down = match &egress.kill {
        Some(k) => k.went_down(),
        None => Box::pin(std::future::pending()),
    };
    let tcp = tokio::select! {
        biased;
        _ = stop_asked(shutdown) => return End::Stop(Stop::Shutdown),
        r = connect_tcp_routed(
            &*egress.resolver,
            &cfg.host,
            cfg.port,
            egress.policy,
            &egress.route,
            egress.kill.as_deref(),
            cfg.timeouts.connect,
        ) => match r {
            Ok(s) => s,
            Err(e) => return classify_connect(e),
        },
    };
    let stream: Box<dyn Io> = match tls {
        Some((connector, name)) => {
            let handshake =
                tokio::time::timeout(cfg.timeouts.connect, connector.connect(name.clone(), tcp));
            tokio::select! {
                biased;
                _ = &mut down => return End::Retry(Disconnect::KillSwitch),
                _ = stop_asked(shutdown) => return End::Stop(Stop::Shutdown),
                r = handshake => match r {
                    Err(_) => return End::Retry(Disconnect::Tls),
                    Ok(Err(e)) if is_certificate_error(&e) => return End::Stop(Stop::Certificate),
                    Ok(Err(_)) => return End::Retry(Disconnect::Tls),
                    Ok(Ok(s)) => Box::new(s),
                },
            }
        }
        None => Box::new(tcp),
    };
    Stats::bump(&stats.connections);
    state.send_replace(State::Registering);
    let (mut rd, mut wr) = tokio::io::split(stream);
    let now = Instant::now();
    let mut proto = Proto::new(Arc::clone(cfg), tls.is_some(), now);
    let mut bucket = Bucket::new(cfg.flood, now);
    let mut lines = LineBuffer::new();
    let mut chunk = vec![0u8; READ_CHUNK];
    let mut shown: Option<String> = None;
    let write_timeout = cfg.timeouts.write;

    loop {
        let now = Instant::now();
        proto.on_tick(now);
        if let Some(end) = proto.end.take() {
            return end;
        }
        while proto.has_output() && bucket.take(now) {
            if let Some(line) = proto.next_line() {
                if write_line(&mut wr, &line, write_timeout).await.is_err() {
                    return End::Retry(Disconnect::Io);
                }
                Stats::bump(&stats.lines_out);
            }
        }
        if proto.ready() {
            if registered.is_none() {
                *registered = Some(now);
            }
            if shown.as_deref() != Some(proto.nick.as_str()) {
                shown = Some(proto.nick.clone());
                state.send_replace(State::Ready {
                    nick: proto.nick.clone(),
                });
            }
        }
        let can_send = proto.ready() && !proto.has_output() && bucket.available(now);
        let mut wake = proto.deadline();
        if let Some(t) = bucket.next_token(now) {
            wake = wake.min(t);
        }
        tokio::select! {
            biased;
            _ = &mut down => return End::Retry(Disconnect::KillSwitch),
            _ = stop_asked(shutdown) => {
                if let Ok(quit) = line::encode("QUIT", &[]) {
                    let _ = write_line(&mut wr, &quit, Duration::from_secs(1)).await;
                }
                return End::Stop(Stop::Shutdown);
            }
            _ = events.closed() => return End::Stop(Stop::Shutdown),
            r = rd.read(&mut chunk) => match r {
                Ok(0) => return End::Retry(Disconnect::Closed),
                Err(_) => return End::Retry(Disconnect::Io),
                Ok(n) => {
                    let now = Instant::now();
                    lines.feed(&chunk[..n], |f| on_framed(f, &mut proto, now, events, stats));
                }
            },
            Some(line) = commands.recv(), if can_send => {
                bucket.take(Instant::now());
                if write_line(&mut wr, &line, write_timeout).await.is_err() {
                    return End::Retry(Disconnect::Io);
                }
                Stats::bump(&stats.lines_out);
            }
            _ = tokio::time::sleep_until(wake) => {}
        }
    }
}

fn on_framed(
    f: Framed<'_>,
    proto: &mut Proto,
    now: Instant,
    events: &mpsc::Sender<Message>,
    stats: &Stats,
) {
    if proto.end.is_some() {
        return;
    }
    let raw = match f {
        Framed::Overlong => {
            Stats::bump(&stats.overlong);
            return;
        }
        Framed::Line(raw) => raw,
    };
    let msg = match line::parse(raw) {
        Ok(Some(m)) => m,
        Ok(None) => return,
        Err(_) => {
            Stats::bump(&stats.malformed);
            return;
        }
    };
    Stats::bump(&stats.lines_in);
    match proto.on_message(msg, now) {
        Handled::Emit(m) => {
            if let Err(mpsc::error::TrySendError::Full(_)) = events.try_send(m) {
                Stats::bump(&stats.events_dropped);
            }
        }
        Handled::Ctcp => Stats::bump(&stats.ctcp_ignored),
        Handled::Internal => {}
    }
}

async fn write_line<W: AsyncWrite + Unpin>(
    wr: &mut W,
    line: &[u8],
    timeout: Duration,
) -> Result<(), ()> {
    match tokio::time::timeout(timeout, async {
        wr.write_all(line).await?;
        wr.flush().await
    })
    .await
    {
        Ok(Ok(())) => Ok(()),
        _ => Err(()),
    }
}

/// Outgoing flood control.
#[derive(Debug)]
pub(crate) struct Bucket {
    tokens: u32,
    burst: u32,
    interval: Duration,
    /// When the last token was added (or the bucket was last full).
    last: Instant,
}

impl Bucket {
    pub(crate) fn new(flood: Flood, now: Instant) -> Bucket {
        Bucket {
            tokens: flood.burst,
            burst: flood.burst,
            interval: flood.interval,
            last: now,
        }
    }

    fn refill(&mut self, now: Instant) {
        if self.tokens >= self.burst {
            self.last = now;
            return;
        }
        let elapsed = now.saturating_duration_since(self.last);
        let n = elapsed.as_nanos() / self.interval.as_nanos().max(1);
        if n == 0 {
            return;
        }
        let n = u32::try_from(n).unwrap_or(u32::MAX);
        self.tokens = self.tokens.saturating_add(n).min(self.burst);
        if self.tokens >= self.burst {
            self.last = now;
        } else {
            self.last += self.interval * n;
        }
    }

    pub(crate) fn available(&mut self, now: Instant) -> bool {
        self.refill(now);
        self.tokens > 0
    }

    pub(crate) fn take(&mut self, now: Instant) -> bool {
        self.refill(now);
        if self.tokens == 0 {
            return false;
        }
        self.tokens -= 1;
        true
    }

    /// When the next token arrives, if the bucket isn't full.
    pub(crate) fn next_token(&mut self, now: Instant) -> Option<Instant> {
        self.refill(now);
        (self.tokens < self.burst).then(|| self.last + self.interval)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// `CAP LS 302` sent; reading the list.
    CapLs,
    /// `CAP REQ :sasl` sent.
    CapReq,
    /// `AUTHENTICATE PLAIN` sent; waiting for `AUTHENTICATE +`.
    SaslMech,
    /// The credentials sent; waiting for 903.
    SaslResult,
    /// Waiting for the welcome (001).
    Registering,
    /// NickServ `IDENTIFY` sent; joins wait for 900 or this deadline.
    Identifying(Instant),
    Ready,
}

/// What the session does with a parsed message.
#[derive(Debug)]
pub(crate) enum Handled {
    Emit(Message),
    Ctcp,
    Internal,
}

/// The protocol state machine for one connection, with no I/O: it reads
/// messages and the clock, and queues lines.
pub(crate) struct Proto {
    cfg: Arc<Config>,
    tls: bool,
    phase: Phase,
    started: Instant,
    /// The nick the server knows the client by.
    nick: String,
    nick_tries: u8,
    cap_lines: u8,
    sasl_offered: bool,
    /// Per configured channel.
    joined: Vec<bool>,
    join_sent: Vec<bool>,
    out: VecDeque<Zeroizing<Vec<u8>>>,
    /// At most one `PONG` waits; a newer `PING` replaces it.
    pong: Option<Zeroizing<Vec<u8>>>,
    last_rx: Instant,
    ping_sent: bool,
    pub(crate) end: Option<End>,
}

impl Proto {
    pub(crate) fn new(cfg: Arc<Config>, tls: bool, now: Instant) -> Proto {
        let n = cfg.channels.len();
        let mut p = Proto {
            nick: cfg.nick.clone(),
            cfg,
            tls,
            phase: Phase::Registering,
            started: now,
            nick_tries: 0,
            cap_lines: 0,
            sasl_offered: false,
            joined: vec![false; n],
            join_sent: vec![false; n],
            out: VecDeque::new(),
            pong: None,
            last_rx: now,
            ping_sent: false,
            end: None,
        };
        p.start();
        p
    }

    fn start(&mut self) {
        let cfg = Arc::clone(&self.cfg);
        if matches!(cfg.auth, Auth::SaslPlain { .. }) {
            self.phase = Phase::CapLs;
            self.send("CAP", &["LS", "302"]);
        }
        if let Some(pw) = &cfg.server_password {
            if !self.tls {
                self.stop(Stop::Internal);
                return;
            }
            self.send("PASS", &[pw.expose()]);
        }
        self.send("NICK", &[&cfg.nick]);
        self.send("USER", &[&cfg.username, "0", "*", &cfg.realname]);
    }

    fn stop(&mut self, s: Stop) {
        if self.end.is_none() {
            self.end = Some(End::Stop(s));
        }
    }

    fn retry(&mut self, d: Disconnect) {
        if self.end.is_none() {
            self.end = Some(End::Retry(d));
        }
    }

    fn send(&mut self, cmd: &str, params: &[&str]) {
        match line::encode(cmd, params) {
            Ok(l) if self.out.len() < OUT_QUEUE => self.out.push_back(l),
            Ok(_) => self.retry(Disconnect::ServerFlood),
            Err(_) => self.stop(Stop::Internal),
        }
    }

    pub(crate) fn has_output(&self) -> bool {
        self.pong.is_some() || !self.out.is_empty()
    }

    pub(crate) fn next_line(&mut self) -> Option<Zeroizing<Vec<u8>>> {
        self.pong.take().or_else(|| self.out.pop_front())
    }

    pub(crate) fn ready(&self) -> bool {
        self.phase == Phase::Ready
    }

    fn registered(&self) -> bool {
        matches!(self.phase, Phase::Identifying(_) | Phase::Ready)
    }

    fn in_sasl(&self) -> bool {
        matches!(
            self.phase,
            Phase::CapLs | Phase::CapReq | Phase::SaslMech | Phase::SaslResult
        )
    }

    /// The next time [`Proto::on_tick`] has something to do.
    pub(crate) fn deadline(&self) -> Instant {
        let t = self.cfg.timeouts;
        let mut d = if self.ping_sent {
            self.last_rx + t.ping_after + t.ping_timeout
        } else {
            self.last_rx + t.ping_after
        };
        if !self.registered() {
            d = d.min(self.started + t.register);
        }
        if let Phase::Identifying(until) = self.phase {
            d = d.min(until);
        }
        d
    }

    pub(crate) fn on_tick(&mut self, now: Instant) {
        let t = self.cfg.timeouts;
        if !self.registered() && now >= self.started + t.register {
            self.retry(Disconnect::RegisterTimeout);
            return;
        }
        if let Phase::Identifying(until) = self.phase
            && now >= until
        {
            self.join_all();
            self.phase = Phase::Ready;
        }
        let idle = now.saturating_duration_since(self.last_rx);
        if idle >= t.ping_after + t.ping_timeout {
            self.retry(Disconnect::PingTimeout);
        } else if idle >= t.ping_after && !self.ping_sent {
            self.ping_sent = true;
            self.send("PING", &["lodestar"]);
        }
    }

    pub(crate) fn on_message(&mut self, msg: Message, now: Instant) -> Handled {
        self.last_rx = now;
        self.ping_sent = false;
        if msg.ctcp().is_some() {
            return Handled::Ctcp;
        }
        match msg.command.as_str() {
            "PING" => {
                let token = msg.param(0).unwrap_or("");
                // A token that can't be echoed in a line is answered empty.
                self.pong = line::encode("PONG", &[token])
                    .or_else(|_| line::encode("PONG", &[""]))
                    .ok();
                Handled::Internal
            }
            "CAP" => {
                self.on_cap(&msg);
                Handled::Internal
            }
            "AUTHENTICATE" => {
                self.on_authenticate(&msg);
                Handled::Internal
            }
            "ERROR" => {
                self.retry(Disconnect::ServerError);
                Handled::Internal
            }
            "001" => {
                self.on_welcome(&msg, now);
                Handled::Emit(msg)
            }
            "433" | "436" if !self.registered() => {
                self.next_nick();
                Handled::Emit(msg)
            }
            "432" if !self.registered() => {
                self.stop(Stop::NickRejected);
                Handled::Emit(msg)
            }
            "465" => {
                self.stop(Stop::Banned);
                Handled::Emit(msg)
            }
            "900" | "901" | "902" | "903" | "904" | "905" | "906" | "907" | "908" => {
                self.on_sasl_numeric(&msg.command);
                Handled::Internal
            }
            // A join that failed, so a later invitation may retry it.
            "403" | "405" | "471" | "473" | "474" | "475" | "477" => {
                if let Some(i) = msg.param(1).and_then(|c| self.channel_index(c)) {
                    self.join_sent[i] = false;
                }
                Handled::Emit(msg)
            }
            "INVITE" => {
                self.on_invite(&msg);
                Handled::Emit(msg)
            }
            "JOIN" | "PART" | "KICK" | "NICK" => {
                self.on_membership(&msg);
                Handled::Emit(msg)
            }
            _ => Handled::Emit(msg),
        }
    }

    fn on_cap(&mut self, msg: &Message) {
        let sub = msg.param(1).unwrap_or("").to_ascii_uppercase();
        match (self.phase, sub.as_str()) {
            (Phase::CapLs, "LS") => {
                let more = msg.params.len() >= 4 && msg.param(2) == Some("*");
                let list = msg.params.last().map(String::as_str).unwrap_or("");
                self.cap_lines += 1;
                if self.cap_lines > MAX_CAP_LS_LINES {
                    self.retry(Disconnect::Protocol("CAP LS too long"));
                    return;
                }
                if line::cap_entries(list).any(|c| {
                    c.name.eq_ignore_ascii_case("sasl")
                        && !c.removed
                        && line::sasl_allows(c.value, "PLAIN")
                }) {
                    self.sasl_offered = true;
                }
                if !more {
                    if self.sasl_offered {
                        self.phase = Phase::CapReq;
                        self.send("CAP", &["REQ", "sasl"]);
                    } else {
                        self.stop(Stop::SaslUnavailable);
                    }
                }
            }
            (Phase::CapReq, "ACK") => {
                let list = msg.params.last().map(String::as_str).unwrap_or("");
                if line::cap_entries(list)
                    .any(|c| c.name.eq_ignore_ascii_case("sasl") && !c.removed)
                {
                    self.phase = Phase::SaslMech;
                    self.send("AUTHENTICATE", &["PLAIN"]);
                } else {
                    self.stop(Stop::SaslUnavailable);
                }
            }
            (Phase::CapReq, "NAK") => self.stop(Stop::SaslUnavailable),
            _ => {}
        }
    }

    fn on_authenticate(&mut self, msg: &Message) {
        if self.phase != Phase::SaslMech {
            return;
        }
        if msg.param(0) != Some("+") {
            self.retry(Disconnect::Protocol("unexpected AUTHENTICATE"));
            return;
        }
        let Auth::SaslPlain { account, password } = &self.cfg.auth else {
            self.stop(Stop::Internal);
            return;
        };
        // Never without TLS, whatever the config said.
        if !self.tls {
            self.stop(Stop::Internal);
            return;
        }
        let mut raw = Zeroizing::new(Vec::with_capacity(
            2 * account.len() + password.expose().len() + 2,
        ));
        raw.extend_from_slice(account.as_bytes());
        raw.push(0);
        raw.extend_from_slice(account.as_bytes());
        raw.push(0);
        raw.extend_from_slice(password.expose().as_bytes());
        let encoded = base64(&raw);
        let bytes = encoded.as_bytes();
        let mut chunks = bytes.chunks(SASL_CHUNK).peekable();
        while let Some(c) = chunks.next() {
            // Base64 is ASCII, so every chunk is valid UTF-8.
            let c = std::str::from_utf8(c).unwrap_or("+");
            self.send("AUTHENTICATE", &[c]);
            if chunks.peek().is_none() && c.len() == SASL_CHUNK {
                self.send("AUTHENTICATE", &["+"]);
            }
        }
        self.phase = Phase::SaslResult;
    }

    fn on_sasl_numeric(&mut self, code: &str) {
        match code {
            "900" => {
                if let Phase::Identifying(_) = self.phase {
                    self.join_all();
                    self.phase = Phase::Ready;
                }
            }
            "903" | "907" if self.phase == Phase::SaslResult => {
                self.send("CAP", &["END"]);
                self.phase = Phase::Registering;
            }
            "902" | "904" | "905" | "906" if self.in_sasl() => self.stop(Stop::AuthFailed),
            _ => {}
        }
    }

    fn on_welcome(&mut self, msg: &Message, now: Instant) {
        if self.in_sasl() {
            // Registered without the login the config asks for.
            self.stop(Stop::SaslUnavailable);
            return;
        }
        if self.phase != Phase::Registering {
            return;
        }
        if let Some(n) = msg.param(0)
            && valid_nick(n)
        {
            self.nick = n.to_owned();
        }
        let cfg = Arc::clone(&self.cfg);
        if let Auth::NickServ { account, password } = &cfg.auth {
            if !self.tls {
                self.stop(Stop::Internal);
                return;
            }
            let text = Zeroizing::new(format!("IDENTIFY {account} {}", password.expose()));
            self.send("PRIVMSG", &["NickServ", &text]);
            self.phase = Phase::Identifying(now + cfg.timeouts.nickserv);
        } else {
            self.join_all();
            self.phase = Phase::Ready;
        }
    }

    fn next_nick(&mut self) {
        self.nick_tries += 1;
        if self.nick_tries > NICK_RETRIES {
            self.retry(Disconnect::NickInUse);
            return;
        }
        let tries = usize::from(self.nick_tries);
        let base: String = self.cfg.nick.chars().take(30 - tries).collect();
        self.nick = format!("{base}{}", "_".repeat(tries));
        let nick = self.nick.clone();
        self.send("NICK", &[&nick]);
    }

    fn join_all(&mut self) {
        for i in 0..self.cfg.channels.len() {
            self.join(i);
        }
    }

    fn join(&mut self, i: usize) {
        if self.joined[i] || self.join_sent[i] {
            return;
        }
        let cfg = Arc::clone(&self.cfg);
        let c = &cfg.channels[i];
        match &c.key {
            Some(k) if self.tls => self.send("JOIN", &[&c.name, k.expose()]),
            Some(_) => self.stop(Stop::Internal),
            None => self.send("JOIN", &[&c.name]),
        }
        self.join_sent[i] = true;
    }

    fn channel_index(&self, name: &str) -> Option<usize> {
        self.cfg.channels.iter().position(|c| irc_eq(&c.name, name))
    }

    fn is_me(&self, nick: Option<&str>) -> bool {
        nick.is_some_and(|n| irc_eq(n, &self.nick))
    }

    fn on_invite(&mut self, msg: &Message) {
        if self.phase != Phase::Ready || !self.is_me(msg.param(0)) {
            return;
        }
        if let Some(i) = msg.param(1).and_then(|c| self.channel_index(c)) {
            self.join(i);
        }
    }

    fn on_membership(&mut self, msg: &Message) {
        match msg.command.as_str() {
            "JOIN" if self.is_me(msg.nick()) => {
                if let Some(i) = msg.param(0).and_then(|c| self.channel_index(c)) {
                    self.joined[i] = true;
                    self.join_sent[i] = false;
                }
            }
            "PART" if self.is_me(msg.nick()) => {
                if let Some(i) = msg.param(0).and_then(|c| self.channel_index(c)) {
                    self.joined[i] = false;
                    self.join_sent[i] = false;
                }
            }
            // Kicked: not rejoined on its own; an invitation can bring it
            // back.
            "KICK" if self.is_me(msg.param(1)) => {
                if let Some(i) = msg.param(0).and_then(|c| self.channel_index(c)) {
                    self.joined[i] = false;
                    self.join_sent[i] = false;
                }
            }
            "NICK" if self.is_me(msg.nick()) => {
                if let Some(n) = msg.param(0)
                    && valid_nick(n)
                {
                    self.nick = n.to_owned();
                }
            }
            _ => {}
        }
    }
}

/// Standard base64 with padding, into a buffer wiped on drop (it holds the
/// SASL credentials).
fn base64(data: &[u8]) -> Zeroizing<String> {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Zeroizing::new(String::with_capacity(data.len().div_ceil(3) * 4));
    for chunk in data.chunks(3) {
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(chunk[0]) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        let sym = |shift: u32| char::from(A[((n >> shift) & 63) as usize]);
        out.push(sym(18));
        out.push(sym(12));
        out.push(if chunk.len() > 1 { sym(6) } else { '=' });
        out.push(if chunk.len() > 2 { sym(0) } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(f: impl FnOnce(&mut Config)) -> Arc<Config> {
        let mut c = Config::new("irc.example.test", "lode");
        c.channels = vec![Channel::new("#announce"), Channel::new("#pre")];
        f(&mut c);
        Arc::new(c)
    }

    fn msg(line: &str) -> Message {
        line::parse(line.as_bytes()).unwrap().unwrap()
    }

    /// Every queued line, as text.
    fn drain(p: &mut Proto) -> Vec<String> {
        std::iter::from_fn(|| p.next_line())
            .map(|l| String::from_utf8_lossy(&l).trim_end().to_owned())
            .collect()
    }

    fn sasl() -> Auth {
        Auth::SaslPlain {
            account: "acct".into(),
            password: Secret::new("pw-test"),
        }
    }

    fn nickserv() -> Auth {
        Auth::NickServ {
            account: "acct".into(),
            password: Secret::new("pw-test"),
        }
    }

    /// A SASL proto past `CAP ACK`.
    fn acked(auth: Auth, tls: bool, now: Instant) -> Proto {
        let mut p = Proto::new(cfg(|c| c.auth = auth), tls, now);
        p.on_message(msg(":s CAP * LS :sasl"), now);
        p.on_message(msg(":s CAP * ACK :sasl"), now);
        drain(&mut p);
        p
    }

    #[tokio::test]
    async fn sasl_plain_registers_then_joins() {
        let now = Instant::now();
        let mut p = Proto::new(cfg(|c| c.auth = sasl()), true, now);
        assert_eq!(
            drain(&mut p),
            ["CAP LS 302", "NICK lode", "USER lodestar 0 * Lodestar"]
        );
        p.on_message(msg(":s CAP * LS * :multi-prefix"), now);
        assert!(drain(&mut p).is_empty());
        p.on_message(msg(":s CAP * LS :sasl=EXTERNAL,PLAIN"), now);
        assert_eq!(drain(&mut p), ["CAP REQ sasl"]);
        p.on_message(msg(":s CAP * ACK :sasl"), now);
        assert_eq!(drain(&mut p), ["AUTHENTICATE PLAIN"]);
        p.on_message(msg("AUTHENTICATE +"), now);
        // base64("acct\0acct\0pw-test")
        assert_eq!(drain(&mut p), ["AUTHENTICATE YWNjdABhY2N0AHB3LXRlc3Q="]);
        p.on_message(msg(":s 903 lode :SASL authentication successful"), now);
        assert_eq!(drain(&mut p), ["CAP END"]);
        let welcome = p.on_message(msg(":s 001 lode_x :Welcome"), now);
        assert!(matches!(welcome, Handled::Emit(_)));
        assert_eq!(drain(&mut p), ["JOIN #announce", "JOIN #pre"]);
        assert!(p.ready());
        assert_eq!(p.nick, "lode_x");
        assert_eq!(p.end, None);
    }

    #[tokio::test]
    async fn sasl_fails_closed() {
        let now = Instant::now();
        let fresh = || Proto::new(cfg(|c| c.auth = sasl()), true, now);
        // Not offered with PLAIN.
        let mut p = fresh();
        p.on_message(msg(":s CAP * LS :multi-prefix sasl=EXTERNAL"), now);
        assert_eq!(p.end, Some(End::Stop(Stop::SaslUnavailable)));
        // Refused.
        let mut p = fresh();
        p.on_message(msg(":s CAP * LS :sasl"), now);
        p.on_message(msg(":s CAP * NAK :sasl"), now);
        assert_eq!(p.end, Some(End::Stop(Stop::SaslUnavailable)));
        // A server without CAP that registers the client anyway.
        let mut p = fresh();
        p.on_message(msg(":s 421 * CAP :Unknown command"), now);
        p.on_message(msg(":s 001 lode :Welcome"), now);
        assert_eq!(p.end, Some(End::Stop(Stop::SaslUnavailable)));
        assert!(!drain(&mut p).iter().any(|l| l.starts_with("JOIN")));
        // A wrong password.
        let mut p = acked(sasl(), true, now);
        p.on_message(msg("AUTHENTICATE +"), now);
        p.on_message(msg(":s 904 lode :SASL authentication failed"), now);
        assert_eq!(p.end, Some(End::Stop(Stop::AuthFailed)));
        // An `AUTHENTICATE +` the client didn't ask for gets nothing.
        let mut p = fresh();
        drain(&mut p);
        p.on_message(msg("AUTHENTICATE +"), now);
        assert!(drain(&mut p).is_empty());
        assert_eq!(p.end, None);
        // Too many CAP LS lines.
        let mut p = fresh();
        for _ in 0..=MAX_CAP_LS_LINES {
            p.on_message(msg(":s CAP * LS * :a"), now);
        }
        assert!(matches!(p.end, Some(End::Retry(Disconnect::Protocol(_)))));
    }

    /// Defence in depth: even a config that skipped validation never puts
    /// a credential on a plaintext connection.
    #[tokio::test]
    async fn credentials_never_go_out_without_tls() {
        let now = Instant::now();
        let mut p = acked(sasl(), false, now);
        p.on_message(msg("AUTHENTICATE +"), now);
        assert_eq!(p.end, Some(End::Stop(Stop::Internal)));
        assert!(drain(&mut p).is_empty());

        let pass = cfg(|c| c.server_password = Some(Secret::new("pw-test")));
        let mut p = Proto::new(pass, false, now);
        assert_eq!(p.end, Some(End::Stop(Stop::Internal)));
        assert!(!drain(&mut p).iter().any(|l| l.contains("pw-test")));

        let mut p = Proto::new(cfg(|c| c.auth = nickserv()), false, now);
        p.on_message(msg(":s 001 lode :Welcome"), now);
        assert_eq!(p.end, Some(End::Stop(Stop::Internal)));
        assert!(!drain(&mut p).iter().any(|l| l.contains("pw-test")));

        let key = cfg(|c| c.channels[0].key = Some(Secret::new("chan-key")));
        let mut p = Proto::new(key, false, now);
        p.on_message(msg(":s 001 lode :Welcome"), now);
        assert_eq!(p.end, Some(End::Stop(Stop::Internal)));
        assert!(!drain(&mut p).iter().any(|l| l.contains("chan-key")));
    }

    #[tokio::test]
    async fn nickserv_identifies_then_joins_on_900_or_deadline() {
        let now = Instant::now();
        let c = cfg(|c| c.auth = nickserv());
        let mut p = Proto::new(Arc::clone(&c), true, now);
        drain(&mut p);
        p.on_message(msg(":s 001 lode :Welcome"), now);
        assert_eq!(drain(&mut p), ["PRIVMSG NickServ :IDENTIFY acct pw-test"]);
        assert!(!p.ready());
        p.on_message(msg(":s 900 lode lode!u@h acct :You are now logged in"), now);
        assert_eq!(drain(&mut p), ["JOIN #announce", "JOIN #pre"]);
        assert!(p.ready());

        let mut p = Proto::new(c, true, now);
        p.on_message(msg(":s 001 lode :Welcome"), now);
        drain(&mut p);
        let wait = Timeouts::default().nickserv;
        assert_eq!(p.deadline(), now + wait);
        p.on_tick(now + wait);
        assert_eq!(drain(&mut p), ["JOIN #announce", "JOIN #pre"]);
    }

    #[tokio::test]
    async fn pings_ctcp_invites_and_membership() {
        let now = Instant::now();
        let mut p = Proto::new(cfg(|_| {}), true, now);
        p.on_message(msg(":s 001 lode :Welcome"), now);
        drain(&mut p);
        // Only the newest PONG waits, and it goes first.
        p.on_message(msg("PING :a"), now);
        p.on_message(msg("PING :b"), now);
        p.send("PRIVMSG", &["#x", "queued"]);
        assert_eq!(drain(&mut p), ["PONG b", "PRIVMSG #x queued"]);
        // CTCP, DCC included, is never answered.
        let version = p.on_message(msg(":n PRIVMSG lode :\u{1}VERSION\u{1}"), now);
        assert!(matches!(version, Handled::Ctcp));
        let dcc = p.on_message(msg(":n PRIVMSG lode :\u{1}DCC SEND x 1 2 3\u{1}"), now);
        assert!(matches!(dcc, Handled::Ctcp));
        assert!(drain(&mut p).is_empty());
        // Invitations: only configured channels, only when not joined.
        p.on_message(msg(":bot INVITE lode #elsewhere"), now);
        p.on_message(msg(":bot INVITE someone #announce"), now);
        assert!(drain(&mut p).is_empty());
        p.on_message(msg(":lode!u@h JOIN #announce"), now);
        p.on_message(msg(":bot INVITE lode #ANNOUNCE"), now);
        assert!(drain(&mut p).is_empty());
        p.on_message(msg(":op KICK #announce lode :bye"), now);
        assert!(drain(&mut p).is_empty(), "a kick isn't rejoined on its own");
        p.on_message(msg(":bot INVITE lode #announce"), now);
        assert_eq!(drain(&mut p), ["JOIN #announce"]);
        // A failed join can be retried by a later invitation.
        p.on_message(msg(":s 473 lode #pre :Cannot join channel (+i)"), now);
        p.on_message(msg(":bot INVITE lode #pre"), now);
        assert_eq!(drain(&mut p), ["JOIN #pre"]);
        // Nick changes follow the server.
        p.on_message(msg(":lode!u@h NICK :lode2"), now);
        assert_eq!(p.nick, "lode2");
        p.on_message(msg(":s ERROR :Closing link"), now);
        assert_eq!(p.end, Some(End::Retry(Disconnect::ServerError)));
    }

    #[tokio::test]
    async fn nick_in_use_and_rejected() {
        let now = Instant::now();
        let in_use = ":s 433 * lode :Nickname is already in use";
        let mut p = Proto::new(cfg(|_| {}), true, now);
        drain(&mut p);
        for want in ["NICK lode_", "NICK lode__", "NICK lode___"] {
            p.on_message(msg(in_use), now);
            assert_eq!(drain(&mut p), [want]);
        }
        p.on_message(msg(in_use), now);
        assert_eq!(p.end, Some(End::Retry(Disconnect::NickInUse)));

        let mut p = Proto::new(cfg(|_| {}), true, now);
        p.on_message(msg(":s 432 * lode :Erroneous nickname"), now);
        assert_eq!(p.end, Some(End::Stop(Stop::NickRejected)));
        let mut p = Proto::new(cfg(|_| {}), true, now);
        p.on_message(msg(":s 465 * :You are banned"), now);
        assert_eq!(p.end, Some(End::Stop(Stop::Banned)));
    }

    #[tokio::test]
    async fn timers() {
        let now = Instant::now();
        let t = Timeouts::default();
        let mut p = Proto::new(cfg(|_| {}), true, now);
        drain(&mut p);
        assert_eq!(p.deadline(), now + t.register);
        p.on_tick(now + t.register);
        assert_eq!(p.end, Some(End::Retry(Disconnect::RegisterTimeout)));

        let mut p = Proto::new(cfg(|_| {}), true, now);
        p.on_message(msg(":s 001 lode :Welcome"), now);
        drain(&mut p);
        assert_eq!(p.deadline(), now + t.ping_after);
        p.on_tick(now + t.ping_after);
        assert_eq!(drain(&mut p), ["PING lodestar"]);
        p.on_tick(now + t.ping_after + Duration::from_secs(1));
        assert!(drain(&mut p).is_empty(), "one PING per silence");
        assert_eq!(p.deadline(), now + t.ping_after + t.ping_timeout);
        // Any line ends the silence.
        let later = now + t.ping_after + Duration::from_secs(5);
        p.on_message(msg(":s PONG s :lodestar"), later);
        assert_eq!(p.deadline(), later + t.ping_after);
        p.on_tick(later + t.ping_after);
        drain(&mut p);
        p.on_tick(later + t.ping_after + t.ping_timeout);
        assert_eq!(p.end, Some(End::Retry(Disconnect::PingTimeout)));
    }

    #[tokio::test]
    async fn a_flooding_server_is_disconnected() {
        let now = Instant::now();
        let mut p = Proto::new(cfg(|_| {}), true, now);
        for _ in 0..OUT_QUEUE {
            p.send("PRIVMSG", &["#x", "y"]);
        }
        assert_eq!(p.end, Some(End::Retry(Disconnect::ServerFlood)));
        assert!(p.out.len() <= OUT_QUEUE);
    }

    #[tokio::test]
    async fn token_bucket() {
        let now = Instant::now();
        let s = Duration::from_secs;
        let flood = Flood {
            burst: 3,
            interval: s(2),
        };
        let mut b = Bucket::new(flood, now);
        assert_eq!(b.next_token(now), None);
        assert!(b.take(now) && b.take(now) && b.take(now));
        assert!(!b.take(now));
        assert_eq!(b.next_token(now), Some(now + s(2)));
        assert!(!b.take(now + Duration::from_millis(1999)));
        assert!(b.take(now + s(2)));
        assert!(!b.take(now + s(3)));
        // A long idle refills to the burst, never past it.
        let idle = now + s(100);
        assert!(b.available(idle));
        assert!(b.take(idle) && b.take(idle) && b.take(idle));
        assert!(!b.take(idle));
    }

    #[test]
    fn backoff_doubles_jitters_and_caps() {
        let r = Reconnect::default();
        let s = Duration::from_secs;
        for random in [0, 1, 7_777, u64::MAX] {
            let d1 = backoff(&r, 1, random);
            assert!(d1 >= s(5) / 2 && d1 <= s(5), "{d1:?}");
            let d3 = backoff(&r, 3, random);
            assert!(d3 >= s(10) && d3 <= s(20), "{d3:?}");
            let d = backoff(&r, 1000, random);
            assert!(d >= s(300) && d <= s(600), "{d:?}");
        }
        assert_ne!(random(), random());
    }

    #[test]
    fn base64_vectors() {
        for (input, want) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()).as_str(), want);
        }
    }

    #[tokio::test]
    async fn long_sasl_payloads_are_chunked() {
        let now = Instant::now();
        let auth = |account: usize, password: usize| Auth::SaslPlain {
            account: "a".repeat(account),
            password: Secret::new("p".repeat(password)),
        };
        // 64 + 64 + 234 + 2 = 364 bytes: 488 characters, two lines.
        let mut p = acked(auth(64, 234), true, now);
        p.on_message(msg("AUTHENTICATE +"), now);
        let lines = drain(&mut p);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), "AUTHENTICATE ".len() + SASL_CHUNK);
        // 32 + 32 + 234 + 2 = 300 bytes: exactly 400 characters, so an
        // empty `+` line follows.
        let mut p = acked(auth(32, 234), true, now);
        p.on_message(msg("AUTHENTICATE +"), now);
        let lines = drain(&mut p);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), "AUTHENTICATE ".len() + SASL_CHUNK);
        assert_eq!(lines[1], "AUTHENTICATE +");
    }

    #[test]
    fn config_validation() {
        let check = |f: fn(&mut Config)| {
            let mut c = (*cfg(|_| {})).clone();
            f(&mut c);
            c.validate()
        };
        let invalid = |what| Err(ConfigError::Invalid(what));
        assert_eq!(check(|_| {}), Ok(()));
        assert_eq!(check(|c| c.nick = "1abc".into()), invalid("nick"));
        assert_eq!(check(|c| c.nick = "a b".into()), invalid("nick"));
        assert_eq!(check(|c| c.host = String::new()), invalid("host"));
        assert_eq!(check(|c| c.port = 0), invalid("port"));
        assert_eq!(
            check(|c| c.realname = "a\r\nQUIT".into()),
            invalid("realname")
        );
        assert_eq!(check(|c| c.username = "a b".into()), invalid("username"));
        let comma = |c: &mut Config| c.channels = vec![Channel::new("#a,#b")];
        assert_eq!(check(comma), invalid("channel"));
        let no_prefix = |c: &mut Config| c.channels = vec![Channel::new("nohash")];
        assert_eq!(check(no_prefix), invalid("channel"));
        let many = |c: &mut Config| {
            c.channels = (0..33).map(|i| Channel::new(format!("#c{i}"))).collect();
        };
        assert_eq!(check(many), invalid("channels"));
        let key = |c: &mut Config| c.channels[0].key = Some(Secret::new("a b"));
        assert_eq!(check(key), invalid("channel key"));
        let fast = |c: &mut Config| c.flood.interval = Duration::from_millis(100);
        assert_eq!(check(fast), invalid("flood control"));
        let burst = |c: &mut Config| c.flood.burst = 0;
        assert_eq!(check(burst), invalid("flood control"));
        let eager = |c: &mut Config| c.reconnect.base = Duration::from_millis(10);
        assert_eq!(check(eager), invalid("reconnect"));
        let ping = |c: &mut Config| c.timeouts.ping_after = Duration::ZERO;
        assert_eq!(check(ping), invalid("timeouts"));
        let spaced = |c: &mut Config| {
            c.auth = Auth::SaslPlain {
                account: "a".into(),
                password: Secret::new("has space"),
            };
        };
        assert_eq!(check(spaced), invalid("password"));
        let no_roots = |c: &mut Config| c.roots = Roots::Only(Vec::new());
        assert_eq!(check(no_roots), invalid("trusted certificates"));

        // Credentials of any kind need TLS.
        let plain = |f: fn(&mut Config)| {
            let mut c = (*cfg(|_| {})).clone();
            c.transport = Transport::Plaintext;
            f(&mut c);
            c.validate()
        };
        let need_tls = Err(ConfigError::CredentialsNeedTls);
        assert_eq!(plain(|_| {}), Ok(()));
        assert_eq!(plain(|c| c.auth = sasl()), need_tls);
        assert_eq!(plain(|c| c.auth = nickserv()), need_tls);
        let pass = |c: &mut Config| c.server_password = Some(Secret::new("pw-test"));
        assert_eq!(plain(pass), need_tls);
        let key = |c: &mut Config| c.channels[0].key = Some(Secret::new("k"));
        assert_eq!(plain(key), need_tls);
    }

    #[test]
    fn debug_never_shows_credentials() {
        let mut c = (*cfg(|c| c.auth = sasl())).clone();
        c.server_password = Some(Secret::new("server-pw-test"));
        c.channels[0].key = Some(Secret::new("chan-key-test"));
        let text = format!("{c:?}");
        for secret in ["pw-test", "acct", "chan-key-test"] {
            assert!(!text.contains(secret), "{text}");
        }
        assert!(text.contains("#announce"));
        let n = format!("{:?}", nickserv());
        assert!(!n.contains("acct") && !n.contains("pw-test"));
    }
}
