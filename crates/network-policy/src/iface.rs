//! Crawling through one network interface (X-vpn-bind,
//! `docs/decisions/2026-11-28-X-vpn-bind.md`).
//!
//! With an interface named, every socket the crawl opens is pinned to it
//! before it binds or connects ([`pin`]), and a [`Guard`] watches the
//! interface. While it's missing or down the crawl pauses as "VPN down",
//! and when it's back every socket is made again ([`State::Up`]'s
//! generation changes), because a socket pinned to an interface that was
//! deleted stays pinned to the old one.
//!
//! - **Linux:** `SO_BINDTODEVICE`. The kernel then routes the socket's
//!   packets only through that device, and fails the send when it's gone.
//!   Unprivileged since Linux 5.7 on a socket that isn't bound yet, which
//!   is how [`pin`] is always called; older kernels need `CAP_NET_RAW`,
//!   and without it the pin fails, so the crawl stays down.
//! - **Windows:** the socket binds to the interface's own address. The
//!   strong host model (the default since Vista) sends a packet with that
//!   source address only through the interface that owns it.
//! - **Elsewhere:** unsupported; every pin fails, so the crawl stays down.
//!
//! **The socket is the safety, not the probe.** A probe that wrongly says
//! "up" still can't make a pinned socket send by another route.

use std::fmt;
use std::io;
#[cfg(windows)]
use std::net::IpAddr;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use socket2::Socket;
use tokio::sync::watch;

/// How often [`Guard::run`] checks the interface.
pub const CHECK_EVERY: Duration = Duration::from_secs(2);

/// The longest name accepted: Windows' friendly names are at most 256
/// characters; Linux's are much shorter ([`check_linux_name`]).
pub const MAX_NAME: usize = 256;

/// A network interface name that passed this platform's checks. It's
/// only ever used as a socket option, a sysfs path segment (Linux) or a
/// string compared with the system's list (Windows), never in a shell.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Interface(Arc<str>);

/// Why a name isn't a valid interface name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidName;

impl fmt::Display for InvalidName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("that isn't a valid network interface name")
    }
}

impl std::error::Error for InvalidName {}

/// Linux's own rule (`dev_valid_name`): 1 to 15 bytes, not `.` or `..`,
/// and only visible ASCII without `/` or `:`. So it's also safe as one
/// path segment.
pub fn check_linux_name(name: &str) -> Result<(), InvalidName> {
    let ok = !name.is_empty()
        && name.len() < 16
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_graphic() && b != b'/' && b != b':');
    ok.then_some(()).ok_or(InvalidName)
}

/// Windows' friendly names ("Ethernet 2", "WireGuard Tunnel"): 1 to
/// [`MAX_NAME`] characters, no control characters, and no leading or
/// trailing space (so a stray space can't name a different adapter).
pub fn check_windows_name(name: &str) -> Result<(), InvalidName> {
    let ok = !name.is_empty()
        && name.chars().count() <= MAX_NAME
        && name.trim() == name
        && !name.chars().any(char::is_control);
    ok.then_some(()).ok_or(InvalidName)
}

impl Interface {
    /// Checks `name` with this platform's rule: Windows' on Windows,
    /// Linux's everywhere else (pinning fails off Linux and Windows
    /// anyway).
    pub fn parse(name: &str) -> Result<Interface, InvalidName> {
        if cfg!(windows) {
            check_windows_name(name)?;
        } else {
            check_linux_name(name)?;
        }
        Ok(Interface(name.into()))
    }

    pub fn name(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Interface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*self.0, f)
    }
}

impl fmt::Display for Interface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Pins `socket`, about to bind `addr`, to `iface`, and returns the
/// address to bind. Call it before `bind` and `connect`; a socket that
/// connects without binding binds the returned address first (Windows
/// needs it; on Linux it's `addr` unchanged).
///
/// Fails, and the socket must not be used, when the interface is missing
/// (Linux: `ENODEV`), has no address of `addr`'s family (Windows), or
/// `addr` names a specific address that isn't the interface's (Windows).
pub fn pin(socket: &Socket, iface: &Interface, addr: SocketAddr) -> io::Result<SocketAddr> {
    PINS.fetch_add(1, Ordering::Relaxed);
    pin_os(socket, iface, addr)
}

#[cfg(target_os = "linux")]
fn pin_os(socket: &Socket, iface: &Interface, addr: SocketAddr) -> io::Result<SocketAddr> {
    socket.bind_device(Some(iface.name().as_bytes()))?;
    Ok(addr)
}

