//! The unverified pipeline (architecture §18.5, §20.1, §20.5; plan
//! `docs/plan/2026-11-01-v06-sources.md` SRC-2;
//! `docs/decisions/2026-11-02-SRC-2.md`): what the writer does with a
//! source's claims before and after they're stored.
//!
//! - **Filter before storage.** Every claimed name and file path passes
//!   the full filter pipeline ([`judge`]: the locked floor, the core, your
//!   rules, packs and lists, with your classification rules) before
//!   `put_unverified` or `put_claims`. The store's held-hash guard is the
//!   second line, not the only one.
//! - **A claim on the floor or a CS pattern always carries a hold**
//!   (tombstone or quarantine, as §20 says for that rule), even when the
//!   rule's own action is weaker: such a claim is never stored, promoted
//!   or left only flagged. Any other hold (your drop or quarantine rule, a
//!   pack, a list) holds the hash of a key with no verified metadata, but
//!   never a stored torrent: claims are hints, and they never override
//!   verified metadata (§18.5). That claim is dropped instead.
//! - **Re-judging after storage** ([`Sweep`], [`refile_step`]): stored
//!   unverified items and torrent claims are judged again at start and
//!   whenever the filter engine's version changes, one bounded page per
//!   step through SRC-1's keyset pages, each written in its own short
//!   transaction that the store refuses when the rules moved on.
//! - **Verification** ([`Verify`]): behind `sources.verify_unverified`
//!   (default off), a cursor over the stored items hands a few keys per
//!   flush to intake, which fetches them through the shared pool. The
//!   store is the queue, so it holds nothing in memory.
//! - **Promotion** runs in the writer's commit (`writer::transaction`):
//!   any verified torrent whose hash an unverified item holds promotes it,
//!   keeping its id, whichever path fetched it.
//!
//! Nothing here logs or returns a claim's text: holds carry rule ids,
//! evidence codes and sealed names only.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use lodestar_classify::{Correction, FileInfo, Record, classify_corrected};
use lodestar_filter::reclassify::{adult_hold, judge_with_rules};
use lodestar_filter::{Action, Engine, Facts, Handle, Item, RuleRef, Sealer};
use lodestar_sources::Discovery;
use lodestar_store::claims::{self, Claims};
use lodestar_store::unverified::{
    MAX_PAGE, Network, NewUnverified, Put, RefiledClaims, RefiledUnverified, UnverifiedItem,
};
use lodestar_store::{Filtered, Held, Prepared, SearchableCodes, SourceId, Store, flags};

use crate::source_adult::AdultSources;
use crate::writer::Backoff;

/// The codes a promotion is counted under as newly searchable: Insights'
/// own (`lodestar::insights::CODES`, which this crate can't name).
pub const SEARCHABLE: SearchableCodes = SearchableCodes {
    stage: lodestar_sources::Stage::Searchable as u8,
    newly: crate::rollups::metric::NEWLY_SEARCHABLE,
    delay: crate::rollups::metric::TIME_TO_SEARCHABLE,
};

/// Rows one refile step judges: one store page at most.
pub const REFILE_PAGE: usize = MAX_PAGE;

/// Bytes of text one refile step judges at most ([`judged_len`]), past the
/// first row. A page's decoded claims are bounded (SRC-1's `PAGE_BYTES`),
/// but a details section is judged joined with the claimed and verified
/// names (ENR-9b), so small sections on torrents with long names could
/// otherwise make one step judge hundreds of megabytes while the writer
/// waits. A step that reaches it ends early, and the next goes on from the
/// last row it judged.
pub const REFILE_JUDGED_BYTES: usize = 32 << 20;

/// Keys the verification cursor hands to intake per flush (every 5 s by
/// default, so about 13 a second). The crawler commits thousands a
/// minute, so verification takes a few percent of the writer at most
/// (§18.4: importers and the like stay well under half its share).
pub const VERIFY_PER_FLUSH: usize = 64;

/// After the cursor reaches the last item, it starts over this much later
/// (seconds): a dead swarm is tried again a few times a day, not in a loop.
pub const VERIFY_ROUND_SECS: i64 = 6 * 3600;

/// The pipeline's own strategy code for a verification fetch, beside the
/// DHT's (`dht::strategy`).
pub const VERIFY_STRATEGY: u8 = 201;

/// What intake may hold in flight to the writer, in KiB of claims
/// ([`claims_kib`]): a byte bound on the claims channel, since one
/// discovery's claims run to a megabyte and feeds are untrusted.
pub const CLAIMS_BUDGET_KIB: u32 = 64 * 1024;

/// Claims one writer step takes at most, in KiB: a step's judging and
/// storing stay short beside the crawler's commits however large the
/// claims.
pub const CLAIMS_STEP_KIB: u32 = 8 * 1024;

/// A discovery with claims on its way to the writer, holding its share
/// of [`CLAIMS_BUDGET_KIB`] until the writer has stored or dropped it.
pub type Claimed = (Discovery, tokio::sync::OwnedSemaphorePermit);

/// The longest verified name a details section is joined with at ingest,
/// for [`claims_kib`]'s charge: names are capped at 4,096 raw bytes, but a
/// decoded one can reach about 12 KiB (legacy encodings, runner #557).
pub const JUDGED_NAME_MAX: usize = 12 * 1024;

/// A discovery's claims in KiB, counting each file's entry as well as its
/// text (an empty path still takes memory), at least 1 and at most
/// [`CLAIMS_STEP_KIB`].
///
/// A details section is charged at what [`judge_in`] judges for it
/// ([`details_judged_len`]), joined with the claimed name and a verified
/// name of [`JUDGED_NAME_MAX`] bytes, since intake can't know the torrent
/// it lands on. So the writer's [`CLAIMS_STEP_KIB`] per step also bounds
/// the text one ingest step judges (ENR-10, runner #557): about 8 MiB,
/// plus one discovery.
pub fn claims_kib(d: &Discovery) -> u32 {
    let Some(c) = d.found.claims.as_deref() else {
        return 1;
    };
    let judged = c.details.as_deref().map_or(0, |s| {
        let names = distinct_names(&[c.name.as_deref()]);
        judged_len_of(s, names.iter().map(|n| n.len()).chain([JUDGED_NAME_MAX]))
    });
    let bytes = c.text_len()
        + judged
        + c.files.len() * std::mem::size_of::<lodestar_sources::ClaimedFile>()
        + c.trackers.len() * std::mem::size_of::<String>()
        + c.ids.len() * std::mem::size_of::<lodestar_sources::ExternalId>();
    u32::try_from(bytes / 1024 + 1)
        .unwrap_or(u32::MAX)
        .min(CLAIMS_STEP_KIB)
}

/// Where the writer publishes the ids it promoted under their own id,
/// after each commit (SRC-2b's percolator pickup hooks in here). The sink
/// runs on the writer's task and must not block: hand the ids off and
/// return. Nothing is published while no sink is set.
#[derive(Default)]
pub struct PromotedHook(std::sync::RwLock<Option<PromotedSink>>);

/// A [`PromotedHook`]'s sink.
type PromotedSink = Box<dyn Fn(&[i64]) + Send + Sync>;

