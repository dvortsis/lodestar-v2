//! The result cache (architecture §8): first pages of searches and whole
//! facet responses, reused while the index snapshot they were computed
//! on is the one serving.
//!
//! **The key** is everything a result depends on: the query as the
//! index runs it (the caller's visibility clauses included), a scope the
//! caller names (the API's visibility bits, so two requests that may see
//! different torrents never share an entry even if their queries print
//! alike), the sort and page size or the facet parameters, `now` when the
//! query has relative dates, and the **generation**: the exact segments
//! (and each one's deletes) of the torrent index snapshot, plus the file
//! index's for a query with `file:(…)`. Segment ids are random and
//! segments immutable, so equal generations see the same documents, and
//! any commit that changes the documents (new torrents, a re-indexed one
//! a filter rule hid or released, a deletion, an index swap) is a new
//! generation, a miss by construction. A reload that changed nothing
//! keeps the generation, and the entries with it.
//!
//! **Invalidation is the simple rule: any commit retires every entry.**
//! A finer rule would have to test each entry against each commit's new
//! segments; see `docs/decisions/2026-10-22-B5c.md` for the numbers.
//! Entries of a generation no longer serving are dropped at the next
//! lookup that sees a new one, by the lease sweeper, and when a rebuilt
//! index is swapped in.
//!
//! **A cached search keeps its lease** ([`CachedSearch::lease`]): a hit
//! hands the same lease (the same snapshot, compiled query and count) to
//! the client, so page 2 after a cached page 1 reads the snapshot page 1
//! came from. Entries live at most [`CacheConfig::ttl`] from when they
//! were computed, so a cached lease never outlives a lease's bounds.
//! A search with `file:(…)` isn't kept: its lease holds the clauses' id
//! sets, which the byte estimate doesn't cover.
//!
//! **Bounded** in entries and estimated bytes per kind, least recently
//! used first out. The lock is held only to look up, insert or evict,
//! never across a search; identical misses at the same time compute once
//! (single flight) and the others wait for that result.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tantivy::Searcher;
use tantivy::index::SegmentId;

use crate::cancel::CancelToken;
use crate::count::Total;
use crate::facets::{self, FacetParams, Facets};
use crate::index::{self, Hit, Result, SearchIndex, Sort};
use crate::lease::{Lease, Page, Start};
use crate::query::Node;

/// Bounds on the [`ResultCache`], per kind (searches, facets).
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Off: every lookup computes and nothing is kept.
    pub enabled: bool,
    /// Entries kept per kind; past it the least recently used goes.
    pub max_entries: usize,
    /// Estimated bytes kept per kind; past it the least recently used
    /// goes. An entry larger than this isn't kept.
    pub max_bytes: usize,
    /// An entry is dropped this long after it was computed, used or not.
    pub ttl: Duration,
}

impl Default for CacheConfig {
    fn default() -> Self {
        CacheConfig {
            enabled: true,
            max_entries: 512,
            max_bytes: 8 << 20,
            ttl: Duration::from_secs(600),
        }
    }
}

/// What a cached search holds besides its page: the lease, at about 1–2
/// KB (architecture §16, B5a), estimated at the top of that range.
const LEASE_BYTES: usize = 2048;
/// Bookkeeping per entry (the slot, the key's generation, the map).
const ENTRY_BYTES: usize = 256;

/// The documents a searcher sees: its segments and each one's deletes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Generation(Arc<[(SegmentId, Option<u64>)]>);

impl Generation {
    pub fn of(searcher: &Searcher) -> Generation {
        Generation(
            searcher
                .generation()
                .segments()
                .iter()
                .map(|(id, deletes)| (*id, *deletes))
                .collect(),
        )
    }
}

/// The snapshots a result was computed on.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Snapshot {
    torrents: Generation,
    /// Only for a query with `file:(…)`, whose id sets come from the file
    /// index's snapshot at the time.
    files: Option<Generation>,
}

impl Snapshot {
    /// The snapshot serving now, and where it stands among the index's.
    fn current(index: &SearchIndex, files: bool) -> (Snapshot, Seen) {
        let searcher = index.searcher();
        let at = Snapshot {
            torrents: Generation::of(&searcher),
            files: files.then(|| Generation::of(&index.files_searcher())),
        };
        (at, Seen::of(index, &searcher))
    }

