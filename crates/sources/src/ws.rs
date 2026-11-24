//! A WebSocket client for source plugins (§18.1, §18.6, §12; NOS-2b),
//! opened only through the plugin's egress ([`PluginEgress`]'s
//! [`WsConnect`]), so it takes the same route, kill switch, endpoint
//! policy and source state as the plugin's HTTP.
//!
//! - **`wss://` only.** The server's certificate and name are checked with
//!   the platform verifier the plugin HTTP client uses ([`Roots`]).
//! - **Caps.** A message (and so a frame) is at most [`MAX_MESSAGE`]
//!   (128 KiB) each way; nothing larger is buffered. No extension is
//!   offered (so no permessage-deflate), and a server that answers with one
//!   is refused. The handshake answer is bounded by tungstenite's own check
//!   (64 KiB of headers) and the connect timeout.
//! - **Liveness.** A ping goes out every [`PING_EVERY`] (60 s); a session
//!   that receives nothing at all (no message, ping or pong) for the idle
//!   timeout ends with [`WsError::Idle`]. A send that can't be written
//!   within the idle timeout ends it too.
//! - **Kill switch.** An open session stays under its plugin's kill
//!   switch, as a streamed HTTP body does: a route that goes down ends it
//!   with [`WsError::EgressDown`] and closes the socket at once.
//! - **Reconnecting** is the plugin's loop; [`Reconnect`] gives it the
//!   back-off, from the plugin's own back-off (its manifest's floors).
//! - **Errors** name no URL, address, certificate name or payload.
//!
//! Tests replay transcripts instead ([`crate::testing::WsReplayer`]).
//!
//! [`PluginEgress`]: crate::PluginEgress

use std::fmt;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::Instant;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{self, Message};

use crate::http::{HttpError, backoff_delay};
use crate::manifest::{Backoff, floors};
use crate::plugin::BoxFuture;

/// The largest message (and frame) either way.
pub const MAX_MESSAGE: usize = 128 * 1024;
/// How often a ping goes out, at the most.
pub const PING_EVERY: Duration = Duration::from_secs(60);
/// The shortest ping interval a plugin may ask for.
pub const MIN_PING: Duration = Duration::from_secs(1);
/// The default idle timeout: two missed pings and some slack.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(150);
/// The longest idle timeout.
pub const MAX_IDLE: Duration = Duration::from_secs(600);
/// The default connect timeout: resolving, connecting, the proxy's
/// handshake, TLS and the upgrade, together.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// The shortest and longest connect timeouts.
pub const MIN_CONNECT: Duration = Duration::from_secs(1);
pub const MAX_CONNECT: Duration = Duration::from_secs(60);
/// How long [`WsStream::close`] waits for the close frame to go out.
pub const CLOSE_WAIT: Duration = Duration::from_secs(2);
/// The read buffer each session allocates.
const READ_BUFFER: usize = 16 * 1024;

/// A session's limits, held to the caps by [`WsLimits::clamped`] (which
/// every connector applies).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WsLimits {
    /// The largest message either way: 1 byte to [`MAX_MESSAGE`].
    pub max_message: usize,
    /// How often a ping goes out: [`MIN_PING`] to [`PING_EVERY`].
    pub ping_every: Duration,
    /// How long the session may receive nothing: twice the ping interval
    /// to [`MAX_IDLE`].
    pub idle: Duration,
    /// The whole connection, upgrade included: [`MIN_CONNECT`] to
    /// [`MAX_CONNECT`].
    pub connect: Duration,
}

impl Default for WsLimits {
    fn default() -> Self {
        WsLimits {
            max_message: MAX_MESSAGE,
            ping_every: PING_EVERY,
            idle: IDLE_TIMEOUT,
            connect: CONNECT_TIMEOUT,
        }
    }
}

impl WsLimits {
    /// These limits held to the caps.
    pub fn clamped(self) -> WsLimits {
        let ping_every = self.ping_every.clamp(MIN_PING, PING_EVERY);
        WsLimits {
            max_message: self.max_message.clamp(1, MAX_MESSAGE),
            ping_every,
            idle: self.idle.clamp(ping_every * 2, MAX_IDLE),
            connect: self.connect.clamp(MIN_CONNECT, MAX_CONNECT),
        }
    }

    fn config(&self) -> WebSocketConfig {
        WebSocketConfig::default()
            .read_buffer_size(READ_BUFFER)
            .write_buffer_size(0)
            .max_write_buffer_size(2 * MAX_MESSAGE)
            .max_message_size(Some(self.max_message))
            .max_frame_size(Some(self.max_message))
    }
}

/// One WebSocket message. Control frames (ping, pong, close) are the
/// session's own business and never reach the plugin.
#[derive(Clone, PartialEq, Eq)]
pub enum WsMessage {
    Text(String),
    Binary(Vec<u8>),
}