impl std::fmt::Debug for PromotedHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let set = self.0.read().map(|s| s.is_some()).unwrap_or(false);
        f.debug_struct("PromotedHook").field("set", &set).finish()
    }
}

impl PromotedHook {
    /// Sets the sink, replacing any earlier one.
    pub fn set(&self, sink: impl Fn(&[i64]) + Send + Sync + 'static) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(sink));
    }

    pub(crate) fn publish(&self, ids: &[i64]) {
        if ids.is_empty() {
            return;
        }
        if let Some(sink) = self.0.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
            sink(ids);
        }
    }
}

/// Counters for metrics and the status page.
#[derive(Debug, Default)]
pub struct UnverifiedStats {
    /// Discoveries with claims the writer judged.
    pub received: AtomicU64,
    /// New unverified items stored.
    pub stored: AtomicU64,
    /// Items that already existed and gained the source.
    pub existing: AtomicU64,
    /// Claims stored on a torrent that already held the key.
    pub on_torrent: AtomicU64,
    /// Keys whose claims a rule held: tombstoned or quarantined, never
    /// stored.
    pub held: AtomicU64,
    /// Claims a rule matched on a stored torrent, dropped without holding
    /// it (not a floor or core hit: claims never override verified
    /// metadata).
    pub dropped: AtomicU64,
    /// Discoveries the store refused: the key is held, or the source is
    /// disabled or purged.
    pub refused: AtomicU64,
    /// Claims outside the limits (the emitter clamps them, so none).
    pub invalid: AtomicU64,
    /// Unverified items promoted under their own id.
    pub promoted: AtomicU64,
    /// Unverified items merged into a torrent stored under the hash.
    pub merged: AtomicU64,
    /// Promotions whose claimed file list contradicts the verified one.
    pub mismatches: AtomicU64,
    /// Stored items and claims the refile sweep removed.
    pub refiled: AtomicU64,
    /// Refile sweeps finished.
    pub sweeps: AtomicU64,
    /// Keys the verification cursor handed to intake.
    pub verify_sent: AtomicU64,
    /// Store failures while classifying enrichment matches again or marking
    /// them for the backfill (ENR-1c), one per failed call: logged, and the
    /// matches left for the next pass.
    pub enrich_failed: AtomicU64,
    /// The promoted ids, published after each commit.
    pub promoted_hook: PromotedHook,
}

impl UnverifiedStats {
    fn add(counter: &AtomicU64, n: usize) {
        counter.fetch_add(n as u64, Ordering::Relaxed);
    }
}

/// The form a claimed tracker is stored in, or `None` when it's dropped:
/// the shared guard's canonical form ([`public_tracker_url`]), the address
/// it judged. That's an `http`, `https` or `udp` URL on a public host,
/// with no user name, the host lower-cased and made only of letters,
/// digits, `-` and `.` (or an IP literal), and a `udp` host rebuilt as
/// judged; never a local name, a single label, or a loopback, private or
/// link-local address (X-claim-trackers). The raw text must also be
/// `scheme://` and a host, within the claims' tracker limit, and free of
/// whitespace and control characters: the `url` crate would repair those
/// (`http:1.1` reads as `http://1.0.0.1/`, a tab is removed) rather than
/// refuse them. The canonical form is held to the same limit, since it can
/// be longer (IDNA, percent-encoding). Tracker URLs aren't names, so they
/// aren't judged as text (runner, #383).
///
/// The form is a fixed point: it maps to itself (X-claim-store).
///
/// [`public_tracker_url`]: lodestar_integrations::net::public_tracker_url
pub fn tracker_url(t: &str) -> Option<String> {
    let (_, rest) = t.split_once("://")?;
    if rest.is_empty()
        || rest.starts_with('/')
        || t.len() > claims::MAX_TRACKER
        || t.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    lodestar_integrations::net::public_tracker_url(t).filter(|k| k.len() <= claims::MAX_TRACKER)
}

/// The trackers stored for a claim: each in its [`tracker_url`] form, the
/// dropped ones left out, once each after canonicalising (the first
/// spelling's place), in the claim's order, at most the claims' limit, and
/// at most `room` bytes in all. The canonical form can be longer than the
/// raw text the emitter's clamp measured, so a tracker that would take the
/// claims over [`claims::MAX_TOTAL`] is dropped rather than the store
/// refusing the whole claim set; later, shorter ones may still fit.
pub fn stored_trackers(raw: &[String], room: usize) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut left = room;
    raw.iter()
        .filter_map(|t| tracker_url(t))
        .filter(|t| seen.insert(t.clone()))
        .filter(|t| {
            let fits = t.len() <= left;
            if fits {
                left -= t.len();
            }
            fits
        })
        .take(claims::MAX_TRACKERS)
        .collect()
}

/// The store's claims for a discovery: the source's claims and
/// provenance, already clamped by the emitter, with no `external_ref` and
/// no trackers for a `private_source` find, and the details section only
/// where the store takes it ([`attach_details`]). `None` when it has none.
///
/// Nothing here is judged: every caller judges what this returns
/// ([`judge`]) before it's stored, details section included.
pub fn store_claims(d: &Discovery) -> Option<Claims> {
    let c = d.found.claims.as_deref()?;
    let p = d.found.provenance.as_deref();
    let mut out = Claims {
        name: c.name.clone(),
        size: c.size,
        files: c
            .files
            .iter()
            .map(|f| claims::ClaimedFile {
                path: f.path.clone(),
                size: f.size,
            })
            .collect(),
        trackers: Vec::new(),
        ids: c
            .ids
            .iter()
            .map(|i| claims::ExternalId {
                scheme: i.scheme.clone(),
                value: i.value.clone(),
            })
            .collect(),
        description: c.description.clone(),
        publisher: c.publisher.clone(),
        provenance: claims::Provenance {
            // Never for a private find: a private tracker's reference can
            // be a passkey URL. Plugins drop it too; this enforces it in
            // one place (runner, security pre-audit L3).
            external_ref: p
                .filter(|_| !d.found.private_source)
                .and_then(|p| p.external_ref.clone()),
            published_at: p.and_then(|p| p.published_at),
            confidence: match p.map(|p| p.confidence) {
                Some(lodestar_sources::Confidence::High) => claims::Confidence::High,
                Some(lodestar_sources::Confidence::Medium) => claims::Confidence::Medium,
                _ => claims::Confidence::Low,
            },
        },
        // Judged, as extra evidence, before it's stored ([`judge_in`]).
        adult: c.adult.clone(),
        // Below, once the rest is in place.
        details: None,
    };
    // Judged before it's stored, each text as a name ([`details_texts`]).
    attach_details(c, &mut out);
    // The trackers last, in the room the rest leaves under the total.
    // None for a private find: a private tracker's announce URL can carry
    // a passkey, which the host guard doesn't look for (runner, L3).
    if !d.found.private_source {
        let room = claims::MAX_TOTAL.saturating_sub(out.text_len());
        out.trackers = stored_trackers(&c.trackers, room);
    }
    Some(out)
}