#[cfg(windows)]
fn pin_os(_: &Socket, iface: &Interface, addr: SocketAddr) -> io::Result<SocketAddr> {
    let link = windows_link(iface).map_err(Down::into_io)?;
    let ip = link
        .addrs
        .iter()
        .copied()
        .find(|ip| ip.is_ipv6() == addr.is_ipv6())
        .ok_or_else(|| Down::NoAddress.into_io())?;
    if !addr.ip().is_unspecified() && addr.ip() != ip {
        return Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            format!("{} isn't an address of {iface}", addr.ip()),
        ));
    }
    Ok(SocketAddr::new(ip, addr.port()))
}

#[cfg(not(any(target_os = "linux", windows)))]
fn pin_os(_: &Socket, _: &Interface, _: SocketAddr) -> io::Result<SocketAddr> {
    Err(Down::Unsupported.into_io())
}

static PINS: AtomicU64 = AtomicU64::new(0);

/// Sockets pinned by this process so far, for tests that show a socket
/// constructor goes through [`pin`].
pub fn pins() -> u64 {
    PINS.load(Ordering::Relaxed)
}

/// Whether a socket error says the interface it's pinned to went away
/// (`ENODEV`, `ENETDOWN`, `EADDRNOTAVAIL` or Windows' equivalents), so
/// the guard should check again now rather than at its next tick.
pub fn is_gone(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::NetworkDown | io::ErrorKind::AddrNotAvailable
    ) || (cfg!(target_os = "linux") && e.raw_os_error() == Some(19))
}

/// Why the interface can't be used. [`code`](Self::code) is the stable
/// code the API gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Down {
    /// No interface has that name.
    Missing,
    /// It exists but its link is down.
    Down,
    /// It has no address to bind (Windows).
    NoAddress,
    /// A socket couldn't be pinned to it, or one pinned to it failed as
    /// if it were gone ([`is_gone`]).
    BindFailed,
    /// This platform can't pin a socket to an interface.
    Unsupported,
}

impl Down {
    pub const fn code(self) -> &'static str {
        match self {
            Down::Missing => "missing",
            Down::Down => "down",
            Down::NoAddress => "no_address",
            Down::BindFailed => "bind_failed",
            Down::Unsupported => "unsupported",
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn into_io(self) -> io::Error {
        let kind = match self {
            Down::Unsupported => io::ErrorKind::Unsupported,
            Down::NoAddress => io::ErrorKind::AddrNotAvailable,
            _ => io::ErrorKind::NotFound,
        };
        io::Error::new(kind, format!("network interface {}", self.code()))
    }
}

/// What a check saw of a usable interface. Two checks that see different
/// links (Linux: a new index, the interface was made again; Windows: a
/// new address) mean the old sockets are stale.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Link {
    pub index: Option<u32>,
    #[cfg(windows)]
    pub addrs: Vec<IpAddr>,
}

/// Checks the interface now.
///
/// - **Linux:** `/sys/class/net/<name>`: `IFF_UP` set, `operstate` not
///   `down`, `lowerlayerdown`, `notpresent` or `dormant` (tunnels report
///   `unknown`), and no `carrier` of 0 where the kernel reports one. A
///   WireGuard tunnel with no live peer still reads up: the kernel can't
///   tell; its packets still go nowhere else.
/// - **Windows:** an adapter with that friendly name, operationally up,
///   with an address.
pub fn check(iface: &Interface) -> Result<Link, Down> {
    check_os(iface)
}

#[cfg(target_os = "linux")]
fn check_os(iface: &Interface) -> Result<Link, Down> {
    // The name passed `check_linux_name`, so it's one path segment.
    let read =
        |file: &str| std::fs::read_to_string(format!("/sys/class/net/{}/{file}", iface.name()));
    let flags = read("flags").map_err(|_| Down::Missing)?;
    let flags = u32::from_str_radix(flags.trim().trim_start_matches("0x"), 16)
        .map_err(|_| Down::Missing)?;
    const IFF_UP: u32 = 0x1;
    if flags & IFF_UP == 0 {
        return Err(Down::Down);
    }
    let oper = read("operstate").map_err(|_| Down::Missing)?;
    if matches!(
        oper.trim(),
        "down" | "lowerlayerdown" | "notpresent" | "dormant"
    ) {
        return Err(Down::Down);
    }
    if read("carrier").is_ok_and(|c| c.trim() == "0") {
        return Err(Down::Down);
    }
    let index = read("ifindex")
        .map_err(|_| Down::Missing)?
        .trim()
        .parse()
        .map_err(|_| Down::Missing)?;
    Ok(Link { index: Some(index) })
}

