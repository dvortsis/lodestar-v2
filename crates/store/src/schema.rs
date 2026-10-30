//! Schema migrations and connection setup (architecture §7).
//!
//! Each milestone adds the tables it writes as a new migration; migrations
//! are append-only and never edited once released.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension};

use crate::{Error, Result};

/// `(version, sql)`, applied in order, each in its own transaction.
pub(crate) const MIGRATIONS: &[(i64, &str)] = &[
    (1, V1),
    (2, V2),
    (3, V3),
    (4, V4),
    (5, V5),
    (6, V6),
    (7, V7),
    (8, V8),
    (9, V9),
    (10, V10),
    (11, V11),
    (12, V12),
    (13, V13),
];

/// Imports (IMP-1, `docs/decisions/2026-10-30-IMP-1.md`): durable jobs
/// bound to a source identity, their cursors, which torrents each job
/// touched, valid hashes too incomplete to be torrents, and the quality
/// of imported rows. Additive only.
///
/// - `import_quality` holds what an import or a verified upgrade decided.
///   Without a row, the state is read from `flags` and `info_size`
///   (`import::Quality`): existing imports read as unknown with no
///   migration pass, and nothing is scheduled for them.
/// - Candidates never hold an empty name or a zero size. One whose hash is
///   tombstoned, quarantined or added to the floor's hash list is deleted,
///   and its hash journalled so snapshots lose it too; one whose hash
///   becomes a torrent is deleted.
const V13: &str = "
CREATE TABLE import_sources (
    id         INTEGER PRIMARY KEY,
    adapter    TEXT NOT NULL
        CHECK (length(adapter) BETWEEN 1 AND 32 AND adapter NOT GLOB '*[^a-z0-9_]*'),
    dataset    BLOB NOT NULL CHECK (length(dataset) BETWEEN 1 AND 64),
    schema_sig BLOB NOT NULL CHECK (length(schema_sig) BETWEEN 1 AND 64),
    UNIQUE (adapter, dataset, schema_sig)
) STRICT;

CREATE TABLE import_jobs (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    source        INTEGER NOT NULL REFERENCES import_sources (id),
    adapter_ver   INTEGER NOT NULL,
    contract_ver  INTEGER NOT NULL,
    -- inspecting, ready, reading, committing, indexing, complete,
    -- complete with issues.
    phase         INTEGER NOT NULL CHECK (phase BETWEEN 0 AND 6),
    -- none, paused, blocked, cancelled, failed.
    control       INTEGER NOT NULL CHECK (control BETWEEN 0 AND 4),
    reason        INTEGER CHECK (reason IS NULL OR reason BETWEEN 0 AND 5),
    options       BLOB NOT NULL CHECK (length(options) <= 4096),
    -- A sealed secret's name, never the secret.
    credential    TEXT CHECK (credential IS NULL OR (length(credential) BETWEEN 1 AND 64
        AND credential NOT GLOB '*[^a-z0-9_-]*')),
    read          INTEGER NOT NULL DEFAULT 0,
    new           INTEGER NOT NULL DEFAULT 0,
    merged        INTEGER NOT NULL DEFAULT 0,
    candidates    INTEGER NOT NULL DEFAULT 0,
    filtered      INTEGER NOT NULL DEFAULT 0,
    quarantined   INTEGER NOT NULL DEFAULT 0,
    invalid       INTEGER NOT NULL DEFAULT 0,
    files_dropped INTEGER NOT NULL DEFAULT 0,
    partial       INTEGER NOT NULL DEFAULT 0,
    source_bytes  INTEGER NOT NULL DEFAULT 0,
    max_new_id    INTEGER NOT NULL DEFAULT 0,
    revision      INTEGER NOT NULL DEFAULT 1,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    finished_at   INTEGER
) STRICT;
-- At most one unfinished job per source identity.
CREATE UNIQUE INDEX import_jobs_open ON import_jobs (source) WHERE control < 3 AND phase < 5;

CREATE TABLE import_cursors (
    job      INTEGER PRIMARY KEY REFERENCES import_jobs (id) ON DELETE CASCADE,
    seq      INTEGER NOT NULL CHECK (seq >= 0),
    position BLOB NOT NULL CHECK (length(position) <= 512),
    at       INTEGER NOT NULL
) STRICT;

CREATE TABLE import_membership (
    torrent_id INTEGER NOT NULL REFERENCES torrents (id) ON DELETE CASCADE,
    job        INTEGER NOT NULL REFERENCES import_jobs (id) ON DELETE CASCADE,
    created    INTEGER NOT NULL CHECK (created IN (0, 1)),
    PRIMARY KEY (torrent_id, job)
) STRICT, WITHOUT ROWID;
CREATE INDEX import_membership_job ON import_membership (job);

-- A rowid table: names run to 4 KiB, too big for WITHOUT ROWID. One row
-- per torrent identity, as in `torrents`: the primary hash, and the
-- truncated v2 hash unique too.
CREATE TABLE import_candidates (
    id        INTEGER PRIMARY KEY,
    hash      BLOB NOT NULL UNIQUE CHECK (length(hash) = 20),
    hash_v2   BLOB CHECK (hash_v2 IS NULL OR length(hash_v2) = 32),
    job       INTEGER NOT NULL REFERENCES import_jobs (id) ON DELETE CASCADE,
    name      BLOB CHECK (name IS NULL OR length(name) BETWEEN 1 AND 4096),
    size      INTEGER CHECK (size IS NULL OR size > 0),
    missing   INTEGER NOT NULL CHECK (missing > 0),
    privacy   INTEGER NOT NULL CHECK (privacy BETWEEN 0 AND 2),
    source_at INTEGER,
    at        INTEGER NOT NULL
) STRICT;
CREATE INDEX import_candidates_job ON import_candidates (job);
CREATE UNIQUE INDEX import_candidates_v2 ON import_candidates (substr(hash_v2, 1, 20))
    WHERE hash_v2 IS NOT NULL;

CREATE TABLE import_quality (
    torrent_id   INTEGER PRIMARY KEY REFERENCES torrents (id) ON DELETE CASCADE,
    -- unknown, partial, reported complete.
    completeness INTEGER NOT NULL CHECK (completeness BETWEEN 0 AND 2),
    -- unknown, unverified, verified, conflicting.
    verification INTEGER NOT NULL CHECK (verification BETWEEN 0 AND 3),
    -- unknown, public, private.
    privacy      INTEGER NOT NULL CHECK (privacy BETWEEN 0 AND 2),
    repair       INTEGER NOT NULL CHECK (repair >= 0),
    source_at    INTEGER,
    -- NULL when the row was written by something other than an import.
    imported_at  INTEGER
) STRICT;

CREATE TRIGGER import_candidate_tombstoned AFTER INSERT ON tombstones BEGIN
    INSERT INTO snapshot_deletions (hash, at)
        SELECT hash, unixepoch() FROM import_candidates WHERE hash = new.hash
        UNION ALL
        SELECT hash, unixepoch() FROM import_candidates
        WHERE hash_v2 IS NOT NULL AND substr(hash_v2, 1, 20) = new.hash
        ON CONFLICT (hash) DO UPDATE SET at = max(at, excluded.at);
    DELETE FROM import_candidates WHERE hash = new.hash;
    DELETE FROM import_candidates
        WHERE hash_v2 IS NOT NULL AND substr(hash_v2, 1, 20) = new.hash;
END;
CREATE TRIGGER import_candidate_quarantined AFTER INSERT ON quarantine BEGIN
    INSERT INTO snapshot_deletions (hash, at)
        SELECT hash, unixepoch() FROM import_candidates WHERE hash = new.hash
        UNION ALL
        SELECT hash, unixepoch() FROM import_candidates
        WHERE hash_v2 IS NOT NULL AND substr(hash_v2, 1, 20) = new.hash
        ON CONFLICT (hash) DO UPDATE SET at = max(at, excluded.at);
    DELETE FROM import_candidates WHERE hash = new.hash;
    DELETE FROM import_candidates
        WHERE hash_v2 IS NOT NULL AND substr(hash_v2, 1, 20) = new.hash;