/// Puts the details section (ENR-9) in `out`, the rest of a discovery's
/// claims: the source's section as its clamp left it, or none when it
/// can't be stored beside them. Whatever a source sends,
/// the section either goes in with claims the store accepts or is
/// dropped, so a feed can't make [`Claims::check`] refuse its other
/// claims by adding one.
///
/// - **Never beside an adult section** (one section a row, ENR-9). The
///   clamp drops it there already; this doesn't rely on that.
/// - **Never beside a description or publisher**, which the store refuses
///   there (`details claims`): either could name a person. The clamp drops
///   those when a section survives, so on a clamped input this keeps the
///   section; on any other it keeps what v1 stored.
/// - **Only when the claims, section included, pass [`Claims::check`]**
///   (its limits, `enrichment ids`, the total). Checked before the
///   trackers go in: they're fitted to the room the section leaves.
///
/// `private_source` changes nothing here (`2026-11-06-ENR-9b.md`): no field
/// is a credential or a link, unlike `external_ref` and the trackers, and
/// the row carries the private flag that keeps it local, as it does for
/// the claimed name and files.
fn attach_details(c: &lodestar_sources::Claims, out: &mut Claims) {
    debug_assert!(out.trackers.is_empty(), "the trackers go in after");
    out.details = None;
    let Some(d) = &c.details else {
        return;
    };
    if out.adult.is_some() {
        return;
    }
    if out.description.is_some() || out.publisher.is_some() {
        return;
    }
    out.details = Some(d.clone());
    if out.check().is_err() {
        out.details = None;
    }
}

/// A claim a rule holds back. No name in the clear: only the sealed one,
/// for quarantine.
#[derive(Clone)]
pub struct Hold {
    pub hashes: Vec<[u8; 20]>,
    pub rule: String,
    pub action: u8,
    pub held: Held,
    pub sealed: Option<Vec<u8>>,
    pub evidence: Vec<u8>,
    pub adult_hold: bool,
    /// A floor or core (CS) rule decided: the hash is held even when a
    /// torrent is stored under it.
    pub core: bool,
}

// No name, sealed or not: it may be one a rule holds.
impl std::fmt::Debug for Hold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hold")
            .field("rule", &self.rule)
            .field("held", &self.held)
            .field("core", &self.core)
            .finish_non_exhaustive()
    }
}

impl Hold {
    /// The decision as ingest writes one: a filter hit and the hold.
    pub fn filtered(&self, now: i64, quarantine_secs: i64) -> Filtered<'_> {
        Filtered {
            hashes: &self.hashes,
            rule: &self.rule,
            action: self.action,
            held: Some(self.held),
            sealed: self.sealed.as_deref(),
            evidence: &self.evidence,
            at: now,
            expires_at: now.saturating_add(quarantine_secs),
            adult_hold: self.adult_hold,
        }
    }
}

/// A claimed path as the classifier and filter take it: its segments,
/// split on either separator. Never empty.
fn segments(path: &str) -> Vec<String> {
    let mut out: Vec<String> = path
        .split(['/', '\\'])
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if out.is_empty() {
        out.push(path.to_owned());
    }
    out
}

/// The claimed files as the classifier and filter take them. A claimed
/// size the source left out counts as 0.
pub fn claimed_files(c: &Claims) -> Vec<FileInfo> {
    c.files
        .iter()
        .map(|f| FileInfo::new(segments(&f.path), f.size.unwrap_or(0)))
        .collect()
}

/// Judges one key's claims through the full filter pipeline, as ingest
/// judges a fetched torrent: its hash, then the claimed name and every
/// claimed file path together, classified as they claim, with your
/// classification rules; then the claimed description and the publisher,
/// each judged as a name the same way (runner, #383: every claim text that
/// is stored passes the filter); then the details section's texts
/// ([`details_texts`], ENR-9b), each judged as a name the same way. Tracker
/// URLs aren't names: they're validated as URLs instead ([`store_claims`]).
///
/// `Some` when the claims must not be stored:
/// - a floor or core (CS) rule matched any of the texts: [`Hold::core`],
///   a hold always, a quarantine when the rule's own action is weaker
///   than one (the rule for SRC-2 in `2026-11-02-SRC-1.md`). A core hit
///   on any text wins over another rule's hit on another;
/// - any other rule that drops or quarantines: not [`Hold::core`]. The
///   callers drop the source's claims and hold nothing (runner, #383): a
///   feed must not be able to get an unrelated torrent refused by
///   claiming its hash under a name your rule drops.
///
/// Weaker decisions (flag, hide, demote) and shadow-mode rules don't stop
/// storage: an unverified item isn't searchable, and its verified metadata
/// is judged again when it's promoted.
///
/// `adult`: an always-adult source made these claims, or found the key
/// ([`crate::source_adult`]). Each text is then judged as adult as well,
/// as an Is adult correction makes it, and the stricter verdict stands:
/// the Adult-only core (CS-5a and the rest) checks it whatever its name.
pub fn judge(
    engine: &Engine,
    key: [u8; 20],
    c: &Claims,
    sealer: Option<&Sealer>,
    adult: bool,
    now: i64,
) -> Option<Hold> {
    judge_in(engine, key, c, None, sealer, adult, now)
}

/// [`judge`], and the claims' adult-enrichment section (ENR-0) judged as
/// extra evidence (ENR-1, plan Q3): each of its texts (the scene title,
/// the studio, each performer), all of them together, and, when
/// `context` gives the verified name of the torrent the claims are on,
/// that name with them. Every one is judged as adult, since only Adult
/// items are looked up (Q2), so the Adult-only core's thresholds apply.
///
/// `context` is also joined with the details section's texts, as the
/// claimed name is ([`details_texts`]); those are judged as adult only
/// when `adult` says so, like the claimed name.
///
/// **Stricter only.** The section's texts are judged as extra items: a
/// hit adds a hold (a core one holds the hash, as for any claim), and
/// nothing they hold can cancel a decision on another text. They're never
/// a counter-signal: no exception or rule judging the torrent's own name
/// sees them.
pub fn judge_in(
    engine: &Engine,
    key: [u8; 20],
    c: &Claims,
    context: Option<&str>,
    sealer: Option<&Sealer>,
    adult: bool,
    now: i64,
) -> Option<Hold> {
    let hashes = vec![key];
    let files = claimed_files(c);
    let size = c
        .size
        .unwrap_or_else(|| files.iter().map(|f| f.size).fold(0, u64::saturating_add));
    let mut found: Option<Hold> = None;
    let texts = [
        (c.name.as_deref().unwrap_or(""), true),
        (c.description.as_deref().unwrap_or(""), false),
        (c.publisher.as_deref().unwrap_or(""), false),
    ];
    for (i, (text, with_files)) in texts.into_iter().enumerate() {
        // The name is judged even when empty: the files and the hash.
        if i > 0 && text.is_empty() {
            continue;
        }
        let files: &[FileInfo] = if with_files { &files } else { &[] };
        let Some(h) = judge_text(engine, &hashes, text, files, size, sealer, adult, now) else {
            continue;
        };
        if h.core {
            return Some(h);
        }
        found.get_or_insert(h);
    }
    // The details section's texts, as names, the same way (ENR-9b).
    for text in c
        .details
        .as_deref()
        .map(|d| details_texts(d, &[c.name.as_deref(), context]))
        .unwrap_or_default()
    {
        let Some(h) = judge_text(engine, &hashes, &text, &[], size, sealer, adult, now) else {
            continue;
        };
        if h.core {
            return Some(h);
        }
        found.get_or_insert(h);
    }
    for text in c
        .adult
        .as_ref()
        .map(|a| enrichment_texts(a, context))
        .unwrap_or_default()
    {
        let Some(h) = judge_text(engine, &hashes, &text, &[], 0, sealer, true, now) else {
            continue;
        };
        if h.core {
            return Some(h);
        }
        found.get_or_insert(h);
    }
    found
}