impl WsMessage {
    /// The payload's length in bytes.
    pub fn len(&self) -> usize {
        match self {
            WsMessage::Text(t) => t.len(),
            WsMessage::Binary(b) => b.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// By length only: a payload can carry anything a server sent.
impl fmt::Debug for WsMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WsMessage::Text(t) => write!(f, "Text({} bytes)", t.len()),
            WsMessage::Binary(b) => write!(f, "Binary({} bytes)", b.len()),
        }
    }
}

/// Why a WebSocket session failed or couldn't open. No variant carries a
/// URL, an address, a certificate's names or a payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsError {
    /// Nothing was opened: the URL, the endpoint policy, the kill switch,
    /// the source's state, the connection or TLS, as the egress reports
    /// them for HTTP. [`HttpError::EgressDown`] and [`HttpError::NotReady`]
    /// keep their meaning: the server isn't at fault.
    Connect(HttpError),
    /// The server answered, but didn't accept the upgrade: its HTTP status
    /// when it gave one, `None` for an answer that broke the protocol (a
    /// wrong accept key, an extension or subprotocol nobody asked for, an
    /// oversized head).
    Handshake(Option<u16>),
    /// The plugin's route went down while the session was open, so the
    /// kill switch ended it. Not the server's fault.
    EgressDown(String),
    /// A message over the session's cap, either way. One the plugin tried
    /// to send was refused before anything was sent, and the session goes
    /// on; one the server sent ended the session.
    TooLarge,
    /// Nothing arrived within the idle timeout, or a send couldn't be
    /// written within it.
    Idle,
    /// The server broke the WebSocket protocol (a bad frame, text that
    /// isn't UTF-8, an unmasked or reserved bit).
    Protocol,
    /// The connection failed while open.
    Transport(String),
    /// The session already ended (closed by either side, or failed).
    Closed,
    /// A transcript replayer had nothing recorded for this.
    Unrecorded(String),
}

impl fmt::Display for WsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WsError::Connect(e) => write!(f, "couldn't connect: {e}"),
            WsError::Handshake(Some(status)) => {
                write!(
                    f,
                    "the server refused the WebSocket upgrade (HTTP {status})"
                )
            }
            WsError::Handshake(None) => f.write_str("the server's WebSocket upgrade was invalid"),
            WsError::EgressDown(e) => write!(f, "egress down: {e}"),
            WsError::TooLarge => f.write_str("a message was over the size cap"),
            WsError::Idle => f.write_str("the connection went quiet past the idle timeout"),
            WsError::Protocol => f.write_str("the server broke the WebSocket protocol"),
            WsError::Transport(e) => write!(f, "transport: {e}"),
            WsError::Closed => f.write_str("the WebSocket session already ended"),
            WsError::Unrecorded(e) => write!(f, "no recorded transcript: {e}"),
        }
    }
}

impl std::error::Error for WsError {}

impl WsError {
    /// Whether the plugin's route, not the server, is at fault: the kill
    /// switch refused or ended it, or a companion wasn't ready. Its egress
    /// paces such refusals itself, so they needn't grow the server's
    /// back-off.
    pub fn route_fault(&self) -> bool {
        matches!(
            self,
            WsError::EgressDown(_)
                | WsError::Connect(HttpError::EgressDown(_) | HttpError::NotReady(_))
        )
    }
}

/// Opens WebSocket sessions. [`PluginEgress`](crate::PluginEgress) is the
/// real one; tests use [`crate::testing::WsReplayer`].
pub trait WsConnect: Send + Sync {
    /// Opens a session to `url` (`wss://` only) with `limits`, held to the
    /// caps.
    fn connect<'a>(
        &'a self,
        url: &'a str,
        limits: WsLimits,
    ) -> BoxFuture<'a, Result<Box<dyn WsStream>, WsError>>;
}

/// An open session, used by one task at a time.
///
/// `recv` is cancel-safe: a plugin may wait on it in a `select!` with its
/// shutdown signal or a timer, drop it, [`send`](WsStream::send) and wait
/// again without losing a message. Pings and the idle timeout only run
/// while it's waited on, so a plugin keeps a session alive by waiting on
/// `recv`.
pub trait WsStream: Send {
    /// Sends one message. A message over the cap is refused with
    /// [`WsError::TooLarge`] and the session goes on; any other error ends
    /// the session.
    fn send(&mut self, msg: WsMessage) -> BoxFuture<'_, Result<(), WsError>>;
    /// The next message; `Ok(None)` once the server closed the session.
    /// Any error ends the session; later calls give [`WsError::Closed`].
    fn recv(&mut self) -> BoxFuture<'_, Result<Option<WsMessage>, WsError>>;
    /// Closes the session (a normal close, best effort within
    /// [`CLOSE_WAIT`]). Later sends give [`WsError::Closed`] and receives
    /// `Ok(None)`.
    fn close(&mut self) -> BoxFuture<'_, ()>;
}