END;
CREATE TRIGGER import_candidate_floored AFTER INSERT ON cs_hashes BEGIN
    INSERT INTO snapshot_deletions (hash, at)
        SELECT hash, unixepoch() FROM import_candidates WHERE hash = new.hash
        UNION ALL
        SELECT hash, unixepoch() FROM import_candidates
        WHERE hash_v2 IS NOT NULL AND substr(hash_v2, 1, 20) = new.hash
        ON CONFLICT (hash) DO UPDATE SET at = max(at, excluded.at);
    DELETE FROM import_candidates WHERE hash = new.hash;
    DELETE FROM import_candidates
        WHERE hash_v2 IS NOT NULL AND substr(hash_v2, 1, 20) = new.hash;
END;
CREATE TRIGGER import_candidate_stored AFTER INSERT ON torrents BEGIN
    DELETE FROM import_candidates WHERE hash = new.info_hash;
    DELETE FROM import_candidates
        WHERE hash_v2 IS NOT NULL AND substr(hash_v2, 1, 20) = new.info_hash;
    DELETE FROM import_candidates
        WHERE new.info_hash_v2 IS NOT NULL AND hash = substr(new.info_hash_v2, 1, 20);
    DELETE FROM import_candidates
        WHERE new.info_hash_v2 IS NOT NULL AND hash_v2 IS NOT NULL
          AND substr(hash_v2, 1, 20) = substr(new.info_hash_v2, 1, 20);
END;
";

/// The snapshot purge's deletion journal (`snapshots`): the hash of every
/// torrent the store deletes, with when, so snapshots lose it even after
/// the fetch hold that also recorded it expires or is released. Hashes,
/// not ids: a snapshot from another rollback history numbers torrents
/// differently. A hybrid's truncated v2 hash goes in as well, as
/// `find_any` matches either.
///
/// `at` bounds the journal: a row is matched only in snapshots taken up
/// to a day after it, and dropped once every such snapshot has expired
/// (`RETAIN_SECS` plus that day). Deleting a hash again moves its time
/// forward. The backfill keeps the holds that may stand for a deletion
/// this version didn't see, at their own time, unless the torrent is live
/// again: a snapshot copy of a live torrent isn't purged.
const V12: &str = "
CREATE TABLE snapshot_deletions (
    hash BLOB PRIMARY KEY CHECK (length(hash) = 20),
    at   INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
INSERT INTO snapshot_deletions (hash, at)
    SELECT hash, max(at) FROM (
        SELECT hash, at FROM quarantine UNION ALL SELECT hash, at FROM tombstones
    )
    WHERE hash NOT IN (SELECT info_hash FROM torrents)
      AND hash NOT IN (
          SELECT substr(info_hash_v2, 1, 20) FROM torrents WHERE info_hash_v2 IS NOT NULL
      )
    GROUP BY hash;
CREATE TRIGGER snapshot_torrent_deleted AFTER DELETE ON torrents BEGIN
    INSERT INTO snapshot_deletions (hash, at) VALUES (old.info_hash, unixepoch())
        ON CONFLICT (hash) DO UPDATE SET at = max(at, excluded.at);
    INSERT INTO snapshot_deletions (hash, at)
        SELECT substr(old.info_hash_v2, 1, 20), unixepoch() WHERE old.info_hash_v2 IS NOT NULL
        ON CONFLICT (hash) DO UPDATE SET at = max(at, excluded.at);
END;
";

/// V12's pre-release shape repair. Builds from `a7c6d5c8` up to the fix
/// that added `at` created `snapshot_deletions (hash)` with no time and a
/// trigger that wrote none, and recorded V12 as applied, so V12 doesn't
/// run again there. This brings such a store to V12's final shape, once:
/// rows are dated now (the safe direction: they keep scrubbing snapshots
/// for the journal's full 91 days rather than being dropped as old), a
/// hash that's live again is dropped as V12's backfill skips it, and the
/// trigger is replaced.
const V12_REPAIR: &str = "
ALTER TABLE snapshot_deletions ADD COLUMN at INTEGER NOT NULL DEFAULT 0;
UPDATE snapshot_deletions SET at = unixepoch() WHERE at = 0;
DELETE FROM snapshot_deletions
    WHERE hash IN (SELECT info_hash FROM torrents)
       OR hash IN (
           SELECT substr(info_hash_v2, 1, 20) FROM torrents WHERE info_hash_v2 IS NOT NULL
       );
DROP TRIGGER snapshot_torrent_deleted;
CREATE TRIGGER snapshot_torrent_deleted AFTER DELETE ON torrents BEGIN
    INSERT INTO snapshot_deletions (hash, at) VALUES (old.info_hash, unixepoch())
        ON CONFLICT (hash) DO UPDATE SET at = max(at, excluded.at);
    INSERT INTO snapshot_deletions (hash, at)
        SELECT substr(old.info_hash_v2, 1, 20), unixepoch() WHERE old.info_hash_v2 IS NOT NULL
        ON CONFLICT (hash) DO UPDATE SET at = max(at, excluded.at);
END;
";

/// M0: the canonical torrent tables plus settings and zstd dictionaries.
///
/// Ids are `AUTOINCREMENT` so a deleted id is never reused: hot-column
/// slots, index documents and the percolator watermark all key on it.
/// Child tables use `torrent_id INTEGER PRIMARY KEY`, i.e. they are keyed by
/// rowid with no separate index.
const V1: &str = "
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value ANY NOT NULL
) STRICT;

CREATE TABLE zstd_dicts (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    kind       TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    samples    INTEGER NOT NULL,
    data       BLOB NOT NULL
) STRICT;

CREATE TABLE torrents (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    -- v1 hash, or the truncated v2 hash for pure v2 torrents.
    info_hash     BLOB NOT NULL UNIQUE CHECK (length(info_hash) = 20),
    info_hash_v2  BLOB CHECK (info_hash_v2 IS NULL OR length(info_hash_v2) = 32),
    -- Decoded name; the raw bytes and their encoding are kept only when
    -- they aren't plain UTF-8, so decoding can be redone.
    name          TEXT NOT NULL,
    name_raw      BLOB,
    name_enc      INTEGER,
    size          INTEGER NOT NULL,
    piece_len     INTEGER NOT NULL,
    piece_count   INTEGER NOT NULL,
    file_count    INTEGER NOT NULL,
    info_size     INTEGER NOT NULL,
    flags         INTEGER NOT NULL,
    discovered_at INTEGER NOT NULL,
    metadata_at   INTEGER NOT NULL,
    last_seen_at  INTEGER NOT NULL,
    -- Derived in M3; recomputable at any time.
    content_type  INTEGER,
    junk_score    INTEGER,
    group_key     INTEGER,
    comp          BLOB
) STRICT;

-- Hybrid and v2 torrents are also announced under the truncated v2 hash.
CREATE UNIQUE INDEX torrents_v2 ON torrents (substr(info_hash_v2, 1, 20))
    WHERE info_hash_v2 IS NOT NULL;

CREATE TABLE torrent_files (
    torrent_id INTEGER PRIMARY KEY REFERENCES torrents (id) ON DELETE CASCADE,
    codec      INTEGER NOT NULL,
    dict_id    INTEGER REFERENCES zstd_dicts (id),
    data       BLOB NOT NULL
) STRICT;

CREATE TABLE torrent_extra (
    torrent_id INTEGER PRIMARY KEY REFERENCES torrents (id) ON DELETE CASCADE,
    data       BLOB NOT NULL
) STRICT;

CREATE TABLE torrent_raw (
    torrent_id INTEGER PRIMARY KEY REFERENCES torrents (id) ON DELETE CASCADE,
    data       BLOB NOT NULL
) STRICT;