#[cfg(windows)]
fn check_os(iface: &Interface) -> Result<Link, Down> {
    windows_link(iface)
}

#[cfg(windows)]
fn windows_link(iface: &Interface) -> Result<Link, Down> {
    let all = if_addrs::get_if_addrs().map_err(|_| Down::Missing)?;
    let mine: Vec<_> = all.into_iter().filter(|i| i.name == iface.name()).collect();
    if mine.is_empty() {
        return Err(Down::Missing);
    }
    if !mine
        .iter()
        .any(|i| i.oper_status == if_addrs::IfOperStatus::Up)
    {
        return Err(Down::Down);
    }
    let mut addrs: Vec<IpAddr> = mine
        .iter()
        .map(|i| i.ip())
        // A link-local v6 address needs a scope id the crawl's sockets
        // don't carry; it can't reach the DHT anyway.
        .filter(|ip| match ip {
            IpAddr::V4(v4) => !v4.is_unspecified() && !v4.is_link_local(),
            IpAddr::V6(v6) => !v6.is_unspecified() && !v6.is_unicast_link_local(),
        })
        .collect();
    // One address per family, the same one each time: the first v4 and
    // the first v6 in sorted order.
    addrs.sort();
    addrs.dedup();
    let v4 = addrs.iter().copied().find(IpAddr::is_ipv4);
    let v6 = addrs.iter().copied().find(IpAddr::is_ipv6);
    let addrs: Vec<IpAddr> = v4.into_iter().chain(v6).collect();
    if addrs.is_empty() {
        return Err(Down::NoAddress);
    }
    Ok(Link {
        index: mine.iter().find_map(|i| i.index),
        addrs,
    })
}

#[cfg(not(any(target_os = "linux", windows)))]
fn check_os(_: &Interface) -> Result<Link, Down> {
    Err(Down::Unsupported)
}

/// The interface as the crawl sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Usable. `generation` changes each time it comes (back) up or its
    /// link changes; sockets made under an older one are stale.
    Up {
        generation: u64,
    },
    Down(Down),
}

impl State {
    pub fn is_up(self) -> bool {
        matches!(self, State::Up { .. })
    }
}

type Probe = dyn Fn(&Interface) -> Result<Link, Down> + Send + Sync;

/// Watches one interface for the crawl: [`check`]s it every
/// [`CHECK_EVERY`] ([`run`](Self::run)) and at once when a socket reports
/// it gone ([`trip`](Self::trip)).
pub struct Guard {
    iface: Interface,
    probe: Box<Probe>,
    state: watch::Sender<State>,
    /// The link last seen up; `None` while down.
    link: std::sync::Mutex<Option<Link>>,
    generation: AtomicU64,
    trips: AtomicU64,
}

impl fmt::Debug for Guard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Guard")
            .field("iface", &self.iface)
            .field("state", &*self.state.borrow())
            .finish_non_exhaustive()
    }
}

impl Guard {
    /// A guard that has checked `iface` once already.
    pub fn new(iface: Interface) -> Arc<Guard> {
        Guard::with_probe(iface, check)
    }

    /// With another probe, for tests.
    pub fn with_probe(
        iface: Interface,
        probe: impl Fn(&Interface) -> Result<Link, Down> + Send + Sync + 'static,
    ) -> Arc<Guard> {
        let guard = Arc::new(Guard {
            iface,
            probe: Box::new(probe),
            state: watch::Sender::new(State::Down(Down::Missing)),
            link: std::sync::Mutex::new(None),
            generation: AtomicU64::new(0),
            trips: AtomicU64::new(0),
        });
        guard.poll();
        guard
    }

    pub fn interface(&self) -> &Interface {
        &self.iface
    }

    pub fn state(&self) -> State {
        *self.state.borrow()
    }

    pub fn subscribe(&self) -> watch::Receiver<State> {
        self.state.subscribe()
    }

    /// Times the interface went from up to down.
    pub fn trips(&self) -> u64 {
        self.trips.load(Ordering::Relaxed)
    }

    fn link(&self) -> std::sync::MutexGuard<'_, Option<Link>> {
        self.link.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Checks the interface now and publishes what changed.
    pub fn poll(&self) -> State {
        let seen = (self.probe)(&self.iface);
        let mut link = self.link();
        match seen {
            Ok(now) if link.as_ref() == Some(&now) => {}
            Ok(now) => {
                *link = Some(now);
                let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
                self.state.send_replace(State::Up { generation });
            }
            Err(why) => self.set_down(&mut link, why),
        }
        self.state()
    }