/// No WebSocket for this plugin: every connect is refused with
/// [`HttpError::NoEgress`].
#[derive(Debug, Clone, Copy, Default)]
pub struct NoWs;

impl WsConnect for NoWs {
    fn connect<'a>(
        &'a self,
        _: &'a str,
        _: WsLimits,
    ) -> BoxFuture<'a, Result<Box<dyn WsStream>, WsError>> {
        Box::pin(async { Err(WsError::Connect(HttpError::NoEgress)) })
    }
}

/// Which certificates a session's TLS trusts.
#[derive(Clone, Default)]
pub enum Roots {
    /// The operating system's (rustls-platform-verifier), as the plugin
    /// HTTP client.
    #[default]
    Platform,
    /// Only these (the tests' own CA).
    Only(Vec<rustls::pki_types::CertificateDer<'static>>),
}

impl fmt::Debug for Roots {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Roots::Platform => f.write_str("Platform"),
            Roots::Only(c) => write!(f, "Only({} certificates)", c.len()),
        }
    }
}

/// The TLS connector for `roots`: rustls on ring, TLS 1.2 and 1.3, no
/// client certificate, no ALPN.
pub(crate) fn tls_connector(roots: &Roots) -> Result<tokio_rustls::TlsConnector, WsError> {
    let unusable = || WsError::Connect(HttpError::Transport("TLS couldn't be set up".into()));
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|_| unusable())?;
    let config = match roots {
        Roots::Only(certs) => {
            let mut store = rustls::RootCertStore::empty();
            for c in certs {
                store.add(c.clone()).map_err(|_| unusable())?;
            }
            if store.is_empty() {
                return Err(unusable());
            }
            builder.with_root_certificates(store).with_no_client_auth()
        }
        Roots::Platform => builder
            .dangerous()
            .with_custom_certificate_verifier(std::sync::Arc::new(
                rustls_platform_verifier::Verifier::new(provider).map_err(|_| unusable())?,
            ))
            .with_no_client_auth(),
    };
    Ok(tokio_rustls::TlsConnector::from(std::sync::Arc::new(
        config,
    )))
}

/// What a failed TLS handshake means, in words that carry nothing the
/// server chose (a certificate's names, an alert's text).
pub(crate) fn tls_failed(e: &std::io::Error) -> HttpError {
    let rustls = e.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>());
    HttpError::Transport(
        match rustls {
            Some(rustls::Error::InvalidCertificate(_)) => {
                "TLS: the server's certificate wasn't accepted"
            }
            Some(_) => "TLS: the handshake failed",
            None => "TLS: the connection failed during the handshake",
        }
        .into(),
    )
}

/// A `wss://` URL, checked for form: no other scheme, no credentials, no
/// fragment, a host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WsTarget {
    /// The URL as sent in the upgrade request.
    pub url: url::Url,
    /// The same origin over `https`, for the endpoint policy's URL rules
    /// and host key.
    pub https: url::Url,
    /// The host to resolve and to name in TLS: a DNS name, or an IP
    /// literal without brackets.
    pub host: String,
    pub port: u16,
}

/// Why a URL can't be a WebSocket target. The text names the rule, never
/// the URL.
pub(crate) fn parse_url(raw: &str) -> Result<WsTarget, &'static str> {
    let url = url::Url::parse(raw).map_err(|_| "it doesn't parse")?;
    if url.scheme() != "wss" {
        return Err("only wss:// is supported for WebSocket connections");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("credentials in the URL aren't allowed");
    }
    if url.fragment().is_some() {
        return Err("a WebSocket URL can't have a fragment");
    }
    let host = match url.host() {
        Some(url::Host::Domain(d)) if !d.is_empty() => d.to_owned(),
        Some(url::Host::Ipv4(a)) => a.to_string(),
        Some(url::Host::Ipv6(a)) => a.to_string(),
        _ => return Err("it has no host"),
    };
    let port = url.port_or_known_default().ok_or("it has no port")?;
    if port == 0 {
        return Err("the port isn't valid");
    }
    let mut https = url.clone();
    https.set_scheme("https").map_err(|()| "it doesn't parse")?;
    https.set_query(None);
    https.set_path("/");
    Ok(WsTarget {
        url,
        https,
        host,
        port,
    })
}

/// Any byte stream a session can run over (a TLS stream, in practice).
pub(crate) trait Io: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}
pub(crate) type BoxIo = Box<dyn Io>;

/// The kill switch's hold on an open session.
pub(crate) trait RouteWatch: Send {
    /// Resolves once the route is down (or can no longer be watched), with
    /// why, in words that name no address.
    fn down(&mut self) -> BoxFuture<'_, String>;
}