/// The texts of an adult-enrichment section that [`judge_in`] judges:
/// each one alone (the scene title, the studio, each performer, and the
/// scene id with its `-`s read as spaces, since a slug can carry the
/// title), then all of them together, then `context` (the torrent's
/// verified name) with each one and with all of them. Empty texts are
/// left out.
pub fn enrichment_texts(a: &claims::AdultEnrichment, context: Option<&str>) -> Vec<String> {
    let slug = a.scene_id.replace('-', " ");
    let mut alone: Vec<&str> = Vec::with_capacity(3 + a.performers.len());
    alone.extend(a.title.as_deref());
    alone.extend(a.studio.as_deref());
    alone.extend(a.performers.iter().map(String::as_str));
    alone.push(slug.trim());
    alone.retain(|t| !t.is_empty());
    if alone.is_empty() {
        return Vec::new();
    }
    let together = alone.join(" ");
    let mut out: Vec<String> = alone.iter().map(|t| (*t).to_owned()).collect();
    if alone.len() > 1 {
        out.push(together.clone());
    }
    if let Some(name) = context.filter(|n| !n.is_empty()) {
        out.extend(alone.iter().map(|t| format!("{name} {t}")));
        if alone.len() > 1 {
            out.push(format!("{name} {together}"));
        }
    }
    out
}

/// The texts of a details section (ENR-9) that [`judge_in`] judges, each
/// as a name: the canonical title, each alternative title, each distinct
/// language tag of an alternative title with its `-`s read as spaces (a
/// tag's subtags can carry short words, as a scene id's slug can), the
/// source category, each genre and each newsgroup alone; then, when
/// there's more than one, all of them together; then each of `names` (the
/// claimed name, and the verified name of the torrent the claims are on)
/// with each text and with all of them, since a feed's category or a
/// plugin's genre describes that very item. Each pair is its own text, as
/// for the adult section ([`enrichment_texts`]): the core's pair rules look
/// within a few words, so joining every text to a name in one string would
/// let padding in one field hide another. Empty texts and names, and a name
/// given twice, are left out.
///
/// **Stricter only**, as for the adult section: the joined texts can only
/// add a hold. Every text is still judged alone.
///
/// **Not judged, because they aren't text:** the posting time, the year
/// and the match confidence are numbers. The external ids beside the
/// section are scheme and value pairs the store checks (`id_ok`), as for
/// every claim.
///
/// [`details_judged_len`] gives these texts' total length without building
/// them.
pub fn details_texts(d: &claims::Details, names: &[Option<&str>]) -> Vec<String> {
    let alone = details_alone(d);
    if alone.is_empty() {
        return Vec::new();
    }
    let together = alone.join(" ");
    let mut out: Vec<String> = alone.iter().map(|t| t.to_string()).collect();
    if alone.len() > 1 {
        out.push(together.clone());
    }
    for name in distinct_names(names) {
        out.extend(alone.iter().map(|t| format!("{name} {t}")));
        if alone.len() > 1 {
            out.push(format!("{name} {together}"));
        }
    }
    out
}

/// The bytes [`details_texts`] judges for `d` with `names`, counted
/// without building them, for the refile's per-step budget
/// ([`REFILE_JUDGED_BYTES`]).
pub fn details_judged_len(d: &claims::Details, names: &[Option<&str>]) -> usize {
    judged_len_of(d, distinct_names(names).iter().map(|n| n.len()))
}

/// [`details_judged_len`] from the lengths of the distinct names.
fn judged_len_of(d: &claims::Details, names: impl Iterator<Item = usize>) -> usize {
    let alone = details_alone(d);
    let (k, l) = (alone.len(), alone.iter().map(|t| t.len()).sum::<usize>());
    if k == 0 {
        return 0;
    }
    // Alone, then together (`k - 1` spaces).
    let together = if k > 1 { l + k - 1 } else { 0 };
    let mut total = l + together;
    for name in names {
        let n = name + 1;
        total += k * n + l;
        if k > 1 {
            total += n + together;
        }
    }
    total
}

/// The section's texts judged alone, in [`details_texts`]' order.
fn details_alone(d: &claims::Details) -> Vec<std::borrow::Cow<'_, str>> {
    use std::borrow::Cow;
    let mut alone: Vec<Cow<'_, str>> =
        Vec::with_capacity(2 + 2 * d.alt_titles.len() + d.genres.len() + d.newsgroups.len());
    alone.extend(d.canonical_title.as_deref().map(Cow::from));
    alone.extend(d.alt_titles.iter().map(|a| Cow::from(a.title.as_str())));
    let mut tags: Vec<&str> = Vec::new();
    for a in &d.alt_titles {
        if !tags.contains(&a.language.as_str()) {
            tags.push(&a.language);
        }
    }
    alone.extend(tags.into_iter().map(|t| Cow::from(t.replace('-', " "))));
    alone.extend(d.source_category.as_deref().map(Cow::from));
    alone.extend(d.genres.iter().map(|g| Cow::from(g.as_str())));
    alone.extend(d.newsgroups.iter().map(|g| Cow::from(g.as_str())));
    alone.retain(|t| !t.trim().is_empty());
    alone
}

/// `names` without the empty ones and without repeats, in order.
fn distinct_names<'a>(names: &[Option<&'a str>]) -> Vec<&'a str> {
    let mut out: Vec<&str> = Vec::with_capacity(names.len());
    for name in names.iter().flatten() {
        if !name.trim().is_empty() && !out.contains(name) {
            out.push(name);
        }
    }
    out
}

/// About the bytes of text [`judge_in`] judges for `c` with `context`:
/// its text once ([`Claims::text_len`]), and its details section's texts
/// exactly ([`details_judged_len`]). The refile's budget
/// ([`REFILE_JUDGED_BYTES`]) counts this.
pub fn judged_len(c: &Claims, context: Option<&str>) -> usize {
    c.text_len().saturating_add(
        c.details
            .as_deref()
            .map_or(0, |d| details_judged_len(d, &[c.name.as_deref(), context])),
    )
}