    fn set_down(&self, link: &mut Option<Link>, why: Down) {
        if link.take().is_some() {
            self.trips.fetch_add(1, Ordering::Relaxed);
        }
        self.state.send_if_modified(|s| {
            let changed = *s != State::Down(why);
            *s = State::Down(why);
            changed
        });
    }

    /// A pinned socket failed: when the error says the interface is gone
    /// ([`is_gone`]), the crawl goes down now, and the next
    /// [`poll`](Self::poll) that finds the interface usable brings it up
    /// under a new generation, so every socket is made again.
    pub fn trip(&self, e: &io::Error) {
        if is_gone(e) {
            self.down(Down::BindFailed);
        }
    }

    /// Marks the interface down for `why` (a socket couldn't be pinned).
    pub fn down(&self, why: Down) {
        let mut link = self.link();
        self.set_down(&mut link, why);
    }

    /// Checks every `every` until the last strong reference goes.
    pub async fn run(self: Arc<Guard>, every: Duration) {
        let weak = Arc::downgrade(&self);
        drop(self);
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let Some(guard) = weak.upgrade() else { break };
            guard.poll();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn linux_names_follow_the_kernels_rule() {
        // `$(x)` is visible ASCII, so valid; the name never reaches a
        // shell, only a socket option and one sysfs path segment.
        for ok in [
            "wg0",
            "tun0",
            "eth0.100",
            "br-1a2b3c4d5e6f",
            "a",
            "x_y-z",
            "$(x)",
        ] {
            assert_eq!(check_linux_name(ok), Ok(()), "{ok}");
        }
        for bad in [
            "",
            ".",
            "..",
            "wg/0",
            "wg:0",
            "wg 0",
            "sixteen-bytes-xx",
            "tab\t",
            "nul\0",
            "é",
        ] {
            assert_eq!(check_linux_name(bad), Err(InvalidName), "{bad:?}");
        }
    }

    #[test]
    fn windows_names_allow_friendly_names_only() {
        for ok in [
            "WireGuard Tunnel",
            "Ethernet 2",
            "Local Area Connection* 1",
            "wg0",
        ] {
            assert_eq!(check_windows_name(ok), Ok(()), "{ok}");
        }
        let long = "x".repeat(MAX_NAME + 1);
        for bad in ["", " wg0", "wg0 ", "a\nb", "a\0b", long.as_str()] {
            assert_eq!(check_windows_name(bad), Err(InvalidName), "{bad:?}");
        }
        assert_eq!(check_windows_name(&"é".repeat(MAX_NAME)), Ok(()));
    }

    #[test]
    fn down_codes_are_stable() {
        let codes: Vec<_> = [
            Down::Missing,
            Down::Down,
            Down::NoAddress,
            Down::BindFailed,
            Down::Unsupported,
        ]
        .map(Down::code)
        .into();
        assert_eq!(
            codes,
            [
                "missing",
                "down",
                "no_address",
                "bind_failed",
                "unsupported"
            ]
        );
    }

    #[test]
    fn gone_errors_are_recognised() {
        assert!(is_gone(&io::Error::from(io::ErrorKind::NetworkDown)));
        assert!(is_gone(&io::Error::from(io::ErrorKind::AddrNotAvailable)));
        assert!(!is_gone(&io::Error::from(io::ErrorKind::ConnectionRefused)));
        assert!(!is_gone(&io::Error::from(io::ErrorKind::TimedOut)));
        assert_eq!(
            is_gone(&io::Error::from_raw_os_error(19)),
            cfg!(target_os = "linux")
        );
    }

    fn link(index: u32) -> Link {
        Link {
            index: Some(index),
            ..Link::default()
        }
    }

    /// A guard over a probe the test sets.
    fn scripted(first: Result<Link, Down>) -> (Arc<Guard>, Arc<Mutex<Result<Link, Down>>>) {
        let seen = Arc::new(Mutex::new(first));
        let s = seen.clone();
        let guard = Guard::with_probe(Interface::parse("wg0").unwrap(), move |_| {
            s.lock().unwrap().clone()
        });
        (guard, seen)
    }

    #[test]
    fn the_guard_follows_the_link_and_changes_generation_on_each_return() {
        let (guard, seen) = scripted(Err(Down::Missing));
        assert_eq!(guard.state(), State::Down(Down::Missing), "starts down");
        *seen.lock().unwrap() = Ok(link(7));
        assert_eq!(guard.poll(), State::Up { generation: 1 });
        assert_eq!(guard.poll(), State::Up { generation: 1 }, "same link");
        *seen.lock().unwrap() = Err(Down::Down);
        assert_eq!(guard.poll(), State::Down(Down::Down));
        assert_eq!(guard.trips(), 1);
        *seen.lock().unwrap() = Ok(link(7));
        assert_eq!(guard.poll(), State::Up { generation: 2 }, "back: rebind");
        // Made again with a new index while we weren't looking: stale.
        *seen.lock().unwrap() = Ok(link(9));
        assert_eq!(guard.poll(), State::Up { generation: 3 });
        assert_eq!(guard.trips(), 1, "no down in between");
    }

    #[test]
    fn a_gone_socket_trips_the_guard_until_the_next_check() {
        let (guard, _seen) = scripted(Ok(link(3)));
        assert_eq!(guard.state(), State::Up { generation: 1 });
        guard.trip(&io::Error::from(io::ErrorKind::ConnectionReset));
        assert!(guard.state().is_up(), "an ordinary error isn't a trip");
        guard.trip(&io::Error::from(io::ErrorKind::NetworkDown));
        assert_eq!(guard.state(), State::Down(Down::BindFailed));
        assert_eq!(guard.trips(), 1);
        // The same link is back on the next check, under a new
        // generation: every socket is made again.
        assert_eq!(guard.poll(), State::Up { generation: 2 });
    }

    #[test]
    fn subscribers_see_each_change_once() {
        let (guard, seen) = scripted(Ok(link(1)));
        let mut rx = guard.subscribe();
        rx.borrow_and_update();
        guard.poll();
        assert!(!rx.has_changed().unwrap(), "no change, no wake");
        *seen.lock().unwrap() = Err(Down::Missing);
        guard.poll();
        guard.poll();
        assert!(rx.has_changed().unwrap());
        assert_eq!(*rx.borrow_and_update(), State::Down(Down::Missing));
        guard.poll();
        assert!(!rx.has_changed().unwrap(), "still down: no second wake");
    }

    #[tokio::test]
    async fn run_polls_until_the_guard_is_dropped() {
        let (guard, seen) = scripted(Err(Down::Missing));
        let task = tokio::spawn(guard.clone().run(Duration::from_millis(10)));
        let mut rx = guard.subscribe();
        *seen.lock().unwrap() = Ok(link(1));
        tokio::time::timeout(Duration::from_secs(10), rx.wait_for(|s| s.is_up()))
            .await
            .expect("up within 10 s")
            .unwrap();
        drop(rx);
        drop(guard);
        tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .expect("stops once dropped")
            .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_missing_interface_fails_the_pin_and_the_check() {
        let iface = Interface::parse("lsnone0").unwrap();
        assert_eq!(check(&iface), Err(Down::Missing));
        let s = Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None).unwrap();
        let before = pins();
        let e = pin(&s, &iface, "0.0.0.0:0".parse().unwrap()).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(19), "ENODEV: {e}");
        assert!(is_gone(&e));
        assert!(pins() > before);
    }