    /// Estimated bytes of its generations (each key holds its own).
    fn bytes(&self) -> usize {
        let one = |g: &Generation| 16 + g.0.len() * size_of::<(SegmentId, Option<u64>)>();
        one(&self.torrents) + self.files.as_ref().map_or(0, one)
    }
}

/// Which index a snapshot belongs to and its reader's reload count, so a
/// lookup on a snapshot older than the latest one seen (a lease taken
/// before a commit, a searcher taken just before a reload) is told apart
/// from a new generation. Not part of the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Seen {
    /// The index's identity: its clones' shared liveness token.
    index: usize,
    /// `SearcherGeneration::generation_id`, which grows with each reload
    /// of one index's reader.
    id: u64,
}

impl Seen {
    fn of(index: &SearchIndex, searcher: &Searcher) -> Seen {
        Seen {
            index: std::sync::Weak::as_ptr(&index.holders()) as usize,
            id: searcher.generation().generation_id(),
        }
    }

    /// Whether this is an older snapshot of the same index than `latest`.
    fn before(&self, latest: &Seen) -> bool {
        self.index == latest.index && self.id < latest.id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    /// The query, the scope and the parameters, printed.
    what: String,
    at: Snapshot,
    /// For a query with relative dates, the second they resolved at.
    now: Option<i64>,
}

/// A first page and the lease it was read from.
#[derive(Debug)]
pub struct CachedSearch {
    /// Hand it to the client for the pages after: it reads the same
    /// snapshot. Its count can be made exact on demand
    /// ([`Lease::exact_total`]), which every later hit then reports.
    pub lease: Arc<Lease>,
    /// Hits from offset 0 and where the next page starts.
    pub page: Page,
}

/// A facet response and the count of the lease it was computed on.
#[derive(Debug)]
pub struct CachedFacets {
    pub facets: Facets,
    pub total: Total,
}

/// How a lookup was answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Hit,
    /// An identical miss was being computed; this waited for it.
    Joined,
    Miss,
}

/// A search's first page, as [`ResultCache::search`] looks it up.
#[derive(Debug, Clone, Copy)]
pub struct SearchSpec<'a> {
    /// The query as the index runs it, visibility clauses included.
    pub node: &'a Node,
    /// What else decides which torrents the caller may see (the API's
    /// visibility), so requests that may see different torrents never
    /// share an entry.
    pub scope: &'a str,
    pub sort: Sort,
    /// The first page's size.
    pub limit: usize,
    /// Unix seconds, for relative dates.
    pub now: i64,
}

/// A facet request, as [`ResultCache::facets`] looks it up: the
/// arguments of [`facets::facets`] and those of the lease it runs on.
#[derive(Debug, Clone, Copy)]
pub struct FacetSpec<'a> {
    /// The query the lease runs: `with_filters(node, filters)`.
    pub query: &'a Node,
    /// The user's part, which disjunction edits.
    pub node: &'a Node,
    /// Always-on clauses ANDed onto every pass.
    pub filters: &'a [Node],
    pub params: &'a FacetParams,
    pub scope: &'a str,
    /// The search's sort, for a lease taken here.
    pub sort: Sort,
    pub now: i64,
}

/// Counters of one kind of entry, for `/metrics`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KindStats {
    pub hits: u64,
    /// Waited for an identical miss computed at the same time.
    pub joined: u64,
    pub misses: u64,
    /// Dropped to stay within the bounds.
    pub evictions: u64,
    /// Dropped because their generation stopped serving, or their ttl
    /// passed.
    pub retired: u64,
    pub entries: u64,
    /// Estimated bytes held.
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub search: KindStats,
    pub facets: KindStats,
}

/// The cache of first pages and facet responses (see the module docs).
#[derive(Debug)]
pub struct ResultCache {
    search: Lru<Arc<CachedSearch>>,
    facets: Lru<Arc<CachedFacets>>,
}

impl Default for ResultCache {
    fn default() -> Self {
        ResultCache::new(CacheConfig::default())
    }
}