/// A route that never goes down (a test over a plain stream).
#[cfg(test)]
pub(crate) struct NeverDown;

#[cfg(test)]
impl RouteWatch for NeverDown {
    fn down(&mut self) -> BoxFuture<'_, String> {
        Box::pin(std::future::pending())
    }
}

/// The WebSocket upgrade over `io`: the request names the target and
/// Lodestar's user agent, offers no extension or subprotocol, and the
/// answer must name neither.
pub(crate) async fn handshake(
    io: BoxIo,
    target: &WsTarget,
    limits: WsLimits,
) -> Result<WebSocketStream<BoxIo>, WsError> {
    let mut request = target
        .url
        .as_str()
        .into_client_request()
        .map_err(|_| WsError::Connect(HttpError::BadUrl))?;
    request.headers_mut().insert(
        "user-agent",
        HeaderValue::from_static(lodestar_integrations::USER_AGENT),
    );
    let (ws, response) =
        tokio_tungstenite::client_async_with_config(request, io, Some(limits.config()))
            .await
            .map_err(|e| match e {
                tungstenite::Error::Http(r) => WsError::Handshake(Some(r.status().as_u16())),
                tungstenite::Error::Io(e) => WsError::Connect(HttpError::Transport(format!(
                    "the connection failed during the upgrade: {}",
                    e.kind()
                ))),
                tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed => {
                    WsError::Connect(HttpError::Transport(
                        "the connection closed during the upgrade".into(),
                    ))
                }
                _ => WsError::Handshake(None),
            })?;
    if response.headers().contains_key("sec-websocket-extensions") {
        // RFC 6455 §4.1: an extension the client didn't offer fails the
        // connection (tungstenite leaves this check to its caller).
        return Err(WsError::Handshake(None));
    }
    Ok(ws)
}

/// An open session over a real socket.
pub(crate) struct Socket {
    /// Dropped the moment the session ends, so the socket closes at once.
    ws: Option<WebSocketStream<BoxIo>>,
    watch: Box<dyn RouteWatch>,
    limits: WsLimits,
    next_ping: Instant,
    idle_by: Instant,
    /// Set once the session has ended: `None` for a close, the error
    /// otherwise.
    ended: Option<Option<WsError>>,
}

/// What a wait in [`Socket::recv`] ended on.
enum Wake {
    Down(String),
    Idle,
    Ping,
    Read(Option<Result<Message, tungstenite::Error>>),
}

impl Socket {
    pub(crate) fn new(
        ws: WebSocketStream<BoxIo>,
        limits: WsLimits,
        watch: Box<dyn RouteWatch>,
    ) -> Socket {
        let now = Instant::now();
        Socket {
            ws: Some(ws),
            watch,
            limits,
            next_ping: now + limits.ping_every,
            idle_by: now + limits.idle,
            ended: None,
        }
    }

    /// Ends the session: the socket is dropped (closed) and every later
    /// call gives the end.
    fn end(&mut self, error: Option<WsError>) {
        self.ws = None;
        self.ended = Some(error);
    }

    async fn send_msg(&mut self, msg: WsMessage) -> Result<(), WsError> {
        if self.ended.is_some() {
            return Err(WsError::Closed);
        }
        if msg.len() > self.limits.max_message {
            return Err(WsError::TooLarge);
        }
        let msg = match msg {
            WsMessage::Text(t) => Message::text(t),
            WsMessage::Binary(b) => Message::binary(b),
        };
        match self.write(msg).await {
            Ok(()) => Ok(()),
            Err(e) => {
                self.end(Some(e.clone()));
                Err(e)
            }
        }
    }

    /// Writes one frame and flushes it, under the kill switch, within the
    /// idle timeout.
    async fn write(&mut self, msg: Message) -> Result<(), WsError> {
        let Socket {
            ws, watch, limits, ..
        } = self;
        let Some(ws) = ws.as_mut() else {
            return Err(WsError::Closed);
        };
        let by = Instant::now() + limits.idle;
        tokio::select! {
            biased;
            reason = watch.down() => Err(WsError::EgressDown(reason)),
            () = tokio::time::sleep_until(by) => Err(WsError::Idle),
            r = ws.send(msg) => r.map_err(|e| read_error(&e)),
        }
    }

