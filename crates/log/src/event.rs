//! The catalogue's types: levels, components, data kinds, and the events
//! themselves, each with its switch and counter.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// An event's level, from the least to the most verbose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Level {
    pub const ALL: [Level; 5] = [
        Level::Error,
        Level::Warn,
        Level::Info,
        Level::Debug,
        Level::Trace,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
            Level::Trace => "trace",
        }
    }

    pub fn parse(s: &str) -> Option<Level> {
        Level::ALL
            .into_iter()
            .find(|l| l.name().eq_ignore_ascii_case(s.trim()))
            .or_else(|| {
                s.trim()
                    .eq_ignore_ascii_case("warning")
                    .then_some(Level::Warn)
            })
    }
}

/// What part of Lodestar an event comes from (§12.1's per-component
/// levels). `Libraries` holds no catalogued event: it's the third-party
/// crates, reached only by the raw filter. `System` is the process's own
/// events (start, stop, settings, logging itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Component {
    System,
    Dht,
    Fetch,
    Pipeline,
    Search,
    Api,
    Sources,
    Filters,
    Integrations,
    Workers,
    Companions,
    Auth,
    Libraries,
}

impl Component {
    pub const ALL: [Component; 13] = [
        Component::System,
        Component::Dht,
        Component::Fetch,
        Component::Pipeline,
        Component::Search,
        Component::Api,
        Component::Sources,
        Component::Filters,
        Component::Integrations,
        Component::Workers,
        Component::Companions,
        Component::Auth,
        Component::Libraries,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Component::System => "system",
            Component::Dht => "dht",
            Component::Fetch => "fetch",
            Component::Pipeline => "pipeline",
            Component::Search => "search",
            Component::Api => "api",
            Component::Sources => "sources",
            Component::Filters => "filters",
            Component::Integrations => "integrations",
            Component::Workers => "workers",
            Component::Companions => "companions",
            Component::Auth => "auth",
            Component::Libraries => "libraries",
        }
    }

    pub fn parse(s: &str) -> Option<Component> {
        Component::ALL.into_iter().find(|c| c.name() == s)
    }
}

/// A kind of data a line may contain (§12.1's table), each its own switch
/// per output. The last two are the owner's opt-ins (§2.11): no preset
/// turns them on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// Info hashes. Off, a torrent shows as `ref:xxxxxxxx`.
    Hash,
    /// Torrent names.
    Name,
    /// File names and paths.
    Path,
    /// Tracker and source URLs; credentials are always removed.
    Url,
    /// Search queries.
    Query,
    /// Sign-in and API client addresses: the admin's own, so on.
    Client,
    /// Peer addresses, connection-level only.
    Peer,
    /// Sizes and counts: not identifying, so on.
    Size,
    /// Opt-in: a peer address together with a torrent.
    PeerActivity,
    /// Opt-in: which filter rule, term, exception, pack or list changed.
    FilterChange,
}

impl Kind {
    pub const ALL: [Kind; 10] = [
        Kind::Hash,
        Kind::Name,
        Kind::Path,
        Kind::Url,
        Kind::Query,
        Kind::Client,
        Kind::Peer,
        Kind::Size,
        Kind::PeerActivity,
        Kind::FilterChange,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Kind::Hash => "hash",
            Kind::Name => "name",
            Kind::Path => "path",
            Kind::Url => "url",
            Kind::Query => "query",
            Kind::Client => "client",
            Kind::Peer => "peer",
            Kind::Size => "size",
            Kind::PeerActivity => "peer_activity",
            Kind::FilterChange => "filter_changes",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.name() == s)
    }

    /// Identifies torrents or people: turning it on is time-limited.
    pub const fn identifying(self) -> bool {
        matches!(
            self,
            Kind::Hash | Kind::Name | Kind::Path | Kind::Url | Kind::Query | Kind::Peer
        )
    }

    /// On only by opt-in, for at most 24 hours, never by a preset, and
    /// never on the console, journal or Event Log.
    pub const fn opt_in(self) -> bool {
        matches!(self, Kind::PeerActivity | Kind::FilterChange)
    }

    /// On in the default (Private) preset.
    pub const fn default_on(self) -> bool {
        matches!(self, Kind::Client | Kind::Size)
    }

    pub const fn bit(self) -> u32 {
        1 << self as u32
    }
}

/// A set of kinds: what one output may show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Mask(pub u32);

impl Mask {
    pub const NONE: Mask = Mask(0);
    /// The Private preset: client addresses and sizes.
    pub const PRIVATE: Mask = Mask(Kind::Client.bit() | Kind::Size.bit());

    pub const fn has(self, kind: Kind) -> bool {
        self.0 & kind.bit() != 0
    }

    pub const fn with(self, kind: Kind) -> Mask {
        Mask(self.0 | kind.bit())
    }