impl ResultCache {
    pub fn new(config: CacheConfig) -> ResultCache {
        ResultCache {
            search: Lru::new(config.clone()),
            facets: Lru::new(config),
        }
    }

    /// The first `spec.limit` hits of a search on `index`, from the cache
    /// when the snapshot serving now has them, else from a new lease.
    pub fn search(
        &self,
        index: &Arc<SearchIndex>,
        spec: &SearchSpec<'_>,
        cancel: &CancelToken,
    ) -> Result<(Arc<CachedSearch>, Outcome)> {
        let files = spec.node.has_file_clause();
        let what = format!(
            "{:?}|{}|{:?}|limit={}",
            spec.node, spec.scope, spec.sort, spec.limit
        );
        let now = index::relative(spec.node).then_some(spec.now);
        let (at, seen) = Snapshot::current(index, files);
        let key = Key { what, at, now };
        // A lease with `file:(…)` holds the clauses' id sets (a sorted id
        // list per set, and a bitset of up to the highest id / 8 bytes),
        // far beyond the per-entry estimate: those searches aren't kept.
        let cache = !files;
        self.search.get_or_compute(key, seen, cache, cancel, |key| {
            let lease = Arc::new(Lease::new(
                index.clone(),
                spec.node,
                spec.now,
                spec.sort,
                cancel,
            )?);
            let page = lease.page(Start::Offset(0), spec.limit, cancel)?;
            let kept = settled(index, key, lease.searcher(), files);
            let bytes = ENTRY_BYTES
                + LEASE_BYTES
                + key.what.len()
                + key.at.bytes()
                + page.hits.len() * size_of::<Hit>();
            Ok(Computed {
                value: Arc::new(CachedSearch { lease, page }),
                key: kept,
                bytes,
            })
        })
    }

    /// Facets for `spec` on a new lease, from the cache when the snapshot
    /// serving now has them.
    pub fn facets(
        &self,
        index: &Arc<SearchIndex>,
        spec: &FacetSpec<'_>,
        cancel: &CancelToken,
    ) -> Result<(Arc<CachedFacets>, Outcome)> {
        let (at, seen) = Snapshot::current(index, spec.query.has_file_clause());
        let key = facet_key(spec, at, spec.now);
        self.facets.get_or_compute(key, seen, true, cancel, |key| {
            let lease = Lease::new(index.clone(), spec.query, spec.now, spec.sort, cancel)?;
            let got = facets::facets(&lease, spec.node, spec.filters, spec.params, cancel)?;
            let kept = settled(index, key, lease.searcher(), spec.query.has_file_clause());
            Ok(facets_entry(got, lease.total(), kept))
        })
    }

    /// Facets for `spec` on `lease` (a search's, from its cursor), from the
    /// cache when its snapshot is the one `serving` has now; a lease on an
    /// older snapshot (or a retired index) is counted without the cache,
    /// so it never retires the entries of the one serving. The count is
    /// the lease's own, as it is now (an exact count on demand included).
    pub fn facets_on(
        &self,
        serving: &SearchIndex,
        lease: &Arc<Lease>,
        spec: &FacetSpec<'_>,
        cancel: &CancelToken,
    ) -> Result<(Arc<CachedFacets>, Outcome)> {
        let files = spec.query.has_file_clause();
        let index = lease.index();
        let at = Snapshot {
            torrents: Generation::of(lease.searcher()),
            files: files.then(|| Generation::of(&index.files_searcher())),
        };
        let seen = Seen::of(index, lease.searcher());
        let now = Seen::of(serving, &serving.searcher());
        let cache = seen == now;
        let key = facet_key(spec, at, lease.now());
        let (got, outcome) = self
            .facets
            .get_or_compute(key, seen, cache, cancel, |key| {
                let got = facets::facets(lease, spec.node, spec.filters, spec.params, cancel)?;
                let kept = settled(index, key, lease.searcher(), files);
                Ok(facets_entry(got, lease.total(), kept))
            })?;
        let total = lease.total();
        if got.total == total {
            return Ok((got, outcome));
        }
        Ok((
            Arc::new(CachedFacets {
                facets: got.facets.clone(),
                total,
            }),
            outcome,
        ))
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            search: self.search.stats(),
            facets: self.facets.stats(),
        }
    }

    /// Drops everything (an index swap: nothing cached reads the new
    /// index).
    pub fn clear(&self) {
        self.search.clear();
        self.facets.clear();
    }

    /// Drops entries past their ttl, and those of a generation other than
    /// `serving`'s current one (the lease sweeper, so an idle cache
    /// doesn't pin an old snapshot).
    pub fn sweep(&self, serving: &SearchIndex) {
        let current = Generation::of(&serving.searcher());
        self.search.sweep(&current);
        self.facets.sweep(&current);
    }
}