CREATE TABLE torrent_parsed (
    torrent_id INTEGER PRIMARY KEY REFERENCES torrents (id) ON DELETE CASCADE,
    parser_ver INTEGER NOT NULL,
    data       BLOB NOT NULL
) STRICT;
";

/// M1: provenance, the shared id sequence, Insights counters and the
/// persisted fetch queue (architecture §5, §7, §18.5).
///
/// - `id_seq` is the one id space for every item table. Rows insert
///   explicit ids from it inside the writer's transaction, so an item keeps
///   its id across tables (unverified → torrents, overlay networks).
///   `torrents.id` stays `AUTOINCREMENT` as a second guard against reuse.
/// - `torrents.sources` is a bitset over `sources.id` (0..=63);
///   `first_source` is the source that found it first. Pre-M1 rows have
///   neither.
/// - The rollup and cohort tables hold additive counters keyed by hour.
///   Cohorts also carry the source, so the funnel can be split by it.
/// - `fetch_queue` holds keys still waiting for metadata across restarts.
///   No peers are stored with them: peer addresses are transient (§18.7)
///   and stale by the time we restart anyway.
const V2: &str = "
CREATE TABLE id_seq (
    one  INTEGER PRIMARY KEY CHECK (one = 1),
    next INTEGER NOT NULL CHECK (next > 0)
) STRICT;

INSERT INTO id_seq (one, next) SELECT 1, max(
    coalesce((SELECT seq FROM sqlite_sequence WHERE name = 'torrents'), 0),
    coalesce((SELECT max(id) FROM torrents), 0)
) + 1;

CREATE TABLE sources (
    id       INTEGER PRIMARY KEY CHECK (id BETWEEN 0 AND 63),
    plugin   TEXT NOT NULL UNIQUE,
    version  TEXT NOT NULL,
    state    INTEGER NOT NULL,
    added_at INTEGER NOT NULL
) STRICT;

ALTER TABLE torrents ADD COLUMN sources INTEGER NOT NULL DEFAULT 0;
ALTER TABLE torrents ADD COLUMN first_source INTEGER
    CHECK (first_source IS NULL OR first_source BETWEEN 0 AND 63);

CREATE TABLE rollup_hour (
    hour   INTEGER NOT NULL,
    metric INTEGER NOT NULL,
    dim    INTEGER NOT NULL,
    key    INTEGER NOT NULL,
    value  INTEGER NOT NULL,
    PRIMARY KEY (hour, metric, dim, key)
) STRICT, WITHOUT ROWID;

CREATE TABLE cohort_hour (
    cohort INTEGER NOT NULL,
    source INTEGER NOT NULL,
    stage  INTEGER NOT NULL,
    reason INTEGER NOT NULL,
    count  INTEGER NOT NULL,
    PRIMARY KEY (cohort, source, stage, reason)
) STRICT, WITHOUT ROWID;

CREATE TABLE events (
    id     INTEGER PRIMARY KEY,
    at     INTEGER NOT NULL,
    kind   INTEGER NOT NULL,
    detail TEXT NOT NULL
) STRICT;
CREATE INDEX events_at ON events (at);