    async fn recv_msg(&mut self) -> Result<Option<WsMessage>, WsError> {
        loop {
            match &self.ended {
                Some(None) => return Ok(None),
                Some(Some(_)) => return Err(WsError::Closed),
                None => {}
            }
            let wake = {
                let Socket {
                    ws,
                    watch,
                    next_ping,
                    idle_by,
                    ..
                } = self;
                let Some(ws) = ws.as_mut() else {
                    return Err(WsError::Closed);
                };
                tokio::select! {
                    biased;
                    reason = watch.down() => Wake::Down(reason),
                    () = tokio::time::sleep_until(*idle_by) => Wake::Idle,
                    () = tokio::time::sleep_until(*next_ping) => Wake::Ping,
                    m = ws.next() => Wake::Read(m),
                }
            };
            match wake {
                Wake::Down(reason) => {
                    let e = WsError::EgressDown(reason);
                    self.end(Some(e.clone()));
                    return Err(e);
                }
                Wake::Idle => {
                    self.end(Some(WsError::Idle));
                    return Err(WsError::Idle);
                }
                Wake::Ping => {
                    self.next_ping = Instant::now() + self.limits.ping_every;
                    if let Err(e) = self.write(Message::Ping(Default::default())).await {
                        self.end(Some(e.clone()));
                        return Err(e);
                    }
                }
                Wake::Read(read) => {
                    let msg = match read {
                        // The socket ended without a close frame.
                        None => {
                            let e = WsError::Transport("the connection closed".into());
                            self.end(Some(e.clone()));
                            return Err(e);
                        }
                        Some(Err(e)) => match e {
                            tungstenite::Error::ConnectionClosed
                            | tungstenite::Error::AlreadyClosed => {
                                self.end(None);
                                return Ok(None);
                            }
                            e => {
                                let e = read_error(&e);
                                self.end(Some(e.clone()));
                                return Err(e);
                            }
                        },
                        Some(Ok(m)) => m,
                    };
                    self.idle_by = Instant::now() + self.limits.idle;
                    match msg {
                        Message::Text(t) => {
                            return Ok(Some(WsMessage::Text(t.as_str().to_owned())));
                        }
                        Message::Binary(b) => return Ok(Some(WsMessage::Binary(b.to_vec()))),
                        // tungstenite queues the pong; send it now rather
                        // than with the next frame.
                        Message::Ping(_) => {
                            if let Err(e) = self.flush().await {
                                self.end(Some(e.clone()));
                                return Err(e);
                            }
                        }
                        Message::Pong(_) | Message::Frame(_) => {}
                        Message::Close(_) => {
                            // tungstenite answers the close; give it a
                            // moment to go out, then drop the socket.
                            let _ = tokio::time::timeout(CLOSE_WAIT, self.flush()).await;
                            self.end(None);
                            return Ok(None);
                        }
                    }
                }
            }
        }
    }

    async fn flush(&mut self) -> Result<(), WsError> {
        let Socket {
            ws, watch, limits, ..
        } = self;
        let Some(ws) = ws.as_mut() else {
            return Err(WsError::Closed);
        };
        let by = Instant::now() + limits.idle;
        tokio::select! {
            biased;
            reason = watch.down() => Err(WsError::EgressDown(reason)),
            () = tokio::time::sleep_until(by) => Err(WsError::Idle),
            r = ws.flush() => r.map_err(|e| read_error(&e)),
        }
    }

    async fn close_session(&mut self) {
        if self.ended.is_some() {
            return;
        }
        if let Some(ws) = self.ws.as_mut() {
            let frame = tungstenite::protocol::CloseFrame {
                code: CloseCode::Normal,
                reason: Default::default(),
            };
            let _ = tokio::time::timeout(CLOSE_WAIT, ws.close(Some(frame))).await;
        }
        self.end(None);
    }
}

/// What a tungstenite error on an open session means.
fn read_error(e: &tungstenite::Error) -> WsError {
    use tungstenite::Error as E;
    match e {
        E::Capacity(_) | E::WriteBufferFull(_) => WsError::TooLarge,
        E::Protocol(_) | E::Utf8(_) | E::AttackAttempt => WsError::Protocol,
        E::Io(e) => WsError::Transport(format!("connection error: {}", e.kind())),
        E::Tls(_) => WsError::Transport("TLS error".into()),
        E::ConnectionClosed | E::AlreadyClosed => WsError::Closed,
        _ => WsError::Protocol,
    }
}

impl WsStream for Socket {
    fn send(&mut self, msg: WsMessage) -> BoxFuture<'_, Result<(), WsError>> {
        Box::pin(self.send_msg(msg))
    }

    fn recv(&mut self) -> BoxFuture<'_, Result<Option<WsMessage>, WsError>> {
        Box::pin(self.recv_msg())
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(self.close_session())
    }
}

/// Reconnect back-off for a plugin's WebSocket loop (§18.7 politeness):
/// the wait after the `n`th failure in a row is in `[full, 1.5 × full]`,
/// where `full = min(max, base × 2ⁿ⁻¹)`, from the plugin's back-off with
/// the floor applied ([`floors::BACKOFF_BASE`]). It's never below the base
/// nor above the max. The count resets only after a session that stayed
/// up for [`Reconnect::STABLE_AFTER`], so a server that accepts and then
/// drops the connection keeps the back-off growing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconnect {
    backoff: Backoff,
    failures: u32,
}