/// [`judge`] for one text, as a torrent's name, with `files`.
#[allow(clippy::too_many_arguments)]
fn judge_text(
    engine: &Engine,
    hashes: &[[u8; 20]],
    name: &str,
    files: &[FileInfo],
    size: u64,
    sealer: Option<&Sealer>,
    adult: bool,
    now: i64,
) -> Option<Hold> {
    let class = classify_corrected(name, files, Correction::default());
    let facts = Facts {
        class: &class,
        size,
        piece_len: 0,
        flags: 0,
        discovered_at: now,
        now,
    };
    let item = Item {
        hashes,
        name,
        files,
        content_type: class.content_type,
    };
    let (verdict, fix) = if adult {
        // A hash on the floor's or a dropping list first, as
        // `judge_with_rules` does; then both readings.
        match engine.blocked_hash(hashes) {
            Some(d) => (
                lodestar_filter::Verdict {
                    decision: Some(d),
                    shadow: Vec::new(),
                },
                None,
            ),
            None => crate::source_adult::judge(engine, &item, &facts, now),
        }
    } else {
        judge_with_rules(engine, &item, &class, &facts, now)
    };
    let d = verdict.decision?;
    let core = matches!(d.rule, RuleRef::Core(_));
    let action = match d.action {
        Action::Drop => Action::Drop,
        Action::Quarantine => Action::Quarantine,
        _ if core => Action::Quarantine,
        _ => return None,
    };
    let held = if action == Action::Drop {
        Held::Tombstone
    } else {
        Held::Quarantine
    };
    let sealed = (core && held == Held::Quarantine)
        .then(|| sealer.and_then(|s| s.seal(name).ok()))
        .flatten();
    Some(Hold {
        hashes: hashes.to_vec(),
        rule: d.rule.id(),
        action: action.code(),
        held,
        sealed,
        evidence: d.evidence_codes(),
        // Held by the Adult-only core only because your rule (or its
        // source) made it adult: a tombstone when the quarantine ends
        // (§20.9).
        adult_hold: fix == Some(true) && adult_hold(&d),
        core,
    })
}

/// What the writer needs to judge and hold.
#[derive(Clone, Copy, Default)]
pub struct Ctx<'a> {
    pub sealer: Option<&'a Sealer>,
    pub quarantine_secs: i64,
    /// The sources whose every find is adult ([`crate::source_adult`]).
    pub adult: AdultSources,
    /// The adult-enrichment sources: only the listed ones (the
    /// `AdultEnrichSource` plugins the host runs, ENR-1c) may attach an
    /// adult-enrichment section. Any other source's section is dropped and
    /// its other claims take the usual path, so an always-adult source can't
    /// use one to skip its mask. A section-only claim is a bare sighting,
    /// except from an unlisted enrichment source, whose answers aren't
    /// finds: refused. Empty (the default): no source may.
    pub enrichers: crate::adult_enrich::EnrichSources,
    /// The live filter: an enrichment match is classified under its
    /// classification rules read *after* the generation, never under the
    /// engine the batch was judged with (ENR-1b). Without it, matches are
    /// left for the backfill.
    pub filter: Option<&'a Handle>,
    /// Raised when matches were left for the backfill, so it runs.
    pub reenrich: Option<&'a AtomicBool>,
}

impl std::fmt::Debug for Ctx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ctx")
            .field("sealer", &self.sealer.is_some())
            .field("quarantine_secs", &self.quarantine_secs)
            .field("adult", &self.adult)
            .field("enrichers", &self.enrichers)
            .field("filter", &self.filter.is_some())
            .finish_non_exhaustive()
    }
}

/// What one ingest batch did, for the writer to account.
#[derive(Debug, Default)]
pub struct Ingested {
    /// Torrents stored under a key the item's own claims now hold (crawled
    /// before the claims arrived): deleted, and logged for the index.
    pub deleted: Vec<i64>,
    /// Stored torrents that gained claims: `(id, at, source, strategy)`.
    /// The writer accounts each as a rediscovery, as a bare sighting of a
    /// stored key is.
    pub touched: Vec<(i64, i64, SourceId, u8)>,
    /// Stored torrents an always-adult source has just claimed: the writer
    /// classifies them as adult (`enrich::adult_by_source`).
    pub adult: Vec<i64>,
    /// Stored torrents that gained an adult-enrichment match (ENR-1),
    /// classified again with it here ([`crate::enrich::apply_matches`]).
    pub matched: Vec<i64>,
    /// Stored torrents that gained an identification (ENR-10,
    /// [`is_identification`]): marked for the backfill, which resolves
    /// their effective title with it and logs them for the index.
    pub identified: Vec<i64>,
}

/// Whether `d` is a general enrichment answer (ENR-10,
/// [`crate::identify`]): a details section with nothing but ids beside
/// it, from a registered `Kind::Enrichment` source that isn't a listed
/// adult enricher. Like an adult match, it's about a stored torrent the
/// runtime asked about, not a find: [`ingest`] stores it on that torrent
/// only, never as an unverified item, a sighting or a rediscovery.
pub fn is_identification(d: &Discovery, enrichers: crate::adult_enrich::EnrichSources) -> bool {
    enrichers.of_kind(d.source)
        && !enrichers.has(d.source)
        && d.found.claims.as_deref().is_some_and(|c| {
            c.details.is_some()
                && c.name.is_none()
                && c.size.is_none()
                && c.files.is_empty()
                && c.trackers.is_empty()
                && c.description.is_none()
                && c.publisher.is_none()
                && c.adult.is_none()
        })
}