fn facet_key(spec: &FacetSpec<'_>, at: Snapshot, now: i64) -> Key {
    Key {
        what: format!(
            "{:?}|{:?}|{:?}|{}",
            spec.node, spec.filters, spec.params, spec.scope
        ),
        at,
        now: index::relative(spec.query).then_some(now),
    }
}

fn facets_entry(got: Facets, total: Total, key: Option<Key>) -> Computed<Arc<CachedFacets>> {
    let values: usize = got
        .terms
        .iter()
        .flat_map(|(_, t)| &t.values)
        .map(|v| v.value.len() + 48)
        .sum();
    let bins: usize = got
        .histograms
        .iter()
        .map(|(_, h)| h.bins.len() * size_of::<facets::Bin>() + 64)
        .sum();
    let bytes = ENTRY_BYTES
        + key.as_ref().map_or(0, |k| k.what.len() + k.at.bytes())
        + size_of::<Facets>()
        + values
        + bins;
    Computed {
        value: Arc::new(CachedFacets { facets: got, total }),
        key,
        bytes,
    }
}

/// The key to keep a result under: the snapshot it was actually computed
/// on, which may be newer than the lookup's. `None` (don't keep it) when
/// the file index moved while it ran, since its id sets then came from
/// either snapshot.
fn settled(index: &SearchIndex, looked_up: &Key, searcher: &Searcher, files: bool) -> Option<Key> {
    let files = if files {
        let now = Generation::of(&index.files_searcher());
        if looked_up.at.files.as_ref() != Some(&now) {
            return None;
        }
        Some(now)
    } else {
        None
    };
    Some(Key {
        what: looked_up.what.clone(),
        at: Snapshot {
            torrents: Generation::of(searcher),
            files,
        },
        now: looked_up.now,
    })
}

/// A computed value, the key to keep it under (`None`: don't) and its
/// estimated size.
struct Computed<V> {
    value: V,
    key: Option<Key>,
    bytes: usize,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One kind of entry: a map bounded by entries and bytes, least recently
/// used first out, with single flight for identical misses.
struct Lru<V> {
    config: CacheConfig,
    inner: Mutex<Inner<V>>,
    counters: Counters,
}

impl<V: Clone> std::fmt::Debug for Lru<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lru")
            .field("config", &self.config)
            .field("stats", &self.stats())
            .finish()
    }
}

#[derive(Default)]
struct Counters {
    hits: AtomicU64,
    joined: AtomicU64,
    misses: AtomicU64,
    evictions: AtomicU64,
    retired: AtomicU64,
    entries: AtomicU64,
    bytes: AtomicU64,
}

struct Inner<V> {
    map: HashMap<Key, Slot<V>>,
    flights: HashMap<Key, Arc<Flight<V>>>,
    bytes: usize,
    tick: u64,
    /// The torrent generation of the latest lookup, and where it stands:
    /// entries of any other can't be hit again, and are dropped when it
    /// moves on. A lookup on an older snapshot of the same index never
    /// moves it back.
    current: Option<(Seen, Generation)>,
}

struct Slot<V> {
    value: V,
    bytes: usize,
    used: u64,
    born: Instant,
}

/// An identical miss being computed: `None` until it's done, then the
/// result (`None` again inside when it failed, and each waiter computes
/// for itself).
struct Flight<V> {
    done: Mutex<Option<Option<V>>>,
    ready: Condvar,
}

impl<V: Clone> Flight<V> {
    fn finish(&self, v: Option<V>) {
        *lock(&self.done) = Some(v);
        self.ready.notify_all();
    }