impl Reconnect {
    /// How long a session must stay up for the count to reset.
    pub const STABLE_AFTER: Duration = Duration::from_secs(300);

    /// From the plugin's effective back-off (`Policy::limits().backoff`).
    pub fn new(backoff: Backoff) -> Reconnect {
        let base = backoff.base.max(floors::BACKOFF_BASE);
        Reconnect {
            backoff: Backoff {
                base,
                max: backoff.max.max(base),
            },
            failures: 0,
        }
    }

    /// Failures in a row so far.
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// The wait before the next attempt after a connect failed. `entropy`
    /// picks the point in the range (any value keeps the bounds).
    pub fn failed(&mut self, entropy: u64) -> Duration {
        self.failures = self.failures.saturating_add(1);
        backoff_delay(self.backoff, self.failures, entropy)
    }

    /// The wait before the next attempt after a session that was open for
    /// `up_for` ended, however it ended.
    pub fn ended(&mut self, up_for: Duration, entropy: u64) -> Duration {
        if up_for >= Reconnect::STABLE_AFTER {
            self.failures = 0;
        }
        self.failed(entropy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
    use tokio::sync::watch;

    #[test]
    fn urls_are_wss_only_with_a_host_and_no_credentials() {
        let t = parse_url("wss://Relay.Example/path?x=1").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("relay.example", 443));
        assert_eq!(t.https.as_str(), "https://relay.example/");
        let t = parse_url("wss://[::1]:7447/").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("::1", 7447));
        assert_eq!(t.https.as_str(), "https://[::1]:7447/");
        assert_eq!(parse_url("wss://10.0.0.1/").unwrap().host, "10.0.0.1");
        for bad in [
            "ws://relay.example/",
            "https://relay.example/",
            "wss://user:secret@relay.example/",
            "wss://user@relay.example/",
            "wss://relay.example/#frag",
            "wss:///",
            "wss://relay.example:0/",
            "relay.example",
            "",
        ] {
            let why = parse_url(bad).unwrap_err();
            assert!(
                !why.contains("relay") && !why.contains("secret"),
                "{bad}: {why}"
            );
        }
    }

    #[test]
    fn limits_are_held_to_the_caps() {
        let l = WsLimits {
            max_message: usize::MAX,
            ping_every: Duration::from_secs(3600),
            idle: Duration::ZERO,
            connect: Duration::from_secs(3600),
        }
        .clamped();
        assert_eq!(l.max_message, MAX_MESSAGE);
        assert_eq!(l.ping_every, PING_EVERY);
        assert_eq!(l.idle, PING_EVERY * 2);
        assert_eq!(l.connect, MAX_CONNECT);
        let l = WsLimits {
            max_message: 0,
            ping_every: Duration::ZERO,
            idle: Duration::from_secs(86_400),
            connect: Duration::ZERO,
        }
        .clamped();
        assert_eq!(
            (l.max_message, l.ping_every, l.idle, l.connect),
            (1, MIN_PING, MAX_IDLE, MIN_CONNECT)
        );
        assert_eq!(WsLimits::default().clamped(), WsLimits::default());
    }

    #[test]
    fn errors_and_messages_print_no_payload() {
        let m = WsMessage::Text("secret".into());
        assert_eq!(format!("{m:?}"), "Text(6 bytes)");
        assert!(!format!("{:?}", WsMessage::Binary(b"secret".to_vec())).contains("secret"));
        assert!(WsError::Connect(HttpError::EgressDown("x".into())).route_fault());
        assert!(WsError::EgressDown("x".into()).route_fault());
        assert!(!WsError::Handshake(Some(403)).route_fault());
        assert!(!WsError::Idle.route_fault());
    }

    #[test]
    fn reconnect_backs_off_from_the_floor_and_resets_only_after_a_stable_session() {
        let mut r = Reconnect::new(Backoff {
            base: Duration::ZERO,
            max: Duration::ZERO,
        });
        // The floor applies to a plugin that asked for nothing.
        assert_eq!(r.failed(0), floors::BACKOFF_BASE);
        let mut r = Reconnect::new(Backoff {
            base: Duration::from_secs(30),
            max: Duration::from_secs(3600),
        });
        let waits: Vec<_> = (0..10).map(|_| r.failed(0)).collect();
        assert_eq!(waits[0], Duration::from_secs(30));
        assert_eq!(waits[1], Duration::from_secs(60));
        assert_eq!(waits[9], Duration::from_secs(3600));
        // A session that drops at once keeps the back-off growing.
        assert_eq!(
            r.ended(Duration::from_secs(5), 0),
            Duration::from_secs(3600)
        );
        assert_eq!(r.failures(), 11);
        // One that stayed up starts again from the base.
        assert_eq!(r.ended(Reconnect::STABLE_AFTER, 0), Duration::from_secs(30));
        assert_eq!(r.failures(), 1);
    }