/// Judges a batch of discoveries with claims and stores what the rules
/// let through: holds first (calling `mark` with their hashes just before
/// the write, `2026-10-31-B-held-precommit.md`), then the items, one
/// `put_unverified` per source so a source disabled meanwhile is refused
/// alone.
pub fn ingest(
    store: &mut Store,
    engine: &Engine,
    ctx: Ctx<'_>,
    found: &[Discovery],
    stats: &UnverifiedStats,
    now: i64,
    mark: &dyn Fn(&Store, &[[u8; 20]]),
) -> Result<Ingested, lodestar_store::Error> {
    let mut out = Ingested::default();
    UnverifiedStats::add(&stats.received, found.len());
    // `None`: a bare sighting (a section, all a source that isn't an
    // enrichment source claimed, dropped).
    let mut keep: Vec<(&Discovery, Option<Claims>)> = Vec::with_capacity(found.len());
    // Enrichment matches, with the stored torrent they're on.
    // Enrichment matches and identifications, with the stored torrent
    // they're on; `true` for an identification.
    let mut matches: Vec<(&Discovery, Claims, i64, bool)> = Vec::new();
    let mut holds: Vec<Hold> = Vec::new();
    for d in found {
        let Some(mut c) = store_claims(d) else {
            continue;
        };
        if c.check().is_err() {
            UnverifiedStats::add(&stats.invalid, 1);
            continue;
        }
        // Only an enrichment source may attach an adult section (ENR-1b).
        // Any other source's claims go on without it, by the usual path.
        if c.adult.is_some() && !ctx.enrichers.has(d.source) {
            c.adult = None;
            if c == Claims::default() {
                // An enrichment source that isn't listed (the torrent cache,
                // or a plugin the host forgot to list): its answers aren't
                // finds, so nothing is stored and it gains no bit (#502).
                if ctx.enrichers.of_kind(d.source) {
                    UnverifiedStats::add(&stats.refused, 1);
                    continue;
                }
                // Nothing else was claimed: what's left is a bare sighting
                // of the key, stored and counted as one (ENR-1c). On a
                // stored torrent the source gains its bit (so an
                // always-adult source's mask applies) and it's a
                // rediscovery; a key nothing stores is refused, since the
                // writer doesn't fetch.
                keep.push((d, None));
                continue;
            }
        }
        let adult = ctx.adult.has(d.source);
        // An identification (ENR-10) takes the same path as an adult match,
        // and is refused on a private torrent too: the runtime never asks
        // about one, so an answer for it is stale or forged.
        let identification = c.adult.is_none() && is_identification(d, ctx.enrichers);
        // An adult-enrichment match (ENR-1) is only ever about a stored,
        // verified torrent, looked up by the enrichment runtime: one for
        // any other key, or through a private source, is refused.
        let on = if c.adult.is_some() || identification {
            let id = if d.found.private_source {
                None
            } else {
                store.find(&d.found.key)?
            };
            let row = match id {
                Some(id) => store.torrent(id)?,
                None => None,
            };
            let Some(row) = row.filter(|r| !identification || r.flags & flags::PRIVACY == 0) else {
                UnverifiedStats::add(&stats.refused, 1);
                continue;
            };
            Some((row.id, Some(row.name)))
        } else {
            None
        };
        // A details section is judged with the verified name of a torrent
        // stored under the key too, as an adult section is (ENR-9b).
        let verified = match (&on, &c.details) {
            (None, Some(_)) => match store.find(&d.found.key)? {
                Some(id) => store.torrent(id)?.map(|r| r.name),
                None => None,
            },
            _ => None,
        };
        let context = on
            .as_ref()
            .and_then(|(_, name)| name.as_deref())
            .or(verified.as_deref());
        let Some(mut h) = judge_in(engine, d.found.key.0, &c, context, ctx.sealer, adult, now)
        else {
            match on {
                Some((id, _)) => matches.push((d, c, id, identification)),
                None => keep.push((d, Some(c))),
            }
            continue;
        };
        if !h.core {
            // Your rule, a pack or a list: this source's claims are
            // dropped and nothing is held (runner, #383). A torrent under
            // the hash is judged on its own verified metadata, whichever
            // arrives first.
            drop_source_claims(store, d.found.key.0, d.source)?;
            UnverifiedStats::add(&stats.dropped, 1);
            continue;
        }
        // The floor or the core on a stored torrent: every one of its
        // swarm hashes is held (a hybrid's other key too), as the refile
        // does.
        if let Some(id) = store.find(&d.found.key)?
            && let Some(all) = store.torrent_swarm_hashes(id)?
        {
            for x in all {
                if !h.hashes.contains(&x) {
                    h.hashes.push(x);
                }
            }
        }
        holds.push(h);
    }
    if !holds.is_empty() {
        let hashes: Vec<[u8; 20]> = holds.iter().flat_map(|h| h.hashes.clone()).collect();
        mark(store, &hashes);
        let filtered: Vec<Filtered<'_>> = holds
            .iter()
            .map(|h| h.filtered(now, ctx.quarantine_secs))
            .collect();
        // One transaction: the holds, and any torrent stored under their
        // hashes deleted, so a failure can't leave one stored under a held
        // hash. Only floor and core holds reach a stored torrent.
        out.deleted = store.hold_claims(&filtered)?;
        UnverifiedStats::add(&stats.held, holds.len());
    }
    let mut sources: Vec<SourceId> = keep.iter().map(|(d, _)| d.source).collect();
    sources.sort_unstable();
    sources.dedup();
    for source in sources {
        let group: Vec<&(&Discovery, Option<Claims>)> =
            keep.iter().filter(|(d, _)| d.source == source).collect();
        let items: Vec<NewUnverified<'_>> = group
            .iter()
            .map(|(d, c)| NewUnverified {
                network: Network::Bt,
                key: &d.found.key.0,
                source,
                private_source: d.found.private_source,
                claims: c.as_ref(),
                at: d.at,
            })
            .collect();
        let put = match store.put_unverified(&items) {
            Ok(put) => put,
            // Disabled or purged meanwhile: what it found is dropped, so a
            // purge can't be undone by a poll in flight (SRC-1).
            Err(lodestar_store::Error::Invalid(_)) => {
                UnverifiedStats::add(&stats.refused, items.len());
                continue;
            }
            Err(e) => return Err(e),
        };
        for ((d, _), p) in group.iter().zip(put) {
            let counter = match p {
                Put::New(_) => &stats.stored,
                Put::Existing(_) => &stats.existing,
                Put::Torrent(id) => {
                    out.touched.push((id, d.at, d.source, d.found.strategy));
                    if ctx.adult.has(d.source) {
                        out.adult.push(id);
                    }
                    &stats.on_torrent
                }
                Put::Held | Put::Bare => &stats.refused,
            };
            UnverifiedStats::add(counter, 1);
        }
    }
    // Enrichment matches go on the torrent as the source's claims, never
    // as an unverified item and never as a sighting: the source didn't
    // find the key, the runtime asked about it.
    for (d, c, id, identification) in matches {
        let counter = match store.put_claims(id, d.source, &c, false) {
            Ok(true) => {
                let list = if identification {
                    &mut out.identified
                } else {
                    &mut out.matched
                };
                if !list.contains(&id) {
                    list.push(id);
                }
                &stats.on_torrent
            }
            // Held meanwhile, or no longer stored.
            Ok(false) => &stats.refused,
            // Disabled or purged meanwhile (SRC-1).
            Err(lodestar_store::Error::Invalid(_)) => &stats.refused,
            Err(e) => return Err(e),
        };
        UnverifiedStats::add(counter, 1);
    }
    if !out.matched.is_empty() {
        // The kind is a refinement, never a safety decision: if classifying
        // fails here, or there's no live filter to classify under, the
        // backfill applies the match instead.
        let ids = out.matched.clone();
        let deferred = match ctx.filter {
            Some(filter) => match crate::enrich::apply_matches(store, &ids, filter, ctx.adult, now)
            {
                Ok(m) => !m.deferred.is_empty(),
                // Marked if it can be; the flag starts a pass either way.
                Err(e) => {
                    enrich_failed(stats, &e);
                    mark_for_backfill(store, &ids, stats);
                    true
                }
            },
            None => {
                mark_for_backfill(store, &ids, stats);
                true
            }
        };
        if deferred && let Some(flag) = ctx.reenrich {
            flag.store(true, Ordering::Release);
        }
    }
    if !out.identified.is_empty() {
        // The effective title is resolved by the backfill, which reads the
        // claims and logs the torrent for the index (ENR-10). Like a match's
        // kind, it's a refinement, never a safety decision.
        mark_for_backfill(store, &out.identified, stats);
        if let Some(flag) = ctx.reenrich {
            flag.store(true, Ordering::Release);
        }
    }
    Ok(out)
}

/// Marks `ids` for the backfill, logging and counting a failure. When the
/// mark fails, the rows keep their kind until the next rules or parser
/// change re-marks them: the crash window ENR-1b already accepts for a
/// refinement that isn't a safety decision.
fn mark_for_backfill(store: &mut Store, ids: &[i64], stats: &UnverifiedStats) {
    if let Err(e) = store.mark_unenriched(ids) {
        enrich_failed(stats, &e);
    }
}