    /// Waits for the result, giving up when `cancel` stops.
    fn wait(&self, cancel: &CancelToken) -> Result<Option<V>> {
        let mut done = lock(&self.done);
        loop {
            if let Some(v) = done.as_ref() {
                return Ok(v.clone());
            }
            cancel.check()?;
            done = self
                .ready
                .wait_timeout(done, Duration::from_millis(20))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// Resolves a flight as failed if its leader returns early or panics, so
/// no waiter waits for ever.
struct Leader<'a, V: Clone> {
    lru: &'a Lru<V>,
    key: Key,
    flight: Arc<Flight<V>>,
    resolved: bool,
}

impl<V: Clone> Drop for Leader<'_, V> {
    fn drop(&mut self) {
        if !self.resolved {
            self.lru.land(&self.key, &self.flight);
            self.flight.finish(None);
        }
    }
}

impl<V: Clone> Lru<V> {
    fn new(config: CacheConfig) -> Lru<V> {
        Lru {
            config,
            inner: Mutex::new(Inner {
                map: HashMap::new(),
                flights: HashMap::new(),
                bytes: 0,
                tick: 0,
                current: None,
            }),
            counters: Counters::default(),
        }
    }

    fn stats(&self) -> KindStats {
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let c = &self.counters;
        KindStats {
            hits: get(&c.hits),
            joined: get(&c.joined),
            misses: get(&c.misses),
            evictions: get(&c.evictions),
            retired: get(&c.retired),
            entries: get(&c.entries),
            bytes: get(&c.bytes),
        }
    }

    /// The value under `key`, or `compute`'s (given the lookup key; it
    /// says which key to keep its value under).
    /// `seen` places the key's snapshot among the index's; `cache: false`
    /// computes without looking up or keeping anything.
    fn get_or_compute(
        &self,
        key: Key,
        seen: Seen,
        cache: bool,
        cancel: &CancelToken,
        compute: impl FnOnce(&Key) -> Result<Computed<V>>,
    ) -> Result<(V, Outcome)> {
        if !self.config.enabled || !cache {
            self.counters.misses.fetch_add(1, Ordering::Relaxed);
            return Ok((compute(&key)?.value, Outcome::Miss));
        }
        // Dropped after the lock: a value may hold the last lease on a
        // retired index, and unmapping it shouldn't stall other lookups.
        let mut gone = Vec::new();
        let mut inner = lock(&self.inner);
        match &inner.current {
            // An older snapshot than one already seen: never move back
            // to it (that would retire the newer one's entries).
            Some((latest, _)) if seen.before(latest) => {
                drop(inner);
                self.counters.misses.fetch_add(1, Ordering::Relaxed);
                return Ok((compute(&key)?.value, Outcome::Miss));
            }
            Some((_, g)) if *g == key.at.torrents => {}
            _ => {
                gone.extend(self.retire(&mut inner, |k, _| k.at.torrents != key.at.torrents));
                inner.current = Some((seen, key.at.torrents.clone()));
            }
        }
        let ttl = self.config.ttl;
        if inner.map.get(&key).is_some_and(|s| s.born.elapsed() >= ttl) {
            gone.extend(self.retire(&mut inner, |k, _| *k == key));
        }
        inner.tick += 1;
        let tick = inner.tick;
        if let Some(slot) = inner.map.get_mut(&key) {
            slot.used = tick;
            let v = slot.value.clone();
            drop(inner);
            drop(gone);
            self.counters.hits.fetch_add(1, Ordering::Relaxed);
            return Ok((v, Outcome::Hit));
        }
        if let Some(flight) = inner.flights.get(&key).cloned() {
            drop(inner);
            drop(gone);
            if let Some(v) = flight.wait(cancel)? {
                self.counters.joined.fetch_add(1, Ordering::Relaxed);
                return Ok((v, Outcome::Joined));
            }
            // The leader failed (its client went away, say): compute
            // here, without a flight of our own.
            self.counters.misses.fetch_add(1, Ordering::Relaxed);
            let c = compute(&key)?;
            self.keep(c.key, c.value.clone(), c.bytes);
            return Ok((c.value, Outcome::Miss));
        }
        let flight = Arc::new(Flight {
            done: Mutex::new(None),
            ready: Condvar::new(),
        });
        inner.flights.insert(key.clone(), flight.clone());
        drop(inner);
        drop(gone);
        self.counters.misses.fetch_add(1, Ordering::Relaxed);
        let mut leader = Leader {
            lru: self,
            key,
            flight,
            resolved: false,
        };
        let c = compute(&leader.key)?;
        // Kept before the flight lands, so an identical lookup in between
        // finds one or the other and never computes again.
        self.keep(c.key, c.value.clone(), c.bytes);
        self.land(&leader.key, &leader.flight);
        leader.flight.finish(Some(c.value.clone()));
        leader.resolved = true;
        Ok((c.value, Outcome::Miss))
    }