    proptest! {
        #[test]
        fn reconnect_waits_stay_within_base_and_max(
            base in 0u64..100_000,
            max in 0u64..10_000_000,
            failures in 1u32..80,
            entropy in any::<u64>(),
        ) {
            let mut r = Reconnect::new(Backoff {
                base: Duration::from_millis(base),
                max: Duration::from_millis(max),
            });
            let floor = Duration::from_millis(base).max(floors::BACKOFF_BASE);
            let ceiling = Duration::from_millis(max).max(floor);
            for _ in 0..failures {
                let w = r.failed(entropy);
                prop_assert!(w >= floor && w <= ceiling, "{w:?} not in {floor:?}..={ceiling:?}");
            }
        }

        #[test]
        fn parse_url_never_panics_and_accepts_only_wss(raw in ".{0,80}") {
            if let Ok(t) = parse_url(&raw) {
                prop_assert_eq!(t.url.scheme(), "wss");
                prop_assert_eq!(t.https.scheme(), "https");
                prop_assert!(t.url.username().is_empty() && t.url.password().is_none());
                prop_assert!(t.port != 0 && !t.host.is_empty());
            }
        }
    }

    fn target() -> WsTarget {
        parse_url("wss://relay.test/").unwrap()
    }

    fn server_config() -> WebSocketConfig {
        WebSocketConfig::default()
            .max_message_size(None)
            .max_frame_size(None)
    }

    /// A client session and the server's end, over memory.
    async fn pair(watch: Box<dyn RouteWatch>) -> (Socket, WebSocketStream<DuplexStream>) {
        let (client, server) = tokio::io::duplex(1 << 20);
        let limits = WsLimits::default();
        let target = target();
        let (client, server) = tokio::join!(
            handshake(Box::new(client), &target, limits),
            tokio_tungstenite::accept_async_with_config(server, Some(server_config())),
        );
        (Socket::new(client.unwrap(), limits, watch), server.unwrap())
    }

    /// A route a test takes down.
    struct Flag(watch::Receiver<bool>);