fn enrich_failed(stats: &UnverifiedStats, e: &lodestar_store::Error) {
    UnverifiedStats::add(&stats.enrich_failed, 1);
    lodestar_log::log_event!(
        crate::log_events::ENRICHMENT_FAILED,
        error = crate::log_events::store_error(e)
    );
}

/// Removes what `source` claimed about `key`, holding nothing: the
/// unverified item when its claims are this source's, and this source's
/// claims on a torrent stored under the key. Each removal is SRC-1's
/// refile write with no decision.
fn drop_source_claims(
    store: &mut Store,
    key: [u8; 20],
    source: SourceId,
) -> Result<(), lodestar_store::Error> {
    if let Some(item) = store.unverified(Network::Bt, &key)?
        && item.source == source
    {
        let r = RefiledUnverified {
            id: item.id,
            filtered: None,
        };
        store.refile_unverified(&[r], || true)?;
    }
    if let Some(torrent_id) = store.find(&lodestar_metainfo::InfoHash(key))? {
        let r = RefiledClaims {
            torrent_id,
            source,
            filtered: None,
        };
        store.refile_claims(&[r], || true)?;
    }
    Ok(())
}

/// Which table the refile sweep is on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Items,
    Claims,
}

/// The sweep that judges stored unverified items and torrent claims again
/// whenever the filter engine changes, and once per start (as
/// `CandidateSweep`, `2026-10-31-B-import-refile.md`).
#[derive(Debug, Default)]
pub struct Sweep {
    /// The engine version it runs under; `None` until the first step.
    pub version: Option<u64>,
    pub phase: Phase,
    /// The last item id reached.
    pub after_item: i64,
    /// The last `(torrent_id, source)` of claims reached.
    pub after_claims: (i64, u8),
    /// Nothing is left to judge under `version`.
    pub done: bool,
    /// The wait after a store error.
    pub(crate) backoff: Backoff,
}

impl Sweep {
    /// Has a step to take now.
    pub fn ready(&self) -> bool {
        !self.done && self.backoff.ready()
    }

    /// Starts again from the first item under `version`.
    pub fn restart(&mut self, version: u64) {
        *self = Sweep {
            version: Some(version),
            ..Sweep::default()
        };
    }
}

/// What one refile step did.
#[derive(Debug, Default)]
pub struct Step {
    /// The rules changed under it: nothing was written; start again.
    pub rejected: bool,
    /// The phase it read is finished.
    pub end: bool,
    /// Where the phase goes on from.
    pub after_item: i64,
    pub after_claims: (i64, u8),
    /// Hashes the step held: intake refuses them.
    pub held: Vec<[u8; 20]>,
    /// Items and claims removed.
    pub removed: usize,
    /// Torrents a held decision deleted, for the search index.
    pub deleted: Vec<i64>,
}

/// One step of the refile sweep: judges one page of items (or claims)
/// under the engine at `version` and holds back what newly matches, in
/// one short transaction the store refuses when the rules moved on.
///
/// - **Items:** any hold removes the item and holds its hash (it has no
///   verified metadata). Claims that don't decode are removed: they can't
///   be judged, so they can't be kept.
/// - **Claims on stored torrents:** a floor or core hold removes the
///   claims, holds the hash and deletes the torrent; any other hold only
///   removes the claims, since the torrent's verified metadata is judged
///   on its own (claims never override it).
///
/// Memory is one page: at most [`REFILE_PAGE`] rows and SRC-1's
/// `PAGE_BYTES` (8 MiB) of decoded claims plus one row.
#[allow(clippy::too_many_arguments)]
pub fn refile_step(
    store: &mut Store,
    handle: &Handle,
    version: u64,
    phase: Phase,
    after_item: i64,
    after_claims: (i64, u8),
    ctx: Ctx<'_>,
    now: i64,
    mark: &dyn Fn(&Store, &[[u8; 20]]),
) -> Result<Step, lodestar_store::Error> {
    let rejected = Step {
        rejected: true,
        ..Step::default()
    };
    let engine = handle.current();
    if engine.version != version {
        return Ok(rejected);
    }
    let current = || handle.current().version == version;
    match phase {
        Phase::Items => {
            let page = store.unverified_after(after_item, REFILE_PAGE)?;
            let Some(last) = page.last().map(|i| i.id) else {
                return Ok(Step {
                    end: true,
                    after_item,
                    after_claims,
                    ..Step::default()
                });
            };
            // Up to the judging budget: the rest is the next step's.
            let (mut judged, mut spent, mut last) = (Vec::new(), 0usize, last);
            for (n, i) in page.iter().enumerate() {
                let cost = i.claims.as_ref().map_or(0, |c| judged_len(c, None));
                if n > 0 && spent.saturating_add(cost) > REFILE_JUDGED_BYTES {
                    last = page[n - 1].id;
                    break;
                }
                spent = spent.saturating_add(cost);
                judged.extend(judge_item(&engine, i, ctx, now));
            }
            let refiled: Vec<RefiledUnverified<'_>> = judged
                .iter()
                .map(|(id, h)| RefiledUnverified {
                    id: *id,
                    filtered: h.as_ref().map(|h| h.filtered(now, ctx.quarantine_secs)),
                })
                .collect();
            let held: Vec<[u8; 20]> = judged
                .iter()
                .flat_map(|(_, h)| h.iter().flat_map(|h| h.hashes.iter().copied()))
                .collect();
            let mut step = Step {
                after_item: last,
                after_claims,
                ..Step::default()
            };
            if refiled.is_empty() {
                return Ok(step);
            }
            if !held.is_empty() {
                mark(store, &held);
            }
            let Some(done) = store.refile_unverified(&refiled, current)? else {
                return Ok(rejected);
            };
            step.removed = done.ids.len();
            step.deleted = done.torrents_deleted;
            step.held = held;
            Ok(step)
        }
        Phase::Claims => {
            let source = SourceId::new(after_claims.1).unwrap_or(SourceId::new(0).expect("0"));
            let page = store.claims_after((after_claims.0, source), REFILE_PAGE)?;
            let Some(mut last) = page.last().map(|c| (c.torrent_id, c.source.get())) else {
                return Ok(Step {
                    end: true,
                    after_item,
                    after_claims,
                    ..Step::default()
                });
            };
            let mut judged: Vec<(i64, SourceId, Option<Hold>)> = Vec::new();
            let mut spent = 0usize;
            for (n, c) in page.iter().enumerate() {
                let Some(claims) = &c.claims else {
                    judged.push((c.torrent_id, c.source, None));
                    continue;
                };
                let Some(hashes) = store.torrent_swarm_hashes(c.torrent_id)? else {
                    continue;
                };
                // Claims an always-adult source made: judged as adult too.
                let adult = ctx.adult.has(c.source);
                // An enrichment match or a details section is judged with
                // the torrent's name, as at ingest (ENR-1, ENR-9b).
                let context = if claims.adult.is_some() || claims.details.is_some() {
                    store.torrent(c.torrent_id)?.map(|r| r.name)
                } else {
                    None
                };
                // Up to the judging budget: the rest is the next step's.
                let cost = judged_len(claims, context.as_deref());
                if n > 0 && spent.saturating_add(cost) > REFILE_JUDGED_BYTES {
                    let prev = &page[n - 1];
                    last = (prev.torrent_id, prev.source.get());
                    break;
                }
                spent = spent.saturating_add(cost);
                let found = judge_hashes(
                    &engine,
                    &hashes,
                    claims,
                    context.as_deref(),
                    ctx.sealer,
                    adult,
                    now,
                );
                if let Some(mut h) = found {
                    // Only the floor and the core hold a stored torrent on
                    // a claim; any other rule only takes the claims away.
                    let hold = h.core.then(|| {
                        h.hashes = hashes.clone();
                        h
                    });
                    judged.push((c.torrent_id, c.source, hold));
                }
            }
            let refiled: Vec<RefiledClaims<'_>> = judged
                .iter()
                .map(|(t, s, h)| RefiledClaims {
                    torrent_id: *t,
                    source: *s,
                    filtered: h.as_ref().map(|h| h.filtered(now, ctx.quarantine_secs)),
                })
                .collect();
            let held: Vec<[u8; 20]> = judged
                .iter()
                .flat_map(|(_, _, h)| h.iter().flat_map(|h| h.hashes.iter().copied()))
                .collect();
            let mut step = Step {
                after_item,
                after_claims: last,
                ..Step::default()
            };
            if refiled.is_empty() {
                return Ok(step);
            }
            if !held.is_empty() {
                mark(store, &held);
            }
            let Some(done) = store.refile_claims(&refiled, current)? else {
                return Ok(rejected);
            };
            step.removed = done.claims;
            step.deleted = done.torrents_deleted;
            step.held = held;
            Ok(step)
        }
    }
}