    /// Removes `key`'s flight, if it's still `flight`.
    fn land(&self, key: &Key, flight: &Arc<Flight<V>>) {
        let mut inner = lock(&self.inner);
        if inner
            .flights
            .get(key)
            .is_some_and(|f| Arc::ptr_eq(f, flight))
        {
            inner.flights.remove(key);
        }
    }

    /// Keeps `value` under `key` when it's of the generation serving
    /// now, making room first.
    fn keep(&self, key: Option<Key>, value: V, bytes: usize) {
        let Some(key) = key else { return };
        if bytes > self.config.max_bytes {
            return;
        }
        let mut gone = Vec::new();
        let mut inner = lock(&self.inner);
        // A result of a snapshot older than the latest lookup's would
        // never be hit (or one newer: the next lookup will see it).
        if inner.current.as_ref().map(|(_, g)| g) != Some(&key.at.torrents) {
            return;
        }
        if let Some(old) = inner.map.remove(&key) {
            inner.bytes -= old.bytes;
            gone.push(old.value);
        }
        while !inner.map.is_empty()
            && (inner.map.len() >= self.config.max_entries.max(1)
                || inner.bytes + bytes > self.config.max_bytes)
        {
            let Some(lru) = inner
                .map
                .iter()
                .min_by_key(|(_, s)| s.used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some(s) = inner.map.remove(&lru) {
                inner.bytes -= s.bytes;
                gone.push(s.value);
                self.counters.evictions.fetch_add(1, Ordering::Relaxed);
            }
        }
        inner.tick += 1;
        let used = inner.tick;
        inner.bytes += bytes;
        inner.map.insert(
            key,
            Slot {
                value,
                bytes,
                used,
                born: Instant::now(),
            },
        );
        self.publish(&inner);
        drop(inner);
        drop(gone);
    }

    /// Removes the entries `pick` picks, for the caller to drop once it
    /// has let go of the lock.
    fn retire(&self, inner: &mut Inner<V>, pick: impl Fn(&Key, &Slot<V>) -> bool) -> Vec<V> {
        let keys: Vec<Key> = inner
            .map
            .iter()
            .filter(|(k, s)| pick(k, s))
            .map(|(k, _)| k.clone())
            .collect();
        let mut out = Vec::with_capacity(keys.len());
        for k in keys {
            if let Some(s) = inner.map.remove(&k) {
                inner.bytes -= s.bytes;
                out.push(s.value);
            }
        }
        self.counters
            .retired
            .fetch_add(out.len() as u64, Ordering::Relaxed);
        self.publish(inner);
        out
    }

    fn publish(&self, inner: &Inner<V>) {
        self.counters
            .entries
            .store(inner.map.len() as u64, Ordering::Relaxed);
        self.counters
            .bytes
            .store(inner.bytes as u64, Ordering::Relaxed);
    }

    fn sweep(&self, current: &Generation) {
        let ttl = self.config.ttl;
        let mut inner = lock(&self.inner);
        let gone = self.retire(&mut inner, |k, s| {
            k.at.torrents != *current || s.born.elapsed() >= ttl
        });
        drop(inner);
        drop(gone);
    }

    fn clear(&self) {
        let mut inner = lock(&self.inner);
        let gone = self.retire(&mut inner, |_, _| true);
        inner.current = None;
        drop(inner);
        drop(gone);
    }
}

#[cfg(test)]
mod tests;
