//! The typed wrappers (§12.1): the only way a hash, a name, a path, an
//! address, a URL, a query or an error reaches a line. Each one becomes a
//! [`Value`] marked with its kind, and each output renders it by that
//! output's switch for the kind: off, a torrent shows as `ref:xxxxxxxx` and
//! anything else as `[hidden]`.

use std::borrow::Cow;
use std::fmt;
use std::net::{IpAddr, SocketAddr};

use crate::event::{FieldKind, Kind, Mask, torrent_kind};
use crate::url;

/// Makes a torrent's `ref:xxxxxxxx` (the runtime's keyed hash).
pub type Reference<'r> = &'r dyn Fn(&[u8; 20]) -> String;

/// What a value that's off shows as.
pub const HIDDEN: &str = "[hidden]";

/// The kind markers a [`crate::catalogue!`] declaration names for each
/// field (`fields: { torrent: Hash, error: Error }`). A field accepts only
/// the wrappers that carry its kind.
pub mod kind {
    use crate::event::{FieldKind, Kind};

    /// A field kind a declaration can name.
    pub trait Marker {
        const KIND: FieldKind;
    }

    macro_rules! markers {
        ($($name:ident => $kind:expr),* $(,)?) => {$(
            #[derive(Debug)]
            pub enum $name {}
            impl Marker for $name {
                const KIND: FieldKind = $kind;
            }
        )*};
    }

    markers! {
        Hash => FieldKind::Data(Kind::Hash),
        Name => FieldKind::Data(Kind::Name),
        Path => FieldKind::Data(Kind::Path),
        Url => FieldKind::Data(Kind::Url),
        Query => FieldKind::Data(Kind::Query),
        Client => FieldKind::Data(Kind::Client),
        Peer => FieldKind::Data(Kind::Peer),
        Size => FieldKind::Data(Kind::Size),
        FilterChange => FieldKind::Data(Kind::FilterChange),
        Error => FieldKind::Error,
        Label => FieldKind::Label,
        SetupToken => FieldKind::Label,
    }
}

/// A wrapper that a field of kind `K` accepts.
pub trait IntoValue<'a, K> {
    fn into_value(self) -> Value<'a>;
}