/// An item to remove, with the decision to write: `None` keeps it.
fn judge_item(
    engine: &Engine,
    i: &UnverifiedItem,
    ctx: Ctx<'_>,
    now: i64,
) -> Option<(i64, Option<Hold>)> {
    let Some(claims) = &i.claims else {
        // Unreadable claims can't be judged: removed. An item whose
        // claims a purge took has none to judge: kept until its other
        // finder's next claims (SRC-1).
        return i.unreadable.then_some((i.id, None));
    };
    // Not a swarm hash (no network but clearnet BitTorrent exists yet):
    // it can't be judged by hash, so it's removed rather than kept.
    let Ok(key) = <[u8; 20]>::try_from(i.key.as_slice()) else {
        return Some((i.id, None));
    };
    // Only the floor and the core hold; any other rule removes the item
    // and holds nothing (runner, #383). An item any always-adult source
    // found is judged as adult too, whichever source's claims it holds.
    let adult = ctx.adult.any(i.sources) || ctx.adult.has(i.source);
    judge(engine, key, claims, ctx.sealer, adult, now).map(|h| (i.id, h.core.then_some(h)))
}

/// [`judge`] against every swarm hash of a stored torrent.
fn judge_hashes(
    engine: &Engine,
    hashes: &[[u8; 20]],
    c: &Claims,
    context: Option<&str>,
    sealer: Option<&Sealer>,
    adult: bool,
    now: i64,
) -> Option<Hold> {
    let first = *hashes.first()?;
    let mut h = judge_in(engine, first, c, context, sealer, adult, now);
    for &other in &hashes[1..] {
        if h.is_some() {
            break;
        }
        h = judge_in(engine, other, c, context, sealer, adult, now);
    }
    h
}

/// The verification cursor (`sources.verify_unverified`, default off):
/// the stored items are the queue, read a few at a time, so nothing is
/// held in memory however many there are.
#[derive(Debug, Default)]
pub struct Verify {
    pub on: bool,
    /// The last item id handed over.
    pub after: i64,
    /// After a full round, when the next one starts (Unix seconds).
    pub resume_at: Option<i64>,
}

impl Verify {
    /// The next keys to fetch: at most `limit` unverified items after the
    /// cursor, as discoveries from the source whose claims they carry.
    /// Private items stay private: the pool sends them only to their own
    /// source, never to public peer finding (§18.7).
    pub fn next(
        &mut self,
        store: &Store,
        limit: usize,
        now: i64,
    ) -> Result<Vec<Discovery>, lodestar_store::Error> {
        if !self.on || self.resume_at.is_some_and(|at| now < at) {
            return Ok(Vec::new());
        }
        self.resume_at = None;
        // Keys only, no claims decoded; a disabled or purged source's
        // items are passed over.
        let page = store.pending_keys_after(self.after, limit)?;
        let Some(last) = page.last() else {
            if self.after != 0 {
                self.after = 0;
                self.resume_at = Some(now.saturating_add(VERIFY_ROUND_SECS));
            }
            return Ok(Vec::new());
        };
        self.after = last.id;
        Ok(page
            .into_iter()
            .filter_map(|i| {
                let key: [u8; 20] = i.key.as_slice().try_into().ok()?;
                let mut found =
                    lodestar_sources::Found::key(lodestar_metainfo::InfoHash(key), VERIFY_STRATEGY);
                found.private_source = i.private_source;
                Some(Discovery {
                    source: i.source,
                    found,
                    at: now,
                    reply: false,
                })
            })
            .collect())
    }
}

/// `p` with the `claim-mismatch` junk reason added to its enrichment, when
/// the claimed files contradict its verified ones (§18.5). The verified
/// metadata itself is never changed: claims only add the reason. `None`
/// when they agree, or there's no enrichment to add it to.
pub fn with_mismatch(p: &Prepared, verified: &[FileInfo], c: &Claims) -> Option<Prepared> {
    let claimed: Vec<lodestar_classify::ClaimedFile<'_>> = c
        .files
        .iter()
        .map(|f| lodestar_classify::ClaimedFile {
            path: &f.path,
            size: f.size,
        })
        .collect();
    if !lodestar_classify::claim_mismatch(verified, &claimed) {
        return None;
    }
    let e = p.enrichment.as_ref()?;
    let mut record = Record::decode(&e.data).ok()?;
    record
        .junk
        .add(lodestar_classify::JunkReason::ClaimMismatch);
    let mut out = p.clone();
    let e = out.enrichment.as_mut()?;
    e.junk_score = record.junk.score;
    e.data = record.encode();
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claimed_paths_split_on_either_separator() {
        assert_eq!(segments("a/b\\c.mkv"), vec!["a", "b", "c.mkv"]);
        assert_eq!(segments("/x//y/"), vec!["x", "y"]);
        assert_eq!(segments(""), vec![""]);
        assert_eq!(segments("//"), vec!["//"]);
    }

    #[test]
    fn a_hold_prints_no_name() {
        let h = Hold {
            hashes: vec![[1; 20]],
            rule: "CS-2a".into(),
            action: Action::Quarantine.code(),
            held: Held::Quarantine,
            sealed: Some(b"sealed-secret".to_vec()),
            evidence: vec![],
            adult_hold: false,
            core: true,
        };
        let s = format!("{h:?}");
        assert!(!s.contains("sealed-secret") && !s.contains("115"), "{s}");
    }
}