    pub const fn without(self, kind: Kind) -> Mask {
        Mask(self.0 & !kind.bit())
    }

    /// Without the opt-ins: what the console, journal and Event Log may
    /// ever get.
    pub const fn without_opt_ins(self) -> Mask {
        self.without(Kind::PeerActivity).without(Kind::FilterChange)
    }

    pub fn kinds(self) -> impl Iterator<Item = Kind> {
        Kind::ALL.into_iter().filter(move |k| self.has(*k))
    }

    /// Holds an identifying kind or an opt-in: a file written under it
    /// "contains torrent details".
    pub fn has_details(self) -> bool {
        self.kinds().any(|k| k.identifying() || k.opt_in())
    }
}

/// What a declared field carries: a data kind, an error (its kind, and
/// its message only for plain error types), or a label (a `&'static str`,
/// a number or a flag the code itself wrote, never data).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldKind {
    Data(Kind),
    Error,
    Label,
}

impl FieldKind {
    pub const fn name(self) -> &'static str {
        match self {
            FieldKind::Data(k) => k.name(),
            FieldKind::Error => "error",
            FieldKind::Label => "label",
        }
    }
}

/// One catalogued event (§12.1). Declared with [`crate::catalogue!`] and
/// written with [`crate::log_event!`].
#[derive(Debug)]
pub struct Event {
    /// Stable id: `fetch.failed`. The UI's label and help are the message
    /// keys `logs.event.<id>.label` and `.help`.
    pub id: &'static str,
    pub component: Component,
    pub level: Level,
    /// The line's message. Every dynamic part is a declared field.
    pub message: &'static str,
    /// On in the presets up to Normal. An event that's off by default
    /// (a high-volume one) joins at Detailed.
    pub default_on: bool,
    /// Can't be turned off (start, stop, crash, the setup token…).
    pub required: bool,
    /// On only while this opt-in is (`filters.change_detail`).
    pub opt_in: Option<Kind>,
    /// The fields it may carry, with what each holds.
    pub fields: &'static [(&'static str, FieldKind)],
    /// Read on every occurrence, written only when settings apply: it
    /// shares its line with the declaration, which nothing writes.
    on: AtomicBool,
    /// Written on every occurrence, on or off: striped by thread, each
    /// stripe alone on its cache line, so a crawler thread's increment
    /// never invalidates another event's switch or counter, or another
    /// thread's stripe of the same event, on the other cores.
    count: [Padded<AtomicU64>; STRIPES],
}

/// How many counters an event's count is striped over. A thread takes
/// the next stripe the first time it counts anything, so up to this many
/// threads (the DHT actor, the fetch pool's workers) never share one.
pub const STRIPES: usize = 8;

static NEXT_STRIPE: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static STRIPE: Cell<usize> = const { Cell::new(usize::MAX) };
}

/// This thread's stripe.
#[inline]
fn stripe() -> usize {
    STRIPE.with(|s| match s.get() {
        usize::MAX => {
            let n = NEXT_STRIPE.fetch_add(1, Ordering::Relaxed) % STRIPES;
            s.set(n);
            n
        }
        n => n,
    })
}

/// A value alone on its cache line. 128 bytes covers x86's adjacent-line
/// prefetch and Apple silicon's 128-byte lines.
#[derive(Debug, Default)]
#[repr(align(128))]
pub struct Padded<T>(pub T);