/// One field's value, raw, as the record carries it to the outputs.
#[derive(Debug, Clone, Copy)]
pub enum Value<'a> {
    Torrent(&'a [u8; 20]),
    Name(&'a str),
    Path(&'a std::path::Path),
    Url(&'a str, url::Form),
    Query(&'a str),
    Client(IpAddr),
    Peer(SocketAddr),
    Size(u64),
    Signed(i64),
    Detail(&'a str),
    Error(Error<'a>),
    Label(&'static str),
    Flag(bool),
    Count(u64),
    Location(&'a str, u32),
    Listen(SocketAddr),
    /// The setup token, shown until setup completes.
    Secret(&'a str),
}

/// A value as one output shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rendered {
    /// Off for this output: `[hidden]`.
    Hidden,
    /// A torrent with hashes off: `ref:xxxxxxxx`.
    Reference(String),
    Text(String),
    Number(u64),
    Signed(i64),
    Flag(bool),
}

impl fmt::Display for Rendered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Rendered::Hidden => f.write_str(HIDDEN),
            Rendered::Reference(s) | Rendered::Text(s) => f.write_str(s),
            Rendered::Number(n) => write!(f, "{n}"),
            Rendered::Signed(n) => write!(f, "{n}"),
            Rendered::Flag(b) => write!(f, "{b}"),
        }
    }
}

impl Value<'_> {
    pub fn kind(&self) -> FieldKind {
        match self {
            Value::Torrent(_) => FieldKind::Data(Kind::Hash),
            Value::Name(_) => FieldKind::Data(Kind::Name),
            Value::Path(_) => FieldKind::Data(Kind::Path),
            Value::Url(..) => FieldKind::Data(Kind::Url),
            Value::Query(_) => FieldKind::Data(Kind::Query),
            Value::Client(_) => FieldKind::Data(Kind::Client),
            Value::Peer(_) => FieldKind::Data(Kind::Peer),
            Value::Size(_) | Value::Signed(_) => FieldKind::Data(Kind::Size),
            Value::Detail(_) => FieldKind::Data(Kind::FilterChange),
            Value::Error(_) => FieldKind::Error,
            Value::Label(_)
            | Value::Flag(_)
            | Value::Count(_)
            | Value::Location(..)
            | Value::Listen(_)
            | Value::Secret(_) => FieldKind::Label,
        }
    }

    /// The value as an output shows it under `mask`. `pairs_torrent`: the
    /// record also names a torrent, so a peer address shows only with peer
    /// activity on. `reference` makes a torrent's `ref:`; without one
    /// (no runtime: tests, tools) a hidden torrent is `[hidden]` too.
    pub fn render(
        &self,
        mask: Mask,
        pairs_torrent: bool,
        reference: Option<Reference<'_>>,
    ) -> Rendered {
        let text = |kind: Kind, s: Cow<'_, str>| match mask.has(kind) {
            true => Rendered::Text(s.into_owned()),
            false => Rendered::Hidden,
        };
        match *self {
            Value::Torrent(h) => match (mask.has(Kind::Hash), reference) {
                (true, _) => Rendered::Text(hex(h)),
                (false, Some(r)) => Rendered::Reference(r(h)),
                (false, None) => Rendered::Hidden,
            },
            Value::Name(s) => text(Kind::Name, s.into()),
            Value::Path(p) => text(Kind::Path, p.to_string_lossy()),
            Value::Url(s, form) => match mask.has(Kind::Url) {
                true => Rendered::Text(url::scrub(s, form)),
                false => Rendered::Hidden,
            },
            Value::Query(s) => text(Kind::Query, s.into()),
            Value::Client(ip) => text(Kind::Client, ip.to_string().into()),
            Value::Peer(addr) => {
                // Beside a torrent it's peer activity, the opt-in's alone;
                // otherwise a connection-level address, the peer kind's.
                let shown = match pairs_torrent {
                    true => mask.has(Kind::PeerActivity),
                    false => mask.has(Kind::Peer),
                };
                match shown {
                    true => Rendered::Text(addr.to_string()),
                    false => Rendered::Hidden,
                }
            }
            Value::Size(n) => match mask.has(Kind::Size) {
                true => Rendered::Number(n),
                false => Rendered::Hidden,
            },
            Value::Signed(n) => match mask.has(Kind::Size) {
                true => Rendered::Signed(n),
                false => Rendered::Hidden,
            },
            Value::Detail(s) => text(Kind::FilterChange, s.into()),
            Value::Error(e) => Rendered::Text(e.to_string()),
            Value::Label(s) => Rendered::Text(s.to_string()),
            Value::Flag(b) => Rendered::Flag(b),
            Value::Count(n) => Rendered::Number(n),
            Value::Location(file, line) => Rendered::Text(format!("{file}:{line}")),
            Value::Listen(addr) => Rendered::Text(addr.to_string()),
            Value::Secret(s) => Rendered::Text(s.to_string()),
        }
    }
}

/// Whether a record's fields name a torrent (hash, name or path), so a
/// peer address among them is peer activity.
pub fn pairs_torrent(fields: &[(&'static str, Value<'_>)]) -> bool {
    fields.iter().any(|(_, v)| match v.kind() {
        FieldKind::Data(k) => torrent_kind(k),
        _ => false,
    })
}

fn hex(b: &[u8]) -> String {
    use fmt::Write;
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        let _ = write!(s, "{x:02x}");
    }
    s
}

macro_rules! wrapper {
    ($(#[$m:meta])* $name:ident($inner:ty) => $marker:ident, $variant:ident) => {
        $(#[$m])*
        #[derive(Clone, Copy)]
        pub struct $name<'a>(pub $inner);

        impl<'a> IntoValue<'a, kind::$marker> for $name<'a> {
            fn into_value(self) -> Value<'a> {
                Value::$variant(self.0)
            }
        }

        // Never the value: a wrapper that reaches a `{:?}` must not leak.
        impl fmt::Debug for $name<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "(", "[hidden]", ")"))
            }
        }
    };
}

wrapper!(
    /// A torrent by its v1 info hash. Off, it's `ref:xxxxxxxx`: a keyed
    /// hash under a key made at each start, never written down.
    Torrent(&'a [u8; 20]) => Hash, Torrent
);
wrapper!(
    /// A torrent's name.
    Name(&'a str) => Name, Name
);
wrapper!(
    /// A search query.
    Query(&'a str) => Query, Query
);
wrapper!(
    /// Which filter rule, term, exception, pack or list changed, and how:
    /// only for `filters.change_detail`, the owner's opt-in (§2.11). Never
    /// the name of an item a change released (sealed, §20.11).
    Detail(&'a str) => FilterChange, Detail
);

/// A file name or path.
#[derive(Clone, Copy)]
pub struct Path<'a>(pub &'a std::path::Path);

impl<'a> Path<'a> {
    pub fn new(p: &'a (impl AsRef<std::path::Path> + ?Sized)) -> Path<'a> {
        Path(p.as_ref())
    }
}

impl<'a> IntoValue<'a, kind::Path> for Path<'a> {
    fn into_value(self) -> Value<'a> {
        Value::Path(self.0)
    }
}

impl fmt::Debug for Path<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Path([hidden])")
    }
}