    /// The first interface that isn't loopback and checks up.
    #[cfg(target_os = "linux")]
    fn other_interface() -> Option<Interface> {
        let mut names: Vec<String> = std::fs::read_dir("/sys/class/net")
            .ok()?
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .collect();
        names.sort();
        names
            .iter()
            .filter(|n| *n != "lo")
            .filter_map(|n| Interface::parse(n).ok())
            .find(|i| check(i).is_ok())
    }

    /// The pin overrides the routing table: a loopback server, which the
    /// table reaches through `lo`, can't be reached by a socket pinned to
    /// another interface, and can by one pinned to `lo` or not pinned.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_pinned_socket_uses_only_its_interface() {
        use std::net::TcpListener;
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let to: SocketAddr = server.local_addr().unwrap();
        let dial = |iface: Option<&Interface>| {
            let s = Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
            if let Some(i) = iface {
                pin(&s, i, "0.0.0.0:0".parse().unwrap()).unwrap();
            }
            s.connect_timeout(&to.into(), Duration::from_secs(10))
        };
        dial(None).expect("unpinned reaches loopback");
        let lo = Interface::parse("lo").unwrap();
        assert!(check(&lo).is_ok(), "lo is always up");
        dial(Some(&lo)).expect("pinned to lo reaches loopback");
        let Some(other) = other_interface() else {
            println!("skipped the second half: no interface but lo is up here");
            return;
        };
        assert!(
            dial(Some(&other)).is_err(),
            "a socket pinned to {other} reached loopback through lo"
        );
    }
}