impl<T> std::ops::Deref for Padded<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl Event {
    /// What [`crate::catalogue!`] expands to. Starts as the Normal preset
    /// has it.
    #[allow(clippy::too_many_arguments)]
    pub const fn declare(
        id: &'static str,
        component: Component,
        level: Level,
        message: &'static str,
        default_on: bool,
        required: bool,
        opt_in: Option<Kind>,
        fields: &'static [(&'static str, FieldKind)],
    ) -> Event {
        let on = required || (opt_in.is_none() && default_on && level as u8 <= Level::Info as u8);
        Event {
            id,
            component,
            level,
            message,
            default_on,
            required,
            opt_in,
            fields,
            on: AtomicBool::new(on),
            count: [const { Padded(AtomicU64::new(0)) }; STRIPES],
        }
    }

    /// The switch: one relaxed load.
    #[inline]
    pub fn is_on(&self) -> bool {
        self.on.load(Ordering::Relaxed)
    }

    /// Turns it on or off; a required event stays on.
    pub fn set_on(&self, on: bool) {
        self.on.store(on || self.required, Ordering::Relaxed);
    }

    /// Counts one occurrence, on or off (the volume estimates use it).
    ///
    /// A load and a store on this thread's stripe, not a locked
    /// read-modify-write: the stripe is this thread's alone unless more
    /// than [`STRIPES`] threads count, and then two threads sharing one
    /// can, racing, lose an increment. The count is an estimate (rates
    /// per hour, volume), and that's the price of an event that's off
    /// costing about a nanosecond.
    #[inline]
    pub fn hit(&self) {
        let c = &self.count[stripe()];
        c.store(c.load(Ordering::Relaxed).wrapping_add(1), Ordering::Relaxed);
    }

    /// How many times it happened since start, on or off: the stripes'
    /// sum.
    pub fn count(&self) -> u64 {
        self.count
            .iter()
            .fold(0u64, |n, c| n.wrapping_add(c.load(Ordering::Relaxed)))
    }

    pub fn label_key(&self) -> String {
        format!("logs.event.{}.label", self.id)
    }

    pub fn help_key(&self) -> String {
        format!("logs.event.{}.help", self.id)
    }

    /// Whether `level` and the event's default put it on under a verbosity
    /// that shows `max` (overrides and required aside).
    pub fn on_under(&self, max: Level) -> bool {
        if self.opt_in.is_some() {
            return false;
        }
        self.level <= max && (self.default_on || max >= Level::Debug)
    }

    /// The data kinds it can carry, for the catalogue's badges. A peer
    /// address next to a torrent is peer activity.
    pub fn kinds(&self) -> Vec<Kind> {
        let mut kinds: Vec<Kind> = self
            .fields
            .iter()
            .filter_map(|(_, k)| match k {
                FieldKind::Data(k) => Some(*k),
                _ => None,
            })
            .collect();
        if kinds.contains(&Kind::Peer) && kinds.iter().any(|k| torrent_kind(*k)) {
            kinds.push(Kind::PeerActivity);
        }
        if let Some(k) = self.opt_in {
            kinds.push(k);
        }
        kinds.sort();
        kinds.dedup();
        kinds
    }

    pub fn field(&self, name: &str) -> Option<FieldKind> {
        self.fields
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, k)| *k)
    }
}

/// Kinds that say which torrent a line is about.
pub(crate) const fn torrent_kind(k: Kind) -> bool {
    matches!(k, Kind::Hash | Kind::Name | Kind::Path)
}

/// One crate's events, gathered by the binary ([`crate::register`]).
#[derive(Debug)]
pub struct Catalogue {
    /// The crate that declares them.
    pub name: &'static str,
    pub events: &'static [&'static Event],
}

#[cfg(test)]
mod tests {
    use super::*;

    static PAIR: [Event; 2] = [const {
        Event::declare(
            "t.pair",
            Component::Dht,
            Level::Debug,
            "pair",
            false,
            false,
            None,
            &[],
        )
    }; 2];

    #[test]
    fn the_counter_has_its_own_cache_line() {
        assert_eq!(std::mem::align_of::<Padded<AtomicU64>>(), 128);
        assert_eq!(std::mem::size_of::<Padded<AtomicU64>>(), 128);
        assert_eq!(std::mem::align_of::<Event>(), 128);
        let on = std::mem::offset_of!(Event, on);
        let count = std::mem::offset_of!(Event, count);
        let counters = count..count + STRIPES * 128;
        assert_eq!(count % 128, 0);
        assert!(
            !counters.contains(&on),
            "the switch is off the counters' lines: on at {on}, counters at {counters:?}"
        );
        assert!(std::mem::size_of::<Event>() >= (STRIPES + 1) * 128);
        // Neighbours in an array: no counter shares a line with another
        // event's switch or counter.
        let lines = |e: &Event| {
            let base = e as *const Event as usize;
            ((base + on) / 128, (base + count) / 128)
        };
        let (on0, count0) = lines(&PAIR[0]);
        let (on1, count1) = lines(&PAIR[1]);
        let stripes = |first: usize| first..first + STRIPES;
        for (line, counters) in [(on1, count0), (on0, count0), (on0, count1), (on1, count1)] {
            assert!(!stripes(counters).contains(&line));
        }
        assert!(!stripes(count0).contains(&count1) && !stripes(count1).contains(&count0));
    }

    #[test]
    fn threads_take_their_own_stripes_and_the_count_sums_them() {
        static E: Event = Event::declare(
            "t.e",
            Component::Fetch,
            Level::Debug,
            "e",
            false,
            false,
            None,
            &[],
        );
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    for _ in 0..1000 {
                        E.hit();
                    }
                });
            }
        });
        // Four threads, each on its own stripe unless the process has
        // made more than STRIPES counting threads before; the sum is exact
        // when no two raced on one.
        let used = E
            .count
            .iter()
            .filter(|c| c.load(Ordering::Relaxed) > 0)
            .count();
        assert!((1..=4).contains(&used));
        assert!(E.count() <= 4000 && E.count() >= 1000, "{}", E.count());
    }

    #[test]
    fn off_events_still_count() {
        let e = &PAIR[0];
        assert!(!e.is_on());
        let before = e.count();
        e.hit();
        e.hit();
        assert_eq!(e.count(), before + 2);
    }
}