/// A tracker, source, client or notifier URL. Credentials are always
/// removed, URLs on or off: userinfo, the values of `apikey`, `token`,
/// `passkey` and other credential-like query names, and passkey-like path
/// segments ([`crate::url`]).
#[derive(Clone, Copy)]
pub struct Url<'a>(pub &'a str, url::Form);

impl<'a> Url<'a> {
    pub fn new(url: &'a str) -> Url<'a> {
        Url(url, url::Form::Absolute)
    }

    /// A request target (`/api/v1/torznab?apikey=…`), as the access line
    /// has it.
    pub fn request(uri: &'a str) -> Url<'a> {
        Url(uri, url::Form::Request)
    }
}

impl<'a> IntoValue<'a, kind::Url> for Url<'a> {
    fn into_value(self) -> Value<'a> {
        Value::Url(self.0, self.1)
    }
}

impl fmt::Debug for Url<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Url([hidden])")
    }
}

/// A sign-in or API client's address.
#[derive(Debug, Clone, Copy)]
pub struct Client(pub IpAddr);

impl<'a> IntoValue<'a, kind::Client> for Client {
    fn into_value(self) -> Value<'a> {
        Value::Client(self.0)
    }
}

/// A peer's address. Next to a torrent in the same line, it shows only
/// with the peer activity opt-in on; on its own, with peer addresses on.
#[derive(Debug, Clone, Copy)]
pub struct Peer(pub SocketAddr);

impl<'a> IntoValue<'a, kind::Peer> for Peer {
    fn into_value(self) -> Value<'a> {
        Value::Peer(self.0)
    }
}

macro_rules! sizes {
    ($($t:ty),*) => {$(
        impl<'a> IntoValue<'a, kind::Size> for $t {
            fn into_value(self) -> Value<'a> {
                Value::Size(self as u64)
            }
        }
    )*};
}
sizes!(u8, u16, u32, u64, usize);

impl<'a> IntoValue<'a, kind::Size> for i64 {
    fn into_value(self) -> Value<'a> {
        Value::Signed(self)
    }
}

impl<'a> IntoValue<'a, kind::Label> for &'static str {
    fn into_value(self) -> Value<'a> {
        Value::Label(self)
    }
}

impl<'a> IntoValue<'a, kind::Label> for bool {
    fn into_value(self) -> Value<'a> {
        Value::Flag(self)
    }
}

/// A number the code itself chose (an attempt, a revision, a status
/// code), shown at every setting.
#[derive(Debug, Clone, Copy)]
pub struct Count(pub u64);

impl<'a> IntoValue<'a, kind::Label> for Count {
    fn into_value(self) -> Value<'a> {
        Value::Count(self.0)
    }
}

/// The first-run setup token (§12): the one secret a line may carry,
/// printed at start until setup completes, after which it stops working.
/// Only `auth.setup_token` declares it.
#[derive(Clone, Copy)]
pub struct SetupToken<'a>(pub &'a str);

impl<'a> IntoValue<'a, kind::SetupToken> for SetupToken<'a> {
    fn into_value(self) -> Value<'a> {
        Value::Secret(self.0)
    }
}

impl fmt::Debug for SetupToken<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SetupToken([hidden])")
    }
}

/// An address Lodestar itself listens on (its UI and API): its own,
/// never a peer's or a client's.
#[derive(Debug, Clone, Copy)]
pub struct Listen(pub SocketAddr);

impl<'a> IntoValue<'a, kind::Label> for Listen {
    fn into_value(self) -> Value<'a> {
        Value::Listen(self.0)
    }
}

/// Where in the source something happened (the crash event's).
#[derive(Debug, Clone, Copy)]
pub struct Location<'a>(pub &'a std::panic::Location<'a>);

impl<'a> IntoValue<'a, kind::Label> for Location<'a> {
    fn into_value(self) -> Value<'a> {
        Value::Location(self.0.file(), self.0.line())
    }
}

/// An error type whose `Display` is free of names, paths, URLs, addresses
/// and secrets, so [`Error::plain`] may show its message. Implement it
/// only for types whose every message is a fixed text (or holds only
/// numbers and other such types).
pub trait PlainError: fmt::Display {}