    impl RouteWatch for Flag {
        fn down(&mut self) -> BoxFuture<'_, String> {
            Box::pin(async move {
                let _ = self.0.wait_for(|down| *down).await;
                "the test took the route down".into()
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn messages_go_both_ways_and_a_close_ends_the_session() {
        let (mut client, mut server) = pair(Box::new(NeverDown)).await;
        client.send(WsMessage::Text("hello".into())).await.unwrap();
        client.send(WsMessage::Binary(vec![1, 2, 3])).await.unwrap();
        assert_eq!(
            server.next().await.unwrap().unwrap(),
            Message::text("hello")
        );
        assert_eq!(
            server.next().await.unwrap().unwrap(),
            Message::binary(vec![1, 2, 3])
        );
        server.send(Message::text("back")).await.unwrap();
        server.send(Message::binary(vec![9])).await.unwrap();
        assert_eq!(
            client.recv().await,
            Ok(Some(WsMessage::Text("back".into())))
        );
        assert_eq!(client.recv().await, Ok(Some(WsMessage::Binary(vec![9]))));
        server.close(None).await.unwrap();
        assert_eq!(client.recv().await, Ok(None));
        assert_eq!(client.recv().await, Ok(None));
        assert_eq!(
            client.send(WsMessage::Text("late".into())).await,
            Err(WsError::Closed)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_oversized_message_is_refused_both_ways() {
        let (mut client, mut server) = pair(Box::new(NeverDown)).await;
        // Refused before sending; the session goes on.
        assert_eq!(
            client
                .send(WsMessage::Binary(vec![0; MAX_MESSAGE + 1]))
                .await,
            Err(WsError::TooLarge)
        );
        client
            .send(WsMessage::Binary(vec![0; MAX_MESSAGE]))
            .await
            .unwrap();
        assert_eq!(server.next().await.unwrap().unwrap().len(), MAX_MESSAGE);
        // One the server sends ends the session.
        server
            .send(Message::binary(vec![0; MAX_MESSAGE + 1]))
            .await
            .unwrap();
        assert_eq!(client.recv().await, Err(WsError::TooLarge));
        assert_eq!(client.recv().await, Err(WsError::Closed));
        // Text, and a message split over frames, are capped the same.
        let (mut client, mut server) = pair(Box::new(NeverDown)).await;
        server
            .send(Message::text("x".repeat(MAX_MESSAGE + 1)))
            .await
            .unwrap();
        assert_eq!(client.recv().await, Err(WsError::TooLarge));
    }

    #[tokio::test(start_paused = true)]
    async fn pings_go_out_and_a_quiet_server_times_out() {
        let (mut client, mut server) = pair(Box::new(NeverDown)).await;
        let start = Instant::now();
        // The server reads (and so answers pings) on its own task.
        let pings = tokio::spawn(async move {
            let mut at = Vec::new();
            while let Some(Ok(m)) = server.next().await {
                if m.is_ping() {
                    at.push(Instant::now());
                }
                if at.len() == 2 {
                    // Send the queued pong, then stop reading.
                    server.flush().await.unwrap();
                    break;
                }
            }
            (at, server)
        });
        // Pongs keep the session alive past the idle timeout.
        let r = tokio::time::timeout(IDLE_TIMEOUT + PING_EVERY, client.recv()).await;
        assert!(r.is_err(), "the session ended: {r:?}");
        let (at, server) = pings.await.unwrap();
        assert_eq!(at[0] - start, PING_EVERY);
        assert_eq!(at[1] - start, PING_EVERY * 2);
        // A server that stops reading answers no ping: the session ends
        // at the idle timeout after the last frame it sent.
        let quiet = Instant::now();
        assert_eq!(client.recv().await, Err(WsError::Idle));
        assert!(
            Instant::now() - quiet <= IDLE_TIMEOUT,
            "{:?}",
            Instant::now() - quiet
        );
        assert_eq!(client.recv().await, Err(WsError::Closed));
        drop(server);
    }

    #[tokio::test(start_paused = true)]
    async fn the_kill_switch_ends_an_open_session() {
        let (tx, rx) = watch::channel(false);
        let (mut client, _server) = pair(Box::new(Flag(rx))).await;
        let down = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            tx.send(true).unwrap();
            tx
        });
        assert_eq!(
            client.recv().await,
            Err(WsError::EgressDown("the test took the route down".into()))
        );
        let _tx = down.await.unwrap();
        assert_eq!(
            client.send(WsMessage::Text("after".into())).await,
            Err(WsError::Closed)
        );
    }

    /// The server's end, answering the upgrade with `answer` (the
    /// request's key fills `{accept}`).
    async fn raw_answer(answer: &'static str) -> Result<WebSocketStream<BoxIo>, WsError> {
        let (client, mut server) = tokio::io::duplex(1 << 16);
        let serve = async move {
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                server.read_exact(&mut byte).await.unwrap();
                head.push(byte[0]);
            }
            let head = String::from_utf8(head).unwrap();
            assert!(
                head.to_ascii_lowercase().contains("user-agent: lodestar"),
                "{head}"
            );
            assert!(
                !head
                    .to_ascii_lowercase()
                    .contains("sec-websocket-extensions")
            );
            let key = head
                .lines()
                .find_map(|l| l.strip_prefix("Sec-WebSocket-Key: "))
                .or_else(|| {
                    head.lines()
                        .find_map(|l| l.strip_prefix("sec-websocket-key: "))
                })
                .unwrap()
                .trim()
                .to_owned();
            let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
            server
                .write_all(answer.replace("{accept}", &accept).as_bytes())
                .await
                .unwrap();
            server
        };
        let target = target();
        let (r, _server) = tokio::join!(
            handshake(Box::new(client), &target, WsLimits::default()),
            serve
        );
        r
    }

    #[tokio::test]
    async fn an_upgrade_must_be_a_plain_101() {
        let ok = "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n";
        assert!(raw_answer(ok).await.is_ok());
        let refused = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n";
        assert_eq!(
            raw_answer(refused).await.err(),
            Some(WsError::Handshake(Some(403)))
        );
        let deflate = "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\nSec-WebSocket-Extensions: permessage-deflate\r\n\r\n";
        assert_eq!(
            raw_answer(deflate).await.err(),
            Some(WsError::Handshake(None))
        );
        let protocol = "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\nSec-WebSocket-Protocol: chat\r\n\r\n";
        assert_eq!(
            raw_answer(protocol).await.err(),
            Some(WsError::Handshake(None))
        );
        let wrong_key = "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: AAAAAAAAAAAAAAAAAAAAAAAAAAA=\r\n\r\n";
        assert_eq!(
            raw_answer(wrong_key).await.err(),
            Some(WsError::Handshake(None))
        );
    }

    #[test]
    fn tls_failures_carry_no_server_text() {
        let cert = std::io::Error::other(rustls::Error::InvalidCertificate(
            rustls::CertificateError::NotValidForName,
        ));
        let e = tls_failed(&cert).to_string();
        assert!(e.contains("certificate wasn't accepted"), "{e}");
        let other = std::io::Error::other(rustls::Error::General("evil.example said no".into()));
        assert!(!tls_failed(&other).to_string().contains("evil"));
        assert!(matches!(
            tls_connector(&Roots::Only(Vec::new())),
            Err(WsError::Connect(_))
        ));
        assert!(tls_connector(&Roots::Platform).is_ok());
    }
}