CREATE TABLE fetch_queue (
    info_hash BLOB PRIMARY KEY CHECK (length(info_hash) = 20),
    source    INTEGER NOT NULL CHECK (source BETWEEN 0 AND 63),
    cohort    INTEGER NOT NULL,
    strategy  INTEGER NOT NULL,
    priority  INTEGER NOT NULL,
    attempts  INTEGER NOT NULL,
    flags     INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
";

/// M2: torrents changed after they were stored (a partial import upgraded
/// by a fetched info dict), for the search indexer to re-index. The
/// indexer follows `seq` with its own watermark; rows are few (one per
/// upgrade), so they're kept.
const V3: &str = "
CREATE TABLE reindex_log (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    torrent_id INTEGER NOT NULL
) STRICT;
";

/// M4 add-on: content filtering (§20.7). Only what filtering needs to work
/// is kept: no table records who changed a filter, when, or why (§20.11).
const V4: &str = "
-- CS-1 entries (§20.5.2), every hash form. origin: 0 confirmed,
-- 1 reported, 2 imported.
CREATE TABLE cs_hashes (
    hash     BLOB PRIMARY KEY CHECK (length(hash) = 20),
    origin   INTEGER NOT NULL,
    added_at INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

-- CS-8 payload signatures.
CREATE TABLE cs_sigs (
    sig      INTEGER PRIMARY KEY,
    added_at INTEGER NOT NULL
) STRICT;

-- Core rules you switched off. Every rule is on unless listed here; the
-- floor is never listed (§20.5.3).
CREATE TABLE core_membership (
    rule    TEXT PRIMARY KEY,
    enabled INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

-- Dropped torrents, so they aren't fetched again (§20.3).
CREATE TABLE tombstones (
    hash BLOB PRIMARY KEY CHECK (length(hash) = 20),
    rule TEXT NOT NULL,
    at   INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

-- Held torrents: hash, rule and a sealed name only (§20.3).
CREATE TABLE quarantine (
    hash       BLOB PRIMARY KEY CHECK (length(hash) = 20),
    rule       TEXT NOT NULL,
    sealed     BLOB,
    at         INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
CREATE INDEX quarantine_expiry ON quarantine (expires_at);

-- Filter decisions for the review queue and the 30-day replay: no names
-- (§20.11).
CREATE TABLE filter_hits (
    id       INTEGER PRIMARY KEY,
    hash     BLOB NOT NULL,
    rule     TEXT NOT NULL,
    action   INTEGER NOT NULL,
    at       INTEGER NOT NULL,
    evidence BLOB NOT NULL
) STRICT;
CREATE INDEX filter_hits_at ON filter_hits (at);
";

/// M4: swarm health (architecture §7, §16 M4).
///
/// - `trackers` is the scrape list. Ids are `AUTOINCREMENT` within u16 and
///   never reused, because `swarm_trackers` packs them: a reused id would
///   credit old counts to a new tracker.
/// - `swarm` has one row per scraped torrent; a torrent with no row has
///   never been scraped. `dht_*` are NULL without a DHT estimate.
///   `next_scrape_at` is indexed for the scheduler's "due" lane.
/// - `swarm_trackers` holds per-tracker counts, only for trackers that
///   report the hash (s+l+c > 0).
/// - `swarm_history` is a fixed-size ring (`history` module), kept only
///   for swarms that were ever non-zero or that a user looked at.
const V5: &str = "
CREATE TABLE trackers (
    id       INTEGER PRIMARY KEY AUTOINCREMENT CHECK (id BETWEEN 1 AND 65535),
    url      TEXT NOT NULL UNIQUE,
    added_at INTEGER NOT NULL,
    enabled  INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    state    INTEGER NOT NULL DEFAULT 0,
    state_at INTEGER,
    message  TEXT
) STRICT;

CREATE TABLE swarm (
    torrent_id         INTEGER PRIMARY KEY REFERENCES torrents (id) ON DELETE CASCADE,
    seeders            INTEGER NOT NULL,
    leechers           INTEGER NOT NULL,
    completed          INTEGER NOT NULL,
    trackers_reporting INTEGER NOT NULL,
    trackers_checked   INTEGER NOT NULL,
    dht_seeds          INTEGER,
    dht_peers          INTEGER,
    last_scrape_at     INTEGER NOT NULL,
    next_scrape_at     INTEGER NOT NULL,
    flags              INTEGER NOT NULL
) STRICT;
CREATE INDEX swarm_due ON swarm (next_scrape_at);

CREATE TABLE swarm_trackers (
    torrent_id INTEGER PRIMARY KEY REFERENCES torrents (id) ON DELETE CASCADE,
    data       BLOB NOT NULL
) STRICT;

CREATE TABLE swarm_history (
    torrent_id INTEGER PRIMARY KEY REFERENCES torrents (id) ON DELETE CASCADE,
    data       BLOB NOT NULL
) STRICT;
";

/// Blocklist subscriptions (§20.13): what to fetch, how it went, and the
/// parsed list (hashes, 20 bytes each, or encoded IP ranges). A version
/// held back by the sanity gates waits as `pending = 1`.
const V6: &str = "
CREATE TABLE subscriptions (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    url           TEXT NOT NULL UNIQUE,
    name          TEXT NOT NULL,
    -- 0 content (hashes), 1 IP ranges.
    kind          INTEGER NOT NULL,
    -- NULL: detected on each fetch.
    format        TEXT,
    -- The filter action code, for content lists.
    action        INTEGER NOT NULL,
    -- A child-abuse hash list you marked as CS-1b.
    floor         INTEGER NOT NULL DEFAULT 0,
    enabled       INTEGER NOT NULL DEFAULT 1,
    interval_secs INTEGER NOT NULL,
    etag          TEXT,
    last_modified TEXT,
    checked_at    INTEGER,
    updated_at    INTEGER,
    entries       INTEGER NOT NULL DEFAULT 0,
    status        TEXT NOT NULL DEFAULT 'new',
    error         TEXT
) STRICT;

CREATE TABLE subscription_data (
    sub_id  INTEGER NOT NULL REFERENCES subscriptions (id) ON DELETE CASCADE,
    pending INTEGER NOT NULL,
    entries INTEGER NOT NULL,
    data    BLOB NOT NULL,
    PRIMARY KEY (sub_id, pending)
) STRICT, WITHOUT ROWID;
";

/// Your rules, pack switches, the CSX-5 override list, exceptions and
/// *Always allow* (§20.7). Each row holds only what filtering needs: no
/// author, reason or history (§20.11). `rule_set_prev` keeps the one
/// previous rule set for Undo.
const V7: &str = "
CREATE TABLE filter_rules (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    position     INTEGER NOT NULL,
    query        TEXT NOT NULL,
    action       INTEGER NOT NULL,
    enabled      INTEGER NOT NULL DEFAULT 1,
    shadow_until INTEGER,
    unless_query TEXT
) STRICT;

-- Packs (by name) and pack rules (by id) whose switch was set.
CREATE TABLE pack_switches (
    id      TEXT PRIMARY KEY,
    enabled INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

-- kind: 0 exact torrents (20-byte hashes), 1 keyword, 2 query.
-- strength: 0 override, 1 neutralize.
CREATE TABLE override_list (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    kind         INTEGER NOT NULL,
    value        BLOB NOT NULL,
    strength     INTEGER NOT NULL,
    scope        TEXT,
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER,
    shadow_until INTEGER,
    hits         INTEGER NOT NULL DEFAULT 0
) STRICT;

-- kind: 0 exact torrents, 2 query, 3 payload signature (8 bytes).
CREATE TABLE filter_exceptions (
    id    INTEGER PRIMARY KEY AUTOINCREMENT,
    rule  TEXT NOT NULL,
    kind  INTEGER NOT NULL,
    value BLOB NOT NULL
) STRICT;

CREATE TABLE filter_allow (
    id    INTEGER PRIMARY KEY AUTOINCREMENT,
    query TEXT NOT NULL
) STRICT;

-- Must-match (1) and must-not-match (0) examples as hashed tokens.
CREATE TABLE filter_fixtures (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    rule       TEXT NOT NULL,
    must_match INTEGER NOT NULL,
    tokens     BLOB NOT NULL
) STRICT;

CREATE TABLE rule_set_prev (
    id   INTEGER PRIMARY KEY CHECK (id = 1),
    data BLOB NOT NULL
) STRICT;
";

/// M4 add-on: your corrections to classification (§20.7, §20.9): Is
/// adult / Not adult and Not junk, re-applied whenever the torrent is
/// classified again.
const V8: &str = "
CREATE TABLE class_corrections (
    torrent_id INTEGER PRIMARY KEY REFERENCES torrents (id) ON DELETE CASCADE,
    -- 1 Is adult, 0 Not adult, NULL no correction.
    adult      INTEGER CHECK (adult IN (0, 1)),
    not_junk   INTEGER NOT NULL CHECK (not_junk IN (0, 1))
) STRICT;
";

/// M4 add-on: classification rules (§20.9): corrections that apply to
/// every torrent a query matches, the way a per-torrent correction applies
/// to one.
const V9: &str = "
CREATE TABLE class_rules (
    id    INTEGER PRIMARY KEY AUTOINCREMENT,
    query TEXT NOT NULL,
    -- 1 Is adult, 0 Not adult.
    adult INTEGER NOT NULL CHECK (adult IN (0, 1))
) STRICT;
";

/// M5: your data (saved searches and their alerts, per-torrent marks,
/// collections), API keys and feed tokens, and reporting (searchable time
/// per hour, the opt-in query log, resolution time). Architecture §7,
/// §10.8, §10.9.5, §12, §12.1; decisions in `docs/decisions/2026-10-22-B8.md`.
///
/// - Ids that anything outside the row refers to (an alert in an SSE
///   cursor or RSS guid, a key in a log reference, a collection in a URL)
///   are `AUTOINCREMENT`, so a deleted id never comes back meaning
///   something else. `query_log` rows are referred to by nothing.
/// - Every child of `torrents` cascades and is indexed on `torrent_id`,
///   so deleting a torrent (retention, a filter relabel) takes its alerts,
///   marks and collection entries with it without scanning.
/// - `api_keys` holds only a SHA-256 of each secret, never the secret.
/// - `query_log` has no column for an address, key or token.
const V10: &str = "
CREATE TABLE saved_searches (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    -- The text as typed, and the parsed AST (serde JSON, the API's and
    -- the percolator's form).
    query      TEXT NOT NULL CHECK (length(CAST(query AS BLOB)) <= 2048),
    ast        TEXT NOT NULL CHECK (json_valid(ast) AND length(ast) <= 65536),
    sort       TEXT CHECK (sort IS NULL OR length(sort) BETWEEN 1 AND 32),
    -- Notify in the app; serve an RSS feed; other channels (webhook, ntfy,
    -- Discord, email: bits defined by the notifier, append only). Pausing
    -- keeps them, so resuming restores them.
    alert      INTEGER NOT NULL CHECK (alert IN (0, 1)),
    rss        INTEGER NOT NULL CHECK (rss IN (0, 1)),
    channels   INTEGER NOT NULL DEFAULT 0 CHECK (channels BETWEEN 0 AND 4294967295),
    paused     INTEGER NOT NULL DEFAULT 0 CHECK (paused IN (0, 1)),
    -- The highest torrent id evaluated against this search.
    watermark  INTEGER NOT NULL CHECK (watermark >= 0),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE alerts (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    saved_search_id INTEGER NOT NULL REFERENCES saved_searches (id) ON DELETE CASCADE,
    torrent_id      INTEGER NOT NULL REFERENCES torrents (id) ON DELETE CASCADE,
    created_at      INTEGER NOT NULL,
    read_at         INTEGER,
    -- A torrent alerts once per search; also the per-search listing.
    UNIQUE (saved_search_id, torrent_id)
) STRICT;
CREATE INDEX alerts_torrent ON alerts (torrent_id);
-- Unread counts and the \"New since\" divider (§10.8).
CREATE INDEX alerts_unread ON alerts (saved_search_id) WHERE read_at IS NULL;

-- One row per torrent you marked; a row with no mark left is deleted.
-- labels: canonical encoding (`user_items` module), NULL for none.
CREATE TABLE user_items (
    torrent_id INTEGER PRIMARY KEY REFERENCES torrents (id) ON DELETE CASCADE,
    favourite  INTEGER NOT NULL CHECK (favourite IN (0, 1)),
    hidden     INTEGER NOT NULL CHECK (hidden IN (0, 1)),
    labels     TEXT CHECK (labels IS NULL OR length(labels) BETWEEN 1 AND 4096),
    note       TEXT CHECK (note IS NULL OR length(note) BETWEEN 1 AND 4000),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK (favourite = 1 OR hidden = 1 OR labels IS NOT NULL OR note IS NOT NULL)
) STRICT;

CREATE TABLE collections (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    note       TEXT CHECK (note IS NULL OR length(note) BETWEEN 1 AND 4000),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE collection_items (
    collection_id INTEGER NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    torrent_id    INTEGER NOT NULL REFERENCES torrents (id) ON DELETE CASCADE,
    position      INTEGER NOT NULL,
    added_at      INTEGER NOT NULL,
    PRIMARY KEY (collection_id, torrent_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX collection_items_order ON collection_items (collection_id, position);
CREATE INDEX collection_items_torrent ON collection_items (torrent_id);

-- kind: 0 API key, 1 feed token (read-only RSS; for one saved search, or
-- every feed when saved_search_id is NULL). scopes: bits defined by the
-- auth code, persisted, append only.
CREATE TABLE api_keys (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    kind            INTEGER NOT NULL CHECK (kind IN (0, 1)),
    name            TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    -- Public, shown to tell keys apart; the lookup key.
    prefix          TEXT NOT NULL UNIQUE CHECK (
        length(prefix) BETWEEN 8 AND 32 AND prefix NOT GLOB '*[^A-Za-z0-9_-]*'),
    hash            BLOB NOT NULL CHECK (length(hash) = 32),
    scopes          INTEGER NOT NULL CHECK (scopes BETWEEN 0 AND 4294967295),
    saved_search_id INTEGER REFERENCES saved_searches (id) ON DELETE CASCADE,
    created_at      INTEGER NOT NULL,
    last_used_at    INTEGER,
    expires_at      INTEGER,
    revoked_at      INTEGER,
    CHECK (kind = 1 OR saved_search_id IS NULL)
) STRICT;
CREATE INDEX api_keys_feed ON api_keys (saved_search_id) WHERE saved_search_id IS NOT NULL;

-- The highest torrent id searchable by the end of each hour (§7). Both
-- columns only grow. Hour 0 holds what was indexed before this table
-- existed, whose hour is unknown.
CREATE TABLE index_watermark_hour (
    hour    INTEGER PRIMARY KEY CHECK (hour >= 0),
    last_id INTEGER NOT NULL CHECK (last_id >= 0)
) STRICT;
CREATE INDEX index_watermark_last_id ON index_watermark_hour (last_id);
INSERT INTO index_watermark_hour (hour, last_id)
    SELECT 0, coalesce(max(id), 0) FROM torrents;

-- Opt-in (off by default), local, pruned to its retention (§10.9.5).
CREATE TABLE query_log (
    id         INTEGER PRIMARY KEY,
    at         INTEGER NOT NULL,
    query      TEXT NOT NULL CHECK (length(CAST(query AS BLOB)) <= 2048),
    results    INTEGER CHECK (results IS NULL OR results >= 0),
    latency_us INTEGER NOT NULL CHECK (latency_us >= 0)
) STRICT;
CREATE INDEX query_log_at ON query_log (at);

-- When enrichment first resolved a torrent's identity, for Discover's
-- \"Recently resolved\" (B13).
ALTER TABLE torrent_parsed ADD COLUMN resolved_at INTEGER;
CREATE INDEX torrent_parsed_resolved ON torrent_parsed (resolved_at, torrent_id)
    WHERE resolved_at IS NOT NULL;

-- Releasing a rule's holds finds them by rule (§20.7), which scanned
-- both tables once per rule.
CREATE INDEX quarantine_rule ON quarantine (rule);
CREATE INDEX tombstones_rule ON tombstones (rule);
";

/// B11b: Insights at daily grain, its state, and removals (architecture §7
/// "Insights rollups", §10.9.3; decisions in
/// `docs/decisions/2026-10-23-B11b.md`).
///
/// - `rollup_day` and `cohort_day` are `rollup_hour` and `cohort_hour` at
///   UTC-day grain (`day` = Unix time / 86,400, `docs/metrics.md` §2),
///   same keys. The nightly compaction folds whole hourly days older than
///   30 days into them, one day per transaction.
/// - `insights_state` holds four integers: `compacted_before_day` (days
///   below it are daily; read in the same snapshot as the rows, so a
///   compaction running beside a read never double-counts or drops a
///   day), `last_compaction_at` (unix seconds, 0 = never),
///   `funnel_events_since_hour` (the first hour fetches are counted by
///   event time, metric 14: earlier buckets are unknown, not 0; 0 on a
///   store with no history yet) and
///   `imported` (searchable imported torrents, kept exact from here on).
/// - `imported` is seeded with the same count `count_flagged_searchable`
///   makes (flag 32 = `flags::IMPORTED`, up to the highest searchable
///   id), once, here. `record_searchable` then adds each range's imports
///   in its own transaction, and the trigger below subtracts.
/// - **The trigger** runs for every torrent deleted by any path (a filter
///   relabel, `delete_torrents`, retention later, the snapshot purge on a
///   migration snapshot) that was searchable (its id at or below the
///   highest `index_watermark_hour.last_id`, hour 0 included). It counts
///   a removal at the current hour under metric 15 (`REMOVED`; dim = its
///   content type, or 255 = `UNCLASSIFIED`), and takes an import off
///   `imported`. The codes are the pipeline's, persisted: a test in
///   `crates/lodestar` holds `metric::REMOVED` to this literal.
const V11: &str = "
CREATE TABLE rollup_day (
    day    INTEGER NOT NULL,
    metric INTEGER NOT NULL,
    dim    INTEGER NOT NULL,
    key    INTEGER NOT NULL,
    value  INTEGER NOT NULL,
    PRIMARY KEY (day, metric, dim, key)
) STRICT, WITHOUT ROWID;

CREATE TABLE cohort_day (
    day    INTEGER NOT NULL,
    source INTEGER NOT NULL,
    stage  INTEGER NOT NULL,
    reason INTEGER NOT NULL,
    count  INTEGER NOT NULL,
    PRIMARY KEY (day, source, stage, reason)
) STRICT, WITHOUT ROWID;

CREATE TABLE insights_state (
    key   TEXT PRIMARY KEY,
    value INTEGER NOT NULL
) STRICT;
INSERT INTO insights_state (key, value) VALUES
    ('compacted_before_day', 0),
    ('last_compaction_at', 0),
    -- The first whole hour after the upgrade (the current one is
    -- partial); on a new store, from the start.
    ('funnel_events_since_hour', CASE
        WHEN EXISTS (SELECT 1 FROM torrents) OR EXISTS (SELECT 1 FROM cohort_hour)
            OR EXISTS (SELECT 1 FROM rollup_hour)
        THEN unixepoch() / 3600 + 1 ELSE 0 END),
    ('imported', (SELECT count(*) FROM torrents WHERE flags & 32 != 0
        AND id <= (SELECT coalesce(max(last_id), 0) FROM index_watermark_hour)));

CREATE TRIGGER torrents_removed AFTER DELETE ON torrents
WHEN old.id <= (SELECT coalesce(max(last_id), 0) FROM index_watermark_hour)
BEGIN
    INSERT INTO rollup_hour (hour, metric, dim, key, value)
    VALUES (unixepoch() / 3600, 15,
            CASE WHEN old.content_type BETWEEN 0 AND 254 THEN old.content_type ELSE 255 END,
            0, 1)
    ON CONFLICT DO UPDATE SET value = value + 1;
    UPDATE insights_state SET value = value - 1
    WHERE key = 'imported' AND old.flags & 32 != 0;
END;
";

/// How long a connection waits for a lock another connection holds.
pub(crate) const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Pragmas that persist in the file and can only be set before the first
/// table exists, then the per-connection ones.
pub(crate) fn configure(conn: &Connection, mmap_size: u64) -> Result<()> {
    let page_count: i64 = conn.pragma_query_value(None, "page_count", |r| r.get(0))?;
    if page_count == 0 {
        conn.pragma_update(None, "page_size", 16384)?;
        conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
    }
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.pragma_update(None, "mmap_size", mmap_size as i64)?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    // Deleted rows are zeroed, not left in free pages: a copy of the data
    // folder (which also holds seal.key) mustn't yield purged quarantine
    // names or deleted torrents (owner, 2026-10-22).
    conn.pragma_update(None, "secure_delete", true)?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    Ok(())
}

/// Brings the schema up to date. Refuses to touch a database written by a
/// newer version, and snapshots an existing database before changing it
/// (architecture §12).
pub(crate) fn migrate(conn: &mut Connection, path: Option<&Path>) -> Result<()> {
    migrate_with(conn, path, MIGRATIONS)?;
    repair_v12(conn)
}

/// Applies [`V12_REPAIR`] to a store with V12's pre-release journal (no
/// `at` column); does nothing otherwise.
fn repair_v12(conn: &mut Connection) -> Result<()> {
    let tx = crate::begin_write(conn)?;
    let has = |column: &str| -> rusqlite::Result<bool> {
        tx.query_row(
            "SELECT count(*) FROM pragma_table_info('snapshot_deletions') WHERE name = ?1",
            [column],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
    };
    if has("hash")? && !has("at")? {
        tx.execute_batch(V12_REPAIR)?;
        tx.commit()?;
    }
    Ok(())
}

/// For read-only connections: the schema must be exactly this build's.
pub(crate) fn check_current(conn: &Connection) -> Result<()> {
    check_current_with(conn, MIGRATIONS.last().map_or(0, |(v, _)| *v))
}

fn check_current_with(conn: &Connection, latest: i64) -> Result<()> {
    let current: i64 = conn
        .query_row("SELECT max(version) FROM schema_migrations", [], |r| {
            r.get::<_, Option<i64>>(0)
        })
        .optional()?
        .flatten()
        .unwrap_or(0);
    if current > latest {
        return Err(Error::SchemaTooNew {
            found: current,
            supported: latest,
        });
    }
    if current < latest {
        return Err(Error::Corrupt(
            "schema not migrated yet; open it for writing first",
        ));
    }
    Ok(())
}

fn migrate_with(
    conn: &mut Connection,
    path: Option<&Path>,
    migrations: &[(i64, &str)],
) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    INTEGER PRIMARY KEY,
             applied_at INTEGER NOT NULL
         ) STRICT;",
    )?;
    let current: i64 = conn
        .query_row("SELECT max(version) FROM schema_migrations", [], |r| {
            r.get::<_, Option<i64>>(0)
        })
        .optional()?
        .flatten()
        .unwrap_or(0);
    let latest = migrations.last().map_or(0, |(v, _)| *v);
    if current > latest {
        return Err(Error::SchemaTooNew {
            found: current,
            supported: latest,
        });
    }
    if current == latest {
        return Ok(());
    }
    if current > 0
        && let Some(path) = path
    {
        take_snapshot(conn, path, current + 1)?;
    }
    for (version, sql) in migrations.iter().filter(|(v, _)| *v > current) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, unixepoch())",
            [version],
        )?;
        tx.commit()?;
    }
    Ok(())
}

/// The newest schema version this build reads and writes.
pub fn latest_schema_version() -> i64 {
    MIGRATIONS.last().map_or(0, |(v, _)| *v)
}

/// The pre-migration snapshot, crash-safe (architecture §12.2): `VACUUM
/// INTO <snapshot>.tmp`, fsync it, rename it to its `pre-v<N>[.<k>].db`
/// name, fsync the directory. A crash leaves only a `.tmp`, which the next
/// migration removes, never a partial snapshot under a snapshot's name:
/// B-update's rollback restores whatever a `pre-v` name holds.
fn take_snapshot(conn: &Connection, db: &Path, version: i64) -> Result<()> {
    remove_stale_tmp(db);
    let backup = snapshot_path(db, version);
    let mut name = backup.clone().into_os_string();
    name.push(".tmp");
    let tmp = std::path::PathBuf::from(name);
    let io = |what: &str, e: std::io::Error| {
        Error::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
            Some(format!("{what} {}: {e}", tmp.display())),
        ))
    };
    let _ = std::fs::remove_file(&tmp);
    let written = conn
        .execute("VACUUM INTO ?1", [tmp.to_string_lossy()])
        .map_err(Error::from)
        .and_then(|_| {
            // Windows' FlushFileBuffers needs write access.
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&tmp)
                .and_then(|f| f.sync_all())
                .map_err(|e| io("syncing", e))
        })
        .and_then(|()| std::fs::rename(&tmp, &backup).map_err(|e| io("renaming", e)));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return written;
    }
    sync_dir(&backup);
    Ok(())
}

/// Removes `<stem>.pre-v….db.tmp` files a crash mid-snapshot left.
fn remove_stale_tmp(db: &Path) {
    let (Some(dir), Some(stem)) = (db.parent(), db.file_stem().and_then(|s| s.to_str())) else {
        return;
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let prefix = format!("{stem}.pre-v");
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with(&prefix)
            && name.ends_with(".db.tmp")
            && entry.file_type().is_ok_and(|t| t.is_file())
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Makes a rename in `file`'s directory durable. Windows can't open a
/// directory for this; NTFS journals the rename itself.
fn sync_dir(file: &Path) {
    #[cfg(unix)]
    if let Some(dir) = file.parent() {
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        if let Ok(f) = std::fs::File::open(dir) {
            let _ = f.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = file;
}

/// `<db>.pre-v<N>.db`, or `<db>.pre-v<N>.<k>.db` if that exists already:
/// `VACUUM INTO` refuses existing files, and an earlier snapshot (say, from
/// a migration that failed and is being retried) must never be lost.
pub(crate) fn snapshot_path(db: &Path, version: i64) -> std::path::PathBuf {
    let candidate = |k: u32| match k {
        0 => db.with_extension(format!("pre-v{version}.db")),
        k => db.with_extension(format!("pre-v{version}.{k}.db")),
    };
    (0..)
        .map(candidate)
        .find(|p| !p.exists())
        .expect("unbounded search")
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE: &[(i64, &str)] = &[
        (1, "CREATE TABLE a (x INTEGER) STRICT;"),
        (2, "CREATE TABLE b (x INTEGER) STRICT;"),
    ];

    fn version(conn: &Connection) -> i64 {
        conn.query_row("SELECT max(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .unwrap()
    }

    /// The id sequence starts after every id torrents has ever used,
    /// including deleted ones.
    /// Migrations apply above the newest recorded one, so a gap (a
    /// number merged out of order) would be skipped forever.
    #[test]
    fn versions_are_contiguous() {
        for (i, (v, _)) in MIGRATIONS.iter().enumerate() {
            assert_eq!(*v, i as i64 + 1);
        }
    }

    #[test]
    fn v2_seeds_the_id_sequence_from_existing_ids() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate_with(&mut conn, None, &MIGRATIONS[..1]).unwrap();
        let insert = |conn: &Connection, id: i64| {
            conn.execute(
                "INSERT INTO torrents (id, info_hash, name, size, piece_len, piece_count,
                     file_count, info_size, flags, discovered_at, metadata_at, last_seen_at)
                 VALUES (?1, ?2, 'x', 0, 0, 0, 0, 0, 0, 0, 0, 0)",
                rusqlite::params![id, vec![id as u8; 20]],
            )
            .unwrap();
        };
        insert(&conn, 7);
        insert(&conn, 41);
        conn.execute("DELETE FROM torrents WHERE id = 41", [])
            .unwrap();
        migrate_with(&mut conn, None, MIGRATIONS).unwrap();
        let next: i64 = conn
            .query_row("SELECT next FROM id_seq", [], |r| r.get(0))
            .unwrap();
        assert_eq!(next, 42);
        let (sources, first): (i64, Option<i64>) = conn
            .query_row("SELECT sources, first_source FROM torrents", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((sources, first), (0, None), "pre-provenance rows");
    }

    #[test]
    fn applies_pending_migrations_and_snapshots_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let mut conn = Connection::open(&path).unwrap();
        migrate_with(&mut conn, Some(&path), &FAKE[..1]).unwrap();
        assert_eq!(version(&conn), 1);
        // A fresh database has nothing to protect: no snapshot.
        assert!(!dir.path().join("t.pre-v1.db").exists());

        migrate_with(&mut conn, Some(&path), FAKE).unwrap();
        assert_eq!(version(&conn), 2);
        let backup = Connection::open(dir.path().join("t.pre-v2.db")).unwrap();
        assert_eq!(version(&backup), 1);

        // Idempotent.
        migrate_with(&mut conn, Some(&path), FAKE).unwrap();
        assert_eq!(version(&conn), 2);
    }

    #[test]
    fn refuses_newer_schema() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate_with(&mut conn, None, FAKE).unwrap();
        let err = migrate_with(&mut conn, None, &FAKE[..1]).unwrap_err();
        assert!(matches!(
            err,
            Error::SchemaTooNew {
                found: 2,
                supported: 1
            }
        ));
    }

    #[test]
    fn failed_migration_rolls_back() {
        let mut conn = Connection::open_in_memory().unwrap();
        let broken: &[(i64, &str)] = &[(1, "CREATE TABLE ok (x INTEGER); CREATE TABLE ok (y);")];
        assert!(migrate_with(&mut conn, None, broken).is_err());
        let tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name = 'ok'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
    }

    /// M5 (V10) on a database at V9 with data in M4's tables: the data
    /// survives, the version advances once, foreign keys hold, the
    /// snapshot is taken, and reopening changes nothing.
    #[test]
    fn v10_upgrades_a_v9_database_with_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let mut conn = Connection::open(&path).unwrap();
        configure(&conn, 0).unwrap();
        migrate_with(&mut conn, Some(&path), &MIGRATIONS[..9]).unwrap();
        assert_eq!(version(&conn), 9);
        conn.execute_batch(
            "INSERT INTO torrents (id, info_hash, name, size, piece_len, piece_count,
                 file_count, info_size, flags, discovered_at, metadata_at, last_seen_at)
             VALUES (3, zeroblob(20), 'a', 1, 1, 1, 1, 1, 0, 0, 0, 0),
                    (8, x'0101010101010101010101010101010101010101', 'b', 1, 1, 1, 1, 1,
                     0, 0, 0, 0);
             DELETE FROM torrents WHERE id = 8;
             INSERT INTO torrents (id, info_hash, name, size, piece_len, piece_count,
                 file_count, info_size, flags, discovered_at, metadata_at, last_seen_at)
             VALUES (5, x'0202020202020202020202020202020202020202', 'c', 1, 1, 1, 1, 1,
                     0, 0, 0, 0);
             INSERT INTO torrent_parsed (torrent_id, parser_ver, data) VALUES (3, 4, x'aa');
             INSERT INTO swarm VALUES (3, 1, 2, 3, 1, 1, NULL, NULL, 10, 20, 0);
             INSERT INTO trackers (url, added_at) VALUES ('udp://t:80', 0);
             INSERT INTO filter_rules (position, query, action) VALUES (0, 'cam', 1);
             INSERT INTO class_corrections VALUES (5, 1, 0);
             INSERT INTO subscriptions (url, name, kind, action, interval_secs)
                 VALUES ('https://l', 'L', 0, 1, 86400);
             INSERT INTO settings VALUES ('crawler.intensity', '\"normal\"');",
        )
        .unwrap();
        let before = |conn: &Connection| -> i64 {
            conn.query_row(
                "SELECT (SELECT count(*) FROM torrents) + (SELECT count(*) FROM torrent_parsed)
                     + (SELECT count(*) FROM swarm) + (SELECT count(*) FROM trackers)
                     + (SELECT count(*) FROM filter_rules)
                     + (SELECT count(*) FROM class_corrections)
                     + (SELECT count(*) FROM subscriptions) + (SELECT count(*) FROM settings)",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        let rows = before(&conn);

        migrate_with(&mut conn, Some(&path), &MIGRATIONS[..10]).unwrap();
        assert_eq!(version(&conn), 10);
        assert_eq!(before(&conn), rows, "no data lost");
        let applied: i64 = conn
            .query_row(
                "SELECT count(*) FROM schema_migrations WHERE version = 10",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(applied, 1);
        let violations: i64 = conn
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(violations, 0);
        // Hour 0 holds what was stored before: those torrents' hour is
        // unknown.
        let seeded: (i64, i64) = conn
            .query_row("SELECT hour, last_id FROM index_watermark_hour", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(seeded, (0, 5));
        let resolved: Option<i64> = conn
            .query_row("SELECT resolved_at FROM torrent_parsed", [], |r| r.get(0))
            .unwrap();
        assert_eq!(resolved, None);
        let backup = Connection::open(dir.path().join("t.pre-v10.db")).unwrap();
        assert_eq!(version(&backup), 9);

        // Foreign keys are enforced on the new tables.
        let err = conn
            .execute(
                "INSERT INTO user_items (torrent_id, favourite, hidden, created_at, updated_at)
                 VALUES (8, 1, 0, 0, 0)",
                [],
            )
            .unwrap_err();
        assert_eq!(
            err.sqlite_error_code(),
            Some(rusqlite::ErrorCode::ConstraintViolation)
        );

        // A second open is a no-op: no new version, no new snapshot.
        drop(conn);
        let mut conn = Connection::open(&path).unwrap();
        configure(&conn, 0).unwrap();
        migrate_with(&mut conn, Some(&path), &MIGRATIONS[..10]).unwrap();
        let versions: i64 = conn
            .query_row("SELECT count(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(versions, 10);
        assert!(!dir.path().join("t.pre-v10.1.db").exists());
        assert!(!dir.path().join("t.pre-v11.db").exists());
    }

    #[test]
    fn v10_on_a_fresh_database() {
        let mut conn = Connection::open_in_memory().unwrap();
        configure(&conn, 0).unwrap();
        migrate_with(&mut conn, None, MIGRATIONS).unwrap();
        let seeded: (i64, i64) = conn
            .query_row("SELECT hour, last_id FROM index_watermark_hour", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(seeded, (0, 0));
        for table in [
            "saved_searches",
            "alerts",
            "user_items",
            "collections",
            "collection_items",
            "api_keys",
            "query_log",
        ] {
            let strict: i64 = conn
                .query_row(
                    "SELECT strict FROM pragma_table_list WHERE name = ?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(strict, 1, "{table} is STRICT");
        }
        // Releasing a rule's holds finds them by index, not a scan.
        for (table, index) in [
            ("quarantine", "quarantine_rule"),
            ("tombstones", "tombstones_rule"),
        ] {
            let plan: Vec<String> = conn
                .prepare(&format!(
                    "EXPLAIN QUERY PLAN SELECT hash FROM {table} WHERE rule IN ('a', 'b') LIMIT 10"
                ))
                .unwrap()
                .query_map([], |r| r.get(3))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            let plan = plan.join(" | ");
            assert!(plan.contains(index), "{table}: {plan}");
        }
    }

    /// V11 on a V10 database: `imported` is seeded with the searchable
    /// imports, compaction starts from nothing, fetch counting from the
    /// next hour, and deletes are counted from then on.
    #[test]
    fn v11_seeds_insights_state_from_a_v10_database() {
        let mut conn = Connection::open_in_memory().unwrap();
        configure(&conn, 0).unwrap();
        migrate_with(&mut conn, None, &MIGRATIONS[..10]).unwrap();
        conn.execute_batch(
            "INSERT INTO torrents (id, info_hash, name, size, piece_len, piece_count, file_count,
                 info_size, flags, discovered_at, metadata_at, last_seen_at, content_type)
             VALUES (1, zeroblob(20), 'a', 0, 0, 0, 0, 0, 32, 0, 0, 0, 3),
                    (2, randomblob(20), 'b', 0, 0, 0, 0, 0, 32, 0, 0, 0, NULL),
                    (3, randomblob(20), 'c', 0, 0, 0, 0, 0, 32, 0, 0, 0, 1),
                    (4, randomblob(20), 'd', 0, 0, 0, 0, 0, 0, 0, 0, 0, 1);
             UPDATE index_watermark_hour SET last_id = 2 WHERE hour = 0;",
        )
        .unwrap();
        migrate_with(&mut conn, None, MIGRATIONS).unwrap();
        let state = |k: &str| -> i64 {
            conn.query_row(
                "SELECT value FROM insights_state WHERE key = ?1",
                [k],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(state("imported"), 2, "ids 1 and 2: searchable imports");
        assert_eq!(state("compacted_before_day"), 0);
        assert_eq!(state("last_compaction_at"), 0);
        let now_h: i64 = conn
            .query_row("SELECT unixepoch() / 3600", [], |r| r.get(0))
            .unwrap();
        assert!((now_h..=now_h + 2).contains(&state("funnel_events_since_hour")));
        // A new store has nothing uncounted: fetches count from the start.
        let mut fresh = Connection::open_in_memory().unwrap();
        configure(&fresh, 0).unwrap();
        migrate_with(&mut fresh, None, MIGRATIONS).unwrap();
        let since: i64 = fresh
            .query_row(
                "SELECT value FROM insights_state WHERE key = 'funnel_events_since_hour'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(since, 0);
        // Id 3 isn't searchable: no removal. Id 1 is, and imported.
        conn.execute_batch("DELETE FROM torrents WHERE id IN (1, 3)")
            .unwrap();
        let removed: Vec<(i64, i64)> = conn
            .prepare("SELECT dim, value FROM rollup_hour WHERE metric = 15")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(removed, [(3, 1)]);
        assert_eq!(state("imported"), 1);
        for table in ["rollup_day", "cohort_day", "insights_state"] {
            let strict: i64 = conn
                .query_row(
                    "SELECT strict FROM pragma_table_list WHERE name = ?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(strict, 1, "{table} is STRICT");
        }
    }

    /// V13 on a V12 store writes no quality rows (no pass over
    /// `torrents`): each existing import reads as unknown (partial only
    /// where its file list was flagged partial, private only where its
    /// info dict said so), one whose info dict was fetched and verified
    /// since (`info_size` > 0) reads as verified, the crawler's rows read
    /// as verified, and nothing is queued or re-indexed.
    #[test]
    fn v13_reads_existing_imports_as_unknown_and_schedules_nothing() {
        use crate::import::{Completeness as C, Privacy as P, Verification as V, quality};
        let mut conn = Connection::open_in_memory().unwrap();
        migrate_with(&mut conn, None, &MIGRATIONS[..12]).unwrap();
        for (id, flags, info_size) in [(1, 1, 90), (2, 32 | 128 | 1, 0), (3, 32, 0), (4, 32, 90)] {
            conn.execute(
                "INSERT INTO torrents (id, info_hash, name, size, piece_len, piece_count,
                     file_count, info_size, flags, discovered_at, metadata_at, last_seen_at)
                 VALUES (?1, ?2, 'x', 1, 1, 1, 1, ?4, ?3, 0, 0, 0)",
                rusqlite::params![id, vec![id as u8; 20], flags, info_size],
            )
            .unwrap();
        }
        migrate_with(&mut conn, None, MIGRATIONS).unwrap();
        let read = |id| {
            let q = quality(&conn, id).unwrap().unwrap();
            (q.completeness, q.verification, q.privacy)
        };
        assert_eq!(read(1), (C::ReportedComplete, V::Verified, P::Private));
        assert_eq!(read(2), (C::Partial, V::Unknown, P::Private));
        assert_eq!(read(3), (C::Unknown, V::Unknown, P::Unknown));
        assert_eq!(read(4), (C::ReportedComplete, V::Verified, P::Public));
        for table in [
            "import_quality",
            "fetch_queue",
            "reindex_log",
            "import_jobs",
        ] {
            let n: i64 = conn
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{table}");
        }
    }

    /// The v0.5.1 code path (migrations through V12) opening a V13 store
    /// refuses it as newer and changes nothing: no migration, no
    /// snapshot, the file as it was.
    #[test]
    fn a_v12_build_refuses_a_v13_store_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lodestar.db");
        drop(crate::Store::open(&path, crate::StoreConfig::default()).unwrap());
        let listing = || {
            let mut names: Vec<_> = std::fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            names.sort();
            names
        };
        let (files, bytes) = (listing(), std::fs::read(&path).unwrap());

        let mut conn = Connection::open(&path).unwrap();
        configure(&conn, 0).unwrap();
        let err = migrate_with(&mut conn, Some(&path), &MIGRATIONS[..12]).unwrap_err();
        assert!(
            matches!(
                err,
                Error::SchemaTooNew {
                    found: 13,
                    supported: 12
                }
            ),
            "{err:?}"
        );
        assert!(err.to_string().contains("newer than this build supports"));
        assert_eq!(version(&conn), 13);
        drop(conn);
        assert_eq!(listing(), files, "no snapshot or side file left");
        assert!(
            std::fs::read(&path).unwrap() == bytes,
            "the file is unchanged"
        );
        // v0.5.1's read-only connections refuse it too.
        let reader =
            Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert!(matches!(
            check_current_with(&reader, 12),
            Err(Error::SchemaTooNew {
                found: 13,
                supported: 12
            })
        ));
        drop(reader);
    }

    /// A crash mid-`VACUUM INTO` leaves only a `.tmp`, never a file under
    /// a snapshot's name; the next migration removes it and takes a whole
    /// snapshot. A snapshot that can't be written stops the migration.
    #[test]
    fn the_snapshot_is_written_aside_then_renamed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let mut conn = Connection::open(&path).unwrap();
        migrate_with(&mut conn, Some(&path), &FAKE[..1]).unwrap();
        let stale = dir.path().join("t.pre-v2.db.tmp");
        std::fs::write(&stale, b"half a snapshot").unwrap();
        migrate_with(&mut conn, Some(&path), FAKE).unwrap();
        assert!(!stale.exists());
        let snap = Connection::open(dir.path().join("t.pre-v2.db")).unwrap();
        assert_eq!(version(&snap), 1);
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
        assert_eq!(latest_schema_version(), MIGRATIONS.len() as i64);

        // The .tmp can't be written (a folder stands in its way): no
        // snapshot appears and nothing migrates.
        let path = dir.path().join("u.db");
        let mut conn = Connection::open(&path).unwrap();
        migrate_with(&mut conn, Some(&path), &FAKE[..1]).unwrap();
        std::fs::create_dir(dir.path().join("u.pre-v2.db.tmp")).unwrap();
        std::fs::write(dir.path().join("u.pre-v2.db.tmp").join("x"), b"x").unwrap();
        assert!(migrate_with(&mut conn, Some(&path), FAKE).is_err());
        assert!(!dir.path().join("u.pre-v2.db").exists());
        assert_eq!(version(&conn), 1);
    }

    #[test]
    fn existing_snapshot_does_not_block_migration() {
        // A migration that failed after its snapshot leaves the file behind;
        // the retry must still be able to snapshot and migrate.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let mut conn = Connection::open(&path).unwrap();
        migrate_with(&mut conn, Some(&path), &FAKE[..1]).unwrap();
        std::fs::write(dir.path().join("t.pre-v2.db"), b"older snapshot").unwrap();
        migrate_with(&mut conn, Some(&path), FAKE).unwrap();
        assert_eq!(version(&conn), 2);
        assert_eq!(
            std::fs::read(dir.path().join("t.pre-v2.db")).unwrap(),
            b"older snapshot",
            "existing snapshots are never overwritten"
        );
        let backup = Connection::open(dir.path().join("t.pre-v2.1.db")).unwrap();
        assert_eq!(version(&backup), 1);
    }
}