/// An error, as a line shows it: its kind always, and its message only
/// for [`PlainError`] types. Every `%e` / `?e` field goes through this
/// (`.github/scripts/log_lint.py` refuses them by syntax).
#[derive(Clone, Copy)]
pub struct Error<'a> {
    kind: &'static str,
    message: Option<&'a dyn fmt::Display>,
}

impl<'a> Error<'a> {
    /// Its type's name, without the message.
    pub fn new<E: ?Sized>(_: &E) -> Error<'static> {
        Error {
            kind: short_type_name(std::any::type_name::<E>()),
            message: None,
        }
    }

    /// A fixed kind, without the message.
    pub const fn kind(kind: &'static str) -> Error<'static> {
        Error {
            kind,
            message: None,
        }
    }

    /// Its kind and its message.
    pub fn plain<E: PlainError>(e: &'a E) -> Error<'a> {
        Error {
            kind: short_type_name(std::any::type_name::<E>()),
            message: Some(e),
        }
    }

    /// An I/O error: its `ErrorKind` (`NotFound`, `PermissionDenied`…). The
    /// message can name a path, so it's never shown.
    pub fn io(e: &std::io::Error) -> Error<'static> {
        Error {
            kind: io_kind(e.kind()),
            message: None,
        }
    }
}

impl<'a> IntoValue<'a, kind::Error> for Error<'a> {
    fn into_value(self) -> Value<'a> {
        Value::Error(self)
    }
}

impl fmt::Display for Error<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.message {
            Some(m) => write!(f, "{}: {m}", self.kind),
            None => f.write_str(self.kind),
        }
    }
}

impl fmt::Debug for Error<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Error({self})")
    }
}

/// `a::b::Thing<c::D>` → `Thing`.
fn short_type_name(name: &'static str) -> &'static str {
    let base = name.split('<').next().unwrap_or(name);
    base.rsplit("::").next().unwrap_or(base)
}

fn io_kind(kind: std::io::ErrorKind) -> &'static str {
    use std::io::ErrorKind as K;
    match kind {
        K::NotFound => "io:NotFound",
        K::PermissionDenied => "io:PermissionDenied",
        K::ConnectionRefused => "io:ConnectionRefused",
        K::ConnectionReset => "io:ConnectionReset",
        K::ConnectionAborted => "io:ConnectionAborted",
        K::NotConnected => "io:NotConnected",
        K::AddrInUse => "io:AddrInUse",
        K::AddrNotAvailable => "io:AddrNotAvailable",
        K::BrokenPipe => "io:BrokenPipe",
        K::AlreadyExists => "io:AlreadyExists",
        K::WouldBlock => "io:WouldBlock",
        K::InvalidInput => "io:InvalidInput",
        K::InvalidData => "io:InvalidData",
        K::TimedOut => "io:TimedOut",
        K::WriteZero => "io:WriteZero",
        K::Interrupted => "io:Interrupted",
        K::Unsupported => "io:Unsupported",
        K::UnexpectedEof => "io:UnexpectedEof",
        K::OutOfMemory => "io:OutOfMemory",
        K::StorageFull => "io:StorageFull",
        K::ReadOnlyFilesystem => "io:ReadOnlyFilesystem",
        K::DirectoryNotEmpty => "io:DirectoryNotEmpty",
        K::IsADirectory => "io:IsADirectory",
        K::NotADirectory => "io:NotADirectory",
        _ => "io:Other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: [u8; 20] = [0xab; 20];

    fn r(h: &[u8; 20]) -> String {
        format!("ref:{:02x}{:02x}{:02x}{:02x}", h[0], h[1], h[2], h[3])
    }

    #[test]
    fn every_kind_renders_hidden_ref_or_value() {
        let all = Mask(u32::MAX);
        let refs: Reference<'_> = &r;
        let t = Value::Torrent(&H);
        assert_eq!(
            t.render(all, false, Some(refs)),
            Rendered::Text("ab".repeat(20))
        );
        assert_eq!(
            t.render(Mask::PRIVATE, false, Some(refs)),
            Rendered::Reference("ref:abababab".into())
        );
        assert_eq!(t.render(Mask::PRIVATE, false, None), Rendered::Hidden);
        for (v, kind, shown) in [
            (Value::Name("Some.Name"), Kind::Name, "Some.Name"),
            (
                Value::Path(std::path::Path::new("a/b.mkv")),
                Kind::Path,
                "a/b.mkv",
            ),
            (Value::Query("ubuntu iso"), Kind::Query, "ubuntu iso"),
            (
                Value::Client("192.0.2.1".parse().unwrap()),
                Kind::Client,
                "192.0.2.1",
            ),
            (
                Value::Peer("203.0.113.7:51413".parse().unwrap()),
                Kind::Peer,
                "203.0.113.7:51413",
            ),
            (
                Value::Detail("term jailbait: override"),
                Kind::FilterChange,
                "term jailbait: override",
            ),
            (
                Value::Url("udp://t.example:1337/announce", url::Form::Absolute),
                Kind::Url,
                "udp://t.example:1337/announce",
            ),
        ] {
            assert_eq!(
                v.render(Mask::NONE.with(kind), false, Some(refs))
                    .to_string(),
                shown
            );
            assert_eq!(
                v.render(Mask(u32::MAX).without(kind), false, Some(refs)),
                Rendered::Hidden,
                "{kind:?}"
            );
        }
        assert_eq!(
            Value::Size(7).render(Mask::PRIVATE, false, None),
            Rendered::Number(7)
        );
        assert_eq!(
            Value::Size(7).render(Mask::NONE, false, None),
            Rendered::Hidden
        );
        // Labels and counts are the code's own words: always shown.
        assert_eq!(
            Value::Label("qbittorrent")
                .render(Mask::NONE, false, None)
                .to_string(),
            "qbittorrent"
        );
        assert_eq!(
            Value::Count(3).render(Mask::NONE, false, None),
            Rendered::Number(3)
        );
    }

    #[test]
    fn a_peer_beside_a_torrent_needs_peer_activity() {
        let peer = Value::Peer("203.0.113.7:51413".parse().unwrap());
        let peers = Mask::NONE.with(Kind::Peer).with(Kind::Hash);
        assert_eq!(peer.render(peers, true, None), Rendered::Hidden);
        assert!(matches!(peer.render(peers, false, None), Rendered::Text(_)));
        assert!(matches!(
            peer.render(peers.with(Kind::PeerActivity), true, None),
            Rendered::Text(_)
        ));
        // The opt-in alone is enough beside a torrent (its warning says
        // the file will record it), and isn't peer addresses on their own.
        assert!(matches!(
            peer.render(Mask::NONE.with(Kind::PeerActivity), true, None),
            Rendered::Text(_)
        ));
        assert_eq!(
            peer.render(Mask::NONE.with(Kind::PeerActivity), false, None),
            Rendered::Hidden
        );
        let fields = [("torrent", Value::Name("x")), ("peer", peer)];
        assert!(pairs_torrent(&fields));
        assert!(!pairs_torrent(&fields[1..]));
    }

    #[test]
    fn urls_lose_credentials_even_when_shown() {
        let u = Value::Url(
            "https://u:pw@t.example/a/0123456789abcdef0123456789abcdef/announce?passkey=s3cr3t&apikey=k&token=t&x=1",
            url::Form::Absolute,
        );
        let shown = u
            .render(Mask::NONE.with(Kind::Url), false, None)
            .to_string();
        for secret in [
            "pw",
            "s3cr3t",
            "0123456789abcdef0123456789abcdef",
            "=k&",
            "=t&",
        ] {
            assert!(!shown.contains(secret), "{secret} in {shown}");
        }
        assert!(shown.contains("x=1"), "{shown}");
    }

    struct Plain;
    impl fmt::Display for Plain {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("timed out")
        }
    }
    impl PlainError for Plain {}

    #[derive(Debug)]
    struct Leaky;
    impl fmt::Display for Leaky {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("/srv/torrents/Secret.Name.mkv")
        }
    }

    #[test]
    fn errors_show_messages_only_when_plain() {
        assert_eq!(Error::plain(&Plain).to_string(), "Plain: timed out");
        assert_eq!(Error::new(&Leaky).to_string(), "Leaky");
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "/srv/x/Secret.Name");
        assert_eq!(Error::io(&io).to_string(), "io:NotFound");
        assert_eq!(Error::kind("refresh").to_string(), "refresh");
        assert!(!format!("{:?}", Error::new(&Leaky)).contains("Secret"));
    }

    #[test]
    fn wrappers_never_debug_their_value() {
        let h = H;
        for s in [
            format!("{:?}", Torrent(&h)),
            format!("{:?}", Name("Secret.Name")),
            format!("{:?}", Path::new("/srv/Secret.Name")),
            format!("{:?}", Url::new("https://t/?passkey=Secret")),
            format!("{:?}", Query("Secret query")),
            format!("{:?}", Detail("Secret term")),
        ] {
            assert!(!s.contains("Secret") && !s.contains("abab"), "{s}");
        }
    }
}
