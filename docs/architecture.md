# Lodestar 2 — A From-Scratch DHT Crawler & Torrent Search Engine

> Working name "Lodestar". It's a ground-up replacement for bitmagnet + the Lodestar frontend. It lives in a **new repo** you'll create and attach; the first commit puts this plan in as `docs/architecture.md`.
> Decisions so far: **Rust single binary**, **Svelte 5 UI (smallest/fastest first) governed by `dvortsis/skills/ui-design-skills`, applied by spirit where a skill assumes React/Tremor**, **crawler-first with an optional one-time bitmagnet import (M2 add-on)**, **external enrichment as opt-in plugins**.

---

## 1. Context

Lodestar (github.com/dvortsis/lodestar) is a Next.js + Apollo frontend on top of bitmagnet's PostgreSQL. It added PGroonga indexes, two "sidecar" tables and a cron "janitor" to get better search. It does have real strengths: a composition bar, staged advanced filters, file-path search, an 8-tier relevance waterfall and a dense accordion UI. But performance and search quality fell short, and the review below explains why. The cause is architectural, not cosmetic: the search system was bolted onto a database designed for a different purpose.

The goal is to build the whole pipeline from scratch (crawler → metadata → storage → search → UI). Every layer should be designed for:
1. **Pinpoint search over ambiguously named torrents.** Search by file contents, file types, size, dates/times, swarm health and tracker count.
2. **Maximum metadata capture** from each torrent.
3. **Performance and a small database.**
4. **First-class UX.**
5. **Easy deployment** as a Docker container, an Unraid CA app, or natively on Windows.

## 2. What the Lodestar review found (and how this design fixes it)

| # | Problem in Lodestar / bitmagnet stack | Consequence | Fix in new design |
|---|---|---|---|
| 1 | `torrent_search_indexes.search_text` concatenates name + **every file path**. It's indexed 3× (PGroonga bigram on name, PGroonga bigram on `search_text`, and an English tsvector GIN that the service never uses). | Large duplicated storage; bigram on long text = huge, noisy index | One purpose-built inverted index (Tantivy). File paths are indexed once in a compact positional field (with a gap between files), and per-file precision (size/ext per file) goes in a separate "smart" file index. |
| 2 | Ranking runs `strpos`/`ILIKE` over up to 500 KB of `search_text` for **every candidate** across 8 CASE tiers, plus lateral word-count joins | CPU cost grows with hit count, so common words are slow | Per-field BM25 with boosts + phrase/proximity bonuses, scored inside the index with block-max WAND. Quality priors (seeders, freshness) are applied as a **second-phase re-rank of the top ~1,000** so they don't defeat WAND pruning. Top-k cost grows with the candidate window, not the hit count (§8). |
| 3 | A separate `COUNT(*)` re-runs the whole PGroonga match | Double work on every search | No second query. Counts are exact up to a cap and estimated above it, and facets load in a separate, cancellable request (§8), so broad queries never block the first result paint. |
| 4 | Composition filters are correlated subqueries per row; percentages computed at query time; size-based % silently dropped | Unindexed filters; slow; a feature was lost | Per-category **count, bytes and percent precomputed** as columnar fast fields; count- and size-based thresholds both supported |
| 5 | Category maps disagree (`janitor.js` `get_custom_category` vs `fix-db.js`: `img`/`doc` vs `image`/`document`, missing `webm`, `iso` counted as archive, etc.) | Inconsistent filtering | One versioned extension → category registry (Rust, ~600 extensions) used everywhere; re-categorization is a background re-derive |
| 6 | Janitor is an anti-join batch poll; compositions use `ON CONFLICT DO NOTHING` | Index goes stale; compositions can be wrong forever | An event-driven in-process pipeline: a torrent becomes searchable within seconds of its metadata arriving, and every derived field is recomputable |
| 7 | Mutates bitmagnet's schema (drops triggers/columns, adds columns) | Breaks on upstream upgrades | We own the schema end to end, with migrations |
| 8 | Next.js route → Apollo client → own GraphQL route → pg; `/api/stats` has a hard-coded `setTimeout(5000)`; full SQL logged per request | Extra hops, artificial latency, log spam | Rust HTTP API serves JSON directly; the SPA is embedded in the binary; structured, levelled logs |
| 9 | OFFSET pagination capped at 100 pages | Deep pages slow or unreachable | Cursor (`search_after`) pagination; infinite scroll |
| 10 | Only one time axis (discovered); no tracker data; seeders only from stale bitmagnet sources | Can't filter by tracker count or real swarm health | Multiple time axes + an active swarm-health subsystem (DHT scrape + UDP tracker scrape) with a **trackers-reporting count** |
| 11 | No facets, saved searches, alerts or live updates | Discovery is manual | Facet counts on every search, saved searches with alerts (percolation), and a live SSE feed |

**Keep from Lodestar:** the composition bar, the dense accordion/card layout, the file tree with lazy loading, the synthesized single-file entry (native in our model), exact info-hash lookup, the boolean/phrase/prefix/proximity/regex syntax, title-beats-files ranking with capped file contributions, alternate titles, and i18n (en + zh).

## 3. Goals, non-goals, budgets

**Non-goals:** downloading torrent *content* (we only fetch the info dict via BEP 9), so there's no real mediainfo, duration or screenshots. No multi-node clustering in v1. No public-facing hosting features.

**Hard truths about DHT-sourced metadata (UX must be honest about these):**
- BEP 9 delivers only the **info dictionary**. `announce`/`announce-list` (trackers), `creation date`, `comment`, `created by` and `url-list` are **outside** the info dict and are therefore unavailable for DHT-discovered torrents.
- "Number of trackers" is therefore defined as **trackers that report this hash when scraped** (from a configurable tracker list). We also track the DHT swarm estimate (BEP 33). Real announce lists, creation dates and comments are captured when a `.torrent` is imported (folder watch / upload / opt-in importers).

**Performance targets** (measured on a synthetic 10M-torrent / 300M-file dataset, 4 cores, NVMe; enforced as CI benchmark gates):
| Operation | p95 target |
|---|---|
| Typeahead / suggest | < 15 ms |
| Keyword search, top 50 (count exact ≤ 100k hits, "≈" above) | < 30 ms |
| Facets for the same query (separate request; sampled above 1M hits) | < 100 ms |
| Filter-only browse over the whole corpus (cached per index generation) | < 50 ms |
| File-level query joined to torrents | < 150 ms |
| Full-name regex (trigram prefilter + verify) | < 500 ms |
| New torrent → searchable | < 5 s |
| Web UI first load | LCP < 1 s (LAN) / < 2.5 s (4G phone); ≤ 100 KB gzipped total first load (JS ≤ 45 KB); INP < 100 ms (§10.1) |
| Insights overview / stage-page queries (rollups only) | < 50 ms |
| **Reporting overhead** with Insights open and the digest job running | search p95 +≤ 5%; ingest throughput −≤ 2%; rollup storage ≤ 50 MB/year |
| **Coordinator ingest** with remote workers (commit + index, §19) | ≥ 1,000 new verified torrents/s sustained; grant latency p95 < 50 ms (LAN) |
| **Worker** (§19) | RSS ≤ 256 MB at defaults; ≤ 3 KB per torrent on the wire (median) |

**Size budget** (targets to verify in M0/M2 benchmarks, at a 30-file average):
- Canonical store + torrent index: **≤ 1.5 KB per torrent**.
- File index in "smart" mode (default; §8): **≤ 0.8 KB per torrent**. In "all files" mode it's about 40–60 B per file, which can double the total. The settings UI shows the projected size before you switch modes.
- Raw info-dict archive: **off by default.** If enabled, it stores the info dict **minus the `pieces` field**, at roughly +0.3–1 KB per torrent. A full info dict is dominated by `pieces`: 20 incompressible bytes per piece, often 10–60 KB per torrent, which would mean hundreds of GB at 10M torrents. `pieces` carries no searchable metadata, so it's never stored; we keep only the piece count and length.
- **Honest all-in figure: about 2.3 KB per torrent by default (≈ 23 GB per 10M torrents).** That's a target, not a promise, and the M0 benchmarks will confirm or revise it.
- **M0 allocation:** of the 1.5 KB, the canonical store (the whole SQLite file) gets **≤ 1.0 KB** and the torrent index **≤ 0.5 KB**. Within the store, file-tree blobs get **≤ 0.6 KB** mean. CI enforces both store figures with `size-report --gate` on 50k synthetic torrents. M0 measured, on 100k synthetic torrents averaging 31.3 files: **344 B** blob mean and **565 B** SQLite file per torrent (§16).

Steady-state RSS **< 1 GB** at 10M torrents with default caches. A **low-resource mode** (for N100/ARM/HDD-backed Unraid boxes) turns off the trigram field and the file index and shrinks caches and merge threads.

**Cold-start reality (fresh crawler):** a rough expectation, to be measured in M1, is hundreds of thousands to a few million hashes harvested per day, of which only a minority yield metadata (many hashes are dead swarms). The index grows over weeks, not hours. The dashboard projects growth, and the first-run wizard sets this expectation and offers the **bitmagnet import** (M2) as a shortcut.

## 4. Topology

One process, one binary (`lodestar`), with one data directory. Internally it's a set of async services connected by bounded channels, so backpressure is natural and nothing polls. **Optional remote workers** (§19) run the same binary in worker mode on other machines and add crawling, fetching and processing capacity. This single process becomes the **coordinator** and keeps the database, index and UI.

```
             UDP :6881 (DHT, v4+v6)              TCP/uTP :6881 (peer wire)
                    │                                     ▲
 ┌──────────────────▼───────────────┐   infohash+peers    │
 │ dht: routing table, KRPC, BEP 5/ │──────────────────►┌─┴──────────────────────┐
 │ 32/42/51 harvest, passive listen │  (dedup: cuckoo   │ fetch: BEP 3/10/9      │
 │ BEP 33 scrape                    │   filter + LRU)   │ metadata exchange, v1/ │
 └──────────────────────────────────┘                   │ v2 hash verify         │
                                                        └──────────┬─────────────┘
                                            raw info dict (bytes)  │
 ┌─────────────────────────────────────────────────────────────────▼────────────┐
 │ enrich (rayon CPU pool): metainfo parse → file tree → categories/composition  │
 │ → release-name parser (name + dominant files) → content type → junk score     │
 │ → grouping keys/file signatures → (opt-in plugins: TMDB, MusicBrainz…)        │
 └───────────────┬──────────────────────────────────────────────────────────────┘
                 │ batches (≤500 ms / 1k rows)
 ┌───────────────▼──────────────┐   ┌──────────────────────────┐  ┌─────────────┐
 │ store: SQLite (WAL, single   │──►│ index: Tantivy torrent + │─►│ percolator  │─► SSE / alerts
 │ writer task), zstd-dict file │   │ file indexes, NRT commit │  │ saved search│
 │ blobs, hot-column mmap arrays│   └──────────────────────────┘  └─────────────┘
 └───────────────▲──────────────┘
                 │ swarm stats
 ┌───────────────┴──────────────┐   ┌──────────────────────────────────────────┐
 │ health: scheduler, UDP/HTTP  │   │ api (axum): REST /api/v1, SSE, Torznab,   │
 │ tracker scrape (BEP 15/48),  │   │ OpenAPI, /metrics, embedded SPA (Svelte)  │
 │ DHT scrape, decay schedule   │   └──────────────────────────────────────────┘
 └──────────────────────────────┘
```

Data dir layout: `data/lodestar.db` (SQLite), `data/index/torrents/`, `data/index/files/`, `data/hot/*.col` (mmap'd stats columns), `data/dht/state.bin` (routing table + node IDs), `config.toml`.

## 5. DHT crawler

The Mainline DHT crawler is **source plugin #1**. Every other discovery source, importer and network tier plugs into the same plugin API and crawl scheduler (§18).

- **Stack:** tokio and our own zero-copy bencode/KRPC codec. Spike reuse of `librqbit` crates (dht, peer-protocol, bencode) in M1; adopt them only if they support BEP 51 traversal and high fan-out without forking.
- **BEPs:** 5 (DHT), 32 (IPv6), 42 (secure node IDs), 43 (read-only flag respected), **51 (`sample_infohashes`) as the primary, polite harvester**, 33 (DHT scrape bloom filters → seed/peer estimates).
- **Harvest strategies** (each toggleable, with metrics per strategy):
  1. BEP 51 traversal: walk the keyspace, sample infohashes and honour each node's `interval`.
  2. Passive: answer `get_peers`/`announce_peer` and record hashes. `announce_peer` comes with a live peer, so it's the fastest path to metadata.
  3. Active `get_peers` lookups to find peers for hashes that have no peer yet.
- **Virtual node IDs:** 1–16 BEP 42-compliant IDs on one socket to widen keyspace coverage (default 4). BEP 42 allows only **8 id prefixes per external IP**, so beyond that, coverage grows with **more public IPs**, i.e. remote workers on other networks or VPN exits (§19.1). Use "Low / Balanced / Aggressive" presets that cap packets/sec, bandwidth and concurrent fetches. Be a good DHT citizen by default.
- **Dedup:** a persistent "have" cuckoo filter (~1.5–2 bytes per stored hash), a time-rotated "seen" filter (see *Bounded memory* below), an in-flight set, and a failure LRU with exponential backoff (retry later when new peers appear).
- **Routing table** persisted on shutdown for a warm restart. Bootstrap from the standard routers plus the last-known good nodes.
- **Optional UPnP / NAT-PMP** port mapping (off in Docker by default, on for Windows). The dashboard shows whether the node is reachable.
- **External IP discovery** from the `ip` key in KRPC responses (BEP 42), so the virtual IDs stay valid behind NAT and VPNs.
- **Home-router protection:** DHT fan-out can exhaust consumer-router/NAT **conntrack tables** and knock the household offline. Defaults are conservative, there's a global outstanding-UDP-query cap, and the dashboard warns if the reply ratio collapses (a symptom of NAT saturation).
- **VPN-first deployment:** bind to a chosen interface/IP, an optional kill-switch (refuse to crawl unless bound to the VPN interface), and documented `network_mode: service:gluetun` + VPN port-forward setups. Many Unraid users will run it this way.
- **Rediscovery is a free liveness signal:** when a known hash shows up again in the DHT, bump `last_seen` (hot column, no disk write beyond the mmap) and raise its scrape priority. Swarm health then follows real activity instead of blind sweeps.
- **Proxying:** an outbound SOCKS5 option for peer-wire TCP. DHT UDP needs UDP ASSOCIATE, which most VPN SOCKS endpoints don't support, so DHT stays on the bound interface. In v1.x, evaluate an **embedded userspace WireGuard tunnel** (boringtun) so crawler traffic goes through the VPN while the UI/Torznab stay on the LAN, with no gluetun container needed (especially valuable on Windows).
- **Bounded memory:** the "seen" filter is time-rotated (two generations), so it can't grow without limit from the flood of dead hashes BEP 51 returns. A persistent "have" filter covers stored torrents. The pending-metadata queue is persisted (capped, priority-ordered) so restarts don't drop work.

## 6. Metadata fetch & capture ("grab everything")

- **Peer wire:** BEP 3 handshake with the extension bit → BEP 10 extended handshake → BEP 9 `ut_metadata` piece requests → SHA-1 verify (v1) / SHA-256 (v2 `btmh`). **TCP first (M1), uTP (BEP 29) in M7.** The UI shows the success rate so the uTP gain is measurable.
- **MSE/PE (Message Stream Encryption)** handshake with plaintext fallback. Many peers run "require encryption", and without MSE those fetches simply fail. This ships in M1, not later.
- Concurrency budget (default 400 sockets), per-IP limits, 8 s timeouts, and tries multiple peers per hash. Peers for a hash are refreshed via `get_peers` before retrying.
- **Peer finding beyond the DHT** (accelerator plugins, §18): when `get_peers` returns nobody, ask a few public UDP trackers for peers (BEP 15 announce, rate-limited, only when the DHT is empty). During a fetch, use **PEX (BEP 11)** to learn more peers from the one we're connected to. Both raise the metadata success rate, especially for keys from scrapes, dumps and Nostr.
  - **A tracker announce publishes our IP in that swarm's peer list.** That's what copyright monitors collect, so it changes the legal exposure the way the micro-piece prober does (§6). Tracker-announce peer finding is therefore **off by default**, behind a consent screen (class *Restricted*, §18.7), and always uses `event=stopped` semantics where trackers allow it. PEX only reads peers a connected peer sends us, so it stays on.
  - **Private keys never use public peer finding.** Keys flagged `private_source` (§18.7) are never sent to public DHT lookups, public tracker announces or PEX. BEP 27 forbids DHT and PEX for private torrents, and a lookup would leak the hash to strangers.
- **Hostile-input limits:** the metadata size cap is 16 MB, configurable, and anything larger is recorded as "oversize" with hash and size only. There are limits on bencode depth and list length, file count, and path length. Fuzzing (`cargo fuzz`) covers bencode, KRPC and the metainfo parser.
- **Encodings:** use `name.utf-8`/`path.utf-8` when present; otherwise detect legacy charsets (GBK, Big5, Shift-JIS, CP1251) with `chardetng` + `encoding_rs`. The raw bytes are kept so decoding can be redone.
- **v2/hybrid identity:** DHT announces v2 swarms under the *truncated* 32→20-byte hash. Both forms map to one torrent id, so hybrid torrents never appear twice.
- **Re-derivation without the raw dict:** the file-tree blob plus `torrent_extra` (unknown keys) plus the raw name/path bytes already allow every derived field to be recomputed when parsers improve. Archiving the info dict minus `pieces` is an opt-in setting with a size readout (§3). `pieces` is never stored.

**Captured per torrent:**
| Group | Fields |
|---|---|
| Identity | info_hash v1 (20 B), info_hash v2 (32 B, hybrid/v2), meta version, hybrid flag |
| Info dict | name (+ `name.utf-8`), piece length, piece count, total size, private flag, `source` field, any **unknown/extra keys** (e.g. `x_cross_seed`, `publisher`, `publisher-url`) kept as a small bencode blob, info-dict size |
| Files | path segments, size, BEP 47 attrs (padding/hidden/exec/symlink + target), per-file `pieces root` (v2 → exact file identity), file order |
| Derived | per-category count/bytes/percent, dominant category, largest file (name/size), directory depth, distinct extensions, padding ratio |
| Parsed release | title, alt titles, year, season/episode (ranges, absolute, date-based), resolution, source (REMUX/BluRay/WEB-DL…), video codec, bit depth, HDR/DV, audio codec + channels, languages, subtitles, edition, release group, container, pack/complete flag; music (artist/album/format/bitrate), software (version/platform/arch), books (author/format), games (platform/repack) |
| Classification | content type + confidence, junk score + reasons, adult flag, grouping key, file-set signature |
| Time | discovered_at, metadata_at, first_peer_seen_at, last_seen_at (DHT activity), last_scraped_at — all at second precision, UTC. Plus an **`age_confidence`** flag: torrents first seen during the crawler's warm-up window (default the first 30 days) are marked "backfill". Their discovered date only means "when *we* first saw it", and date filters/UI say so. An **estimated release year** comes from the parser year or the `.torrent` creation date. A coarse **age hint** comes from tracker `completed` counts. Both are separate fields from discovered_at. |
| Swarm | seeders/leechers/completed (max & per tracker), **trackers_reporting count**, DHT seed/peer estimate (BEP 33), history (downsampled), trend |
| From `.torrent` imports only | announce list (count + URLs), creation date, created by, comment, url-list/web seeds |
| User | favorite, hidden, labels, notes, collections, "sent to client" timestamp |

**Ambiguous names are the core use case.** The release parser runs on the torrent name **and** on the dominant file(s) and top folder, and picks the most informative result as the *effective title*. For example, a torrent named `a8f3x.zip.part` whose files are `Show.Name.S02E05.2160p.WEB-DL.mkv` becomes searchable as that show. Both raw and effective titles are indexed and displayed.

**Junk/spam score** (0–100, with human-readable reasons): executables or `.lnk`/`.scr`/`.url` files in a media torrent, `password.txt`/"visit www…" lure files, a name that doesn't match the content, tiny "video" files, known malware filename patterns, padding-only torrents. Results above the threshold are hidden by default, with a "show N hidden" toggle and a "why" tooltip.

**Grouping/dedup:** the grouping key (normalized title + year + S/E) plus a MinHash over the major file sizes give "N other versions of this" clusters. There's also an **exact payload signature** (`payload_sig` u64 = hash of the sorted vector of non-padding file sizes) as a fast field, so "identical payload under a different name" is one term lookup (`sig:` / "Identical clones" panel). Exact per-file identity comes from a v2 `pieces root` **or the optional v1 per-file `md5sum`/`sha1`/`ed2k` keys** that some creators embed (captured whenever present).

**Structural fingerprints** (from the file tree alone, no content download): BDMV/`BDMV/STREAM` (Blu-ray), `VIDEO_TS` (DVD), cue+flac/ape (image rips), `.sfv`/`.par2` sets (scene/Usenet style), multi-part `.rNN`/`.partN.rar`, Steam depot/`*.vpk`, game repack layouts (`setup.exe` + `.bin` parts), `Sample/`/`Subs/`/`Proof/` folders, comic `.cbz` sets, photo sets (EXIF-style names). They feed content type, the junk score and `has:` filters.

**Ambiguity flag:** when the raw name is uninformative (hash-like, `New Folder (2)`, `Complete Season`, generic), the torrent gets `ambiguous=true`. The result records **how it was resolved** (dominant file / folder / structure / plugin) plus a confidence score. The UI shows the resolved title next to the raw name with a "resolved via …" tooltip, and power users can filter `ambiguous:yes`.

**Opt-in enrichment plugins** (off by default, rate-limited, cached; full ranked list and roadmap in §18.2–18.3): srrDB, the torrent cache (real `creation date` / `announce-list` for a hash), Wikidata, TMDB/TVDB, AniDB/AniList, MusicBrainz, PreDB, OpenLibrary, IGDB, Discogs, Steam, and debrid cache status. They add name hints, canonical titles, IDs, multilingual alternative titles and genres, each with provenance and confidence. They run through the same plugin API and scheduler (§18).

**Opt-in micro-piece prober (v1.x, off by default, explicit consent screen):** for torrents still ambiguous after file-tree analysis, request just the blocks covering a small text file (`.nfo`/`.diz`/`.sfv`/`.cue`, ≤ 64 KB). Scene NFOs often contain a full mediainfo dump and an IMDb ID. A second, separately consented tier fetches container headers: MKV EBML/Tracks, FLAC STREAMINFO, MP4 `moov` (only when it's at the file start). Caveats the consent screen must state plainly:
1. This **downloads content fragments and makes the crawler a swarm participant**, which changes the legal exposure in many jurisdictions. Never on by default; strongly advised only behind a VPN.
2. Peers rarely unchoke a non-seeding stranger, so expect a low success rate.
3. Block-level data can't be hash-verified without the whole piece, so probed facts are labelled **"unverified"** and down-weighted. Verification would need `pieces`, which is kept only in memory during the probe and never stored.

Text extracted from NFOs is indexed into an `nfo_text` field (boost ~2).

**Safety:** hash and name-pattern **blocklists** with import of published hash lists, plus "never index" categories applied **before** storage.

## 7. Storage (SQLite = source of truth)

**Why SQLite:** a single file, zero ops, native on Windows, online backup API and fast reads. The write load (tens of torrents/s plus batched scrape updates) is trivial for one writer task with batched transactions. Tantivy indexes and hot columns can always be **rebuilt from SQLite** (`lodestar reindex`).

**Pragmas:** WAL, `synchronous=NORMAL`, `page_size=16384`, `mmap_size` sized to RAM, `auto_vacuum=INCREMENTAL`, STRICT tables. Reads go through a pool of read-only connections.

**Schema sketch** (integer surrogate `id` everywhere; the 20-byte hash appears only once). Ids are **monotonic and never reused** (`AUTOINCREMENT`). Hot-column slots, Tantivy docs and the percolator's "new docs = id > watermark" logic all depend on a deleted id never coming back with stale data. Plain SQLite rowids can be reused after deletes.
- `torrents(id INTEGER PK, info_hash BLOB UNIQUE, info_hash_v2 BLOB, name TEXT, name_raw BLOB, name_enc INT /*raw bytes + encoding id, only when the name isn't plain UTF-8*/, size INT, info_size INT, piece_len INT, piece_count INT, file_count INT, flags INT /*bitset: private,v2,hybrid,padding,symlinks,imported…*/, discovered_at INT, metadata_at INT, last_seen_at INT, content_type INT, junk_score INT, group_key INT, comp BLOB /*packed per-category count+bytes*/)`
- `torrent_parsed(torrent_id PK, parser_ver INT, data BLOB /*msgpack of parsed fields*/)`. The hot fields (year, resolution, …) are also flattened into the index.
- `torrent_files(torrent_id PK, codec INT, dict_id INT, data BLOB)`: **one compressed blob per torrent**, never one row per file (bitmagnet's per-file rows are its biggest table). Format: an interned directory table + entries `(parent varint, name, size varint, attrs, pieces_root?)`, then **zstd with a trained dictionary** (versioned dictionaries in `zstd_dicts`).
- `torrent_raw(torrent_id PK, data BLOB)`: the info dict minus `pieces` (opt-in, off by default).
- `torrent_extra(torrent_id PK, data BLOB)`: unknown info keys + `.torrent`-only fields.
- `swarm(torrent_id PK, seeders, leechers, completed, trackers_reporting, dht_seeds, dht_peers, last_scrape_at, next_scrape_at)` plus `swarm_trackers(torrent_id PK, data BLOB)` for the per-tracker detail, packed as `(tracker_id u16, s, l, c, at)` and only for torrents with non-zero results.
- `swarm_history(torrent_id PK, data BLOB)`: a **fixed-size ring buffer** per torrent (e.g. 14 daily + 26 weekly samples of seeders and leechers as `u16` ≈ 160 B), stored **only for torrents with a non-zero swarm or a user interest** (favorited/viewed/saved-search hit). A naive daily-row table would reach billions of rows within a year.
- **Sources and provenance (§18.5):** `torrents.sources` (u64 bitset) + `first_source` (u8); `sources` (plugin registry and state); `torrent_claims(torrent_id, source_id, data BLOB)`; `unverified_items(id, network, key, source_id, claims BLOB, first_at, last_at)` for keys with claims but no verified metadata yet. Overlay networks (§18.6) keep their own item tables but share the one monotonic id sequence.
- `trackers`, `saved_searches`, `alerts`, `user_items` (fav/hidden/labels/notes), `collections`, `settings`, `blocklist`, `zstd_dicts`, `schema_migrations`.

**Hot columns:** `data/hot/{seeders,leechers,trackers,last_seen,user_flags}.col` are mmap'd `u32` arrays indexed by torrent id, read and written as atomics so there are no locks between the scraper and searchers. Seeders/trackers use a sentinel for **"never scraped"**, so the UI shows "—" rather than a misleading 0. Swarm stats change constantly; putting them in Tantivy would force delete+reinsert of whole documents. Instead, search collectors read these arrays (a nanosecond lookup) for filtering, sorting and ranking. SQLite stays authoritative, and the arrays are rebuilt on startup if they are inconsistent.

**Insights rollups (reporting without touching raw data):** every number on Insights (§10.9) comes from small pre-aggregated tables. They're maintained incrementally by the pipeline, so reports never scan `torrents` or the search index.
- **Tables:**
  - `rollup_hour(hour INT, metric INT, dim INT, key INT, value INT, PRIMARY KEY(hour, metric, dim, key)) WITHOUT ROWID`: event-time counters and gauges. Examples: keys harvested by source, strategy, network and IP family; unique finds per source; scheduler shares and auto-balance moves; fetch outcomes; newly searchable by category, content type and resolution method; scrape results; searches, zero results and sends. Latency histograms are stored as log buckets (`key` = bucket).
  - `cohort_hour(cohort INT, stage INT, reason INT, count INT, PRIMARY KEY(cohort, stage, reason)) WITHOUT ROWID`: the discovery-cohort funnel. `stage` ∈ harvested / peer found / metadata fetched / stored / searchable, plus terminal loss `reason`s.
  - `rollup_day` / `cohort_day`: the same shapes at daily grain.
  - `events(at INT, kind INT, detail TEXT)`: trend annotations (restart, upgrade, config/intensity change, import, reindex, pause).
- **Cohort attribution:**
  - Each in-flight hash carries its `cohort` hour (u32) and harvest strategy (u8) in the pending-queue record (§5), so later fetch outcomes are credited to the right cohort.
  - Stored torrents derive their cohort from `discovered_at`.
  - A hash that leaves the queue without metadata is recorded once with its terminal loss reason, including **evicted** (dropped from the capped queue) and **gave up** (retry horizon passed with no peers).
  - **Harvested means new to us:** a hash enters a cohort only when it's neither stored (have-filter) nor already in flight. Rediscoveries of stored torrents only bump `last_seen` (§5).
- **Write path:**
  - Pipeline stages increment **in-memory per-hour accumulators** (lock-free counters, a few hundred keys).
  - The writer flushes them as `INSERT … ON CONFLICT DO UPDATE SET value = value + excluded.value` **inside its existing batched transaction**, at most every 5 s. If no batch has committed for 5 s while counters are dirty (cold start, a run of failed fetches, a paused fetcher with the DHT still harvesting), the writer commits a small counters-only transaction instead, so the flush cadence and the "stale" signal never depend on ingest.
  - There are no extra fsyncs or transactions; the per-torrent cost is a few counter increments.
  - **Searchable time without a per-torrent column:** the index commits in id order under the shared watermark (§8), so recording the highest id made searchable each hour is enough: `index_watermark_hour(hour INT PRIMARY KEY, last_id INT)`, about 9k rows a year. A torrent's searchable hour is the first hour whose `last_id` ≥ its id. A background rebuild (§8) doesn't touch it, because the old index keeps serving.
  - A crash loses at most ~5 s of pre-storage (Discover/Fetch) counts. Stored counts (from `torrents`) and searchable counts (from `torrents` joined to `index_watermark_hour`) can be recomputed exactly, at hourly grain, with `lodestar rollup --repair`. It also runs automatically after an unclean shutdown.
- **Compaction:** hourly rows older than 30 days are folded into daily rows by a nightly job, in short chunked transactions (no long read transactions, so the WAL checkpoint is never starved). Expected size: **≤ 50 MB/year** (hundreds of keys per hour).
- **Read path:** the Insights queries are indexed range scans over the rollup tables (**≤ 50 ms p95**, independent of corpus size), cached per (query, last flush). Heavy exports run as background jobs that stream in chunks with I/O throttling and report progress.

**Retention policies** (settings UI, each with a size preview): drop junk ≥ X, drop torrents never seen in N days with 0 seeders, drop categories, cap file-index coverage. Run with incremental vacuum.

## 8. Search engine (Tantivy)

**Torrent index** (one doc per torrent; only `id` and `name` are stored, and all other display data comes from SQLite by id):
- Text: `name` (positions, boost 3, also **stored** in the compressed docstore for regex verification and snippets without SQLite round-trips), `title` (effective/parsed title, positions, boost 4), `alt_titles` (boost 2), `path_tokens` (each distinct directory name once, then every file name; **words only, without positions**, boost 1. Phrase/NEAR within one file name runs on the file index, whose `name` keeps positions. Positions here measured 372 B per torrent, over the whole index budget; see §16), `name_trigram` (for substring/infix and the regex prefilter; turned off in low-resource mode).
- Keyword/fast fields: `ext[]`, `category[]`, `content_type`, `resolution`, `source`, `vcodec`, `acodec`, `hdr`, `lang[]`, `group`, `year`, `season`, `episode`, `flags`.
- Numeric fast fields: `size`, `file_count`, `largest_file`, `piece_len`, `depth`, `cat_{count,bytes,pct}_{video,audio,image,document,ebook,archive,disc,software,subtitle,other}`, `junk`, `group_key`.
- Dates: `discovered_at`, `metadata_at`.
- From hot columns (not indexed): `seeders`, `leechers`, `trackers`, `last_seen`, user flags.

**File index** (one doc per file; modes: *off* / **smart (default)** / *all*. Smart indexes every file in torrents with ≤ 1,000 files, plus files ≥ 5 MB or in a non-"other" category in larger packs. Padding, `.nfo`/`.txt`/thumbs junk in big packs, and BEP 47 hidden files are skipped. Torrent-level `path_tokens` still covers everything that's skipped.) `torrent_id` (fast), `name` (positions), `dir_tokens`, `ext`, `category`, `size` (fast), `pieces_root` (bytes). A file-level clause (`file:(…)`) runs here and produces a roaring bitmap of torrent ids, which filters the torrent query. Each hit returns **"matched files"** for the UI.

**Tokenizer chain** (the most important quality lever): split on whitespace, `. _ - [ ] ( ) { } + ,` and camelCase/digit boundaries while **also emitting compound tokens** (`S01E02`, `x265`, `5.1`, `DD+`, `WEB-DL`, `24-96`). Then lowercase → ASCII/diacritic folding → CJK segmentation (Lindera/jieba, matching Lodestar's zh support). There's no stemming on names; a light English stemmer runs on a parallel `title_stem` field for "plurals and suffixes" matching (Lodestar's tier 2).

**Query language** (hand-written parser → AST → Tantivy query; the AST JSON is shared with the UI builder for lossless round-tripping):
```
dark knight                      all terms (AND), ranked
"dark knight"                    phrase          dark NEAR/5 knight   proximity
batman | joker   batman OR joker (a | b) AND c   -cam  NOT cam        grouping & negation
comput*   ~knigth (fuzzy, edit distance 1–2)    /S0[1-9]E\d+/ (regex on full name)
name:  title:  file:  path:  group:  lang:       field scoping
ext:mkv,mp4   type:movie|tv|music|software|…    cat:video>=80%   cat:software=0   bytes:video>90%
size:>4GB  size:700MB..1.5GB   files:>=10   largest:>2GB  piece:16MB
res:2160p  hdr:dv  codec:x265  audio:atmos  year:2019..2021  s:2 e:5
seeders:>=5  trackers:>=3  dht:>100  junk:<20  private:no  v2:yes
discovered:<7d  discovered:2026-01-01..2026-03-31  seen:<24h  time:22:00..02:00 (time-of-day, user TZ)
file:(ext:flac size:>50MB)      a file matching ALL inner clauses
has:nfo  has:subs  has:cue  has:sample  has:sfv   structural presence filters
ambiguous:yes   resolved:files|folder|structure|plugin   sig:7a9f3c12 (identical payload)
!ext:exe (alias of -ext:exe)   age:<6h (alias of discovered:<6h)
hash:abc123…  (or a bare 40-hex / magnet link pasted into the box)
```
Units and relative dates are parsed. Unknown fields trigger a helpful error, not a silent ignore. Regex compiles to a trigram prefilter (the Google Code Search / zoekt technique) and then verifies against the name, with a hard time budget.

**Ranking** is two-phase. Phase 1 is per-field BM25 (summed with boosts; Tantivy has no true BM25F) under block-max WAND, giving the top ~1,000 candidates. Phase 2 re-ranks those candidates with the full feature set, which avoids scoring every match just to apply seeder and freshness priors. Paging past the window fetches the next phase-1 window and re-ranks it, so order is exact within each window and approximate only at window boundaries. The features: a phrase/proximity bonus, an exact-name bonus, and a parsed-title match bonus (title hits always outrank file-only hits via a capped file contribution, as Lodestar intended). It's then multiplied by quality priors: `log(1+seeders)`, a freshness decay, a junk penalty and a trackers-reporting nudge. A **swarm-consistency check** also feeds the junk score: one tracker reporting tens of thousands of seeders while the DHT estimate and every other tracker show ~0 is a classic fake-seeding signature, so it's penalized and labelled. All weights live in config, and `?explain=1` returns a per-result score breakdown. A **relevance judgment suite** (query → expected top-k) with an NDCG gate in CI keeps tuning honest.

**Results, facets and features:**
- **Counts:** exact up to 100k matches, then "≈ 2.4M" via segment-level estimation, and exact on demand.
- **Facets** (terms on ext/category/type/resolution/lang/group, histograms on size/discovered/year, range buckets for seeders/trackers) are a **separate request** fired in parallel. Results render first, and facets fill in, typically within ~100 ms. Above 1M matches, facets are computed on a uniform sample and labelled "≈".
- **Result cache:** an LRU keyed by (normalized AST, index generation). Filter-only "browse" queries and the global facet counts are cached and reused until the next commit that affects them.
- **Cancellation:** the server checks a cancel token between segments, and the client aborts stale requests while you type (debounced ~120 ms), so fast typing never queues work.
- **Stable pagination:** a cursor carries a **searcher lease id**, so later pages read the same index snapshot (leases expire after 10 min). NRT commits therefore can't cause duplicate or missing rows while you scroll.
- Sorts: relevance, discovered, seen, size, files, seeders, trackers, plus a secondary sort key. A sort on a hot column (e.g. seeders) for a very broad query does a full fast scan (~5–20 ns/doc), which is covered by the benchmark gates.
- Collapse-by-group option ("show best version, +N others"). The count reports both groups and torrents.
- **Index evolution:** each index directory is versioned (`index/v{schema}-{tokenizer}-{parser}`). A tokenizer or parser change triggers a **background rebuild into a new directory** while the old one keeps serving, then an atomic swap, with progress shown in the dashboard. There's never downtime for a reindex.
- Suggest/typeahead comes from the term dictionaries and the parsed-title FST. "Did you mean" uses fuzzy terms.
- **Saved searches → alerts:** on each NRT commit, each saved query runs against **only the new doc range** (cheap percolation). Notifications go to the UI (SSE), webhook, ntfy, Discord, email, or an RSS feed per saved search.
- **Indexing:** one `IndexWriter` per index (torrent + file) with a 256 MB heap each; commit every 2 s or 5k docs; a log-merge policy tuned for many small commits; reader reload on commit. Both indexes commit under the same id watermark, recorded in SQLite after both succeed. A crash between the two commits is caught up idempotently from that watermark, so `file:(…)` never sees a torrent the torrent index lacks.

## 9. API

axum, JSON. `/api/v1`: `search` (GET with q + params, or POST with AST), `suggest`, `facets`, `torrents/{hash}` (detail + first 500 files), `torrents/{hash}/files?cursor=`, `torrents/{hash}/siblings`, `files/search`, `stats`, `crawler` (status/pause/intensity), `sources` (list, enable/disable, priority/agents/schedule/egress, consent, purge, health), `jobs` (importer progress, pause/resume), `insights/overview?range=&compare=`, `insights/{stage}?range=&reason=`, `insights/events`, `insights/export.csv?panel=`, `digest` (settings + preview), `trackers`, `saved-searches`, `alerts`, `collections`, `user-items`, `settings`, `import` (.torrent upload/folder watch), `export` (CSV/JSON/magnet list). It also serves `/events` (SSE: new torrents, crawler stats, alerts, and `insights` deltas every 30 s), `/torznab/api` (for Prowlarr/Sonarr/Radarr), `/metrics` (Prometheus), `/healthz`, `/openapi.json` (utoipa), and RSS per saved search.

The tracker list ships with a curated default. It can optionally **auto-refresh** from the community lists `ngosang/trackerslist` and `XIU2/TrackersListCollection` (an opt-in external fetch, merged, then health-checked before use).

Magnet links can **append the best-scoring trackers** from scrape data (toggle), which noticeably improves magnet start-up time. Download-client integrations: qBittorrent, Transmission, Deluge, rTorrent/ruTorrent, Aria2 (one-click send and bulk send).

## 10. UI plan (Svelte 5), governed by `ui-design-skills`

### 10.0 Authority and decisions
- **Source of truth:** `dvortsis/skills/ui-design-skills` (15 skills). Its precedence applies: explicit user requirements → accessibility/correctness → coherent existing product language (Lodestar) where no skill owns the decision → skill defaults → `minimalist-visual-style`.
- **Explicit user decisions:** **Svelte 5**, and the **smallest, fastest UI** is the top UI priority. Skills written for React/Tremor are applied **by concept and spirit**. In particular, `data-visualization-design` keeps its data contracts, single formatter per metric, state handling, accessible alternatives, performance rules and validation hooks, but Tremor components are replaced by tiny in-house Svelte SVG chart primitives plus uPlot (lazy-loaded) for time series. `application-interface-design`'s Tailwind + daisyUI guidance applies literally, because daisyUI is CSS-only and framework-agnostic.
- **Who owns which decision:**

| Subject | Owning skill | Section |
|---|---|---|
| Spacing, density | `layout-spacing` | 10.2 |
| Grid, regions, alignment | `layout-grids` | 10.2, 10.4 |
| Breakpoints, adaptation | `responsive-layout` | 10.15 |
| Fonts, scale, numerals | `typography-ui` | 10.2 |
| Radii, borders, heights, hit targets | `component-geometry` | 10.2 |
| Icons | `iconography-ui` | 10.2 |
| Motion | `motion-ui` | 10.2 |
| Navigation, forms, tables, dialogs, drawers, states | `application-interface-design` | 10.3–10.11 |
| Chart choice and chart craft | `analytics-chart-selection` | 10.5, 10.9, 10.12 |
| Insights (process-story dashboard) narrative and interactions | `dashboard-storytelling` | 10.9 |
| Chart implementation (by spirit) | `data-visualization-design` | 10.1, 10.12 |
| Numeric correctness of facets, KPIs and charts | `analytics-validation` | 10.17 |
| Onboarding explainer | `infographic-design` (spirit; no AntV dependency) | 10.10 |
| Aesthetic | `minimalist-visual-style` | below |
| Final QA | `design-review` | 10.17 |
| Colour and themes | user-owned (Riverstone + collection); contrast rules from `design-review`/`application-interface-design` | 10.18 |
| Brand assets and illustration use | user-supplied pack; usage limited by `minimalist-visual-style`, `application-interface-design`, `responsive-layout`, `motion-ui` | 10.19 |

- **Aesthetic: `minimalist-visual-style` (confirmed).** The illustrated mascot scenes are **brand expression**, which the skill allows. They're limited to full-page, empty, onboarding and error states (§10.19) and never used as decoration on working surfaces. This is effectively a new product, and Lodestar's language included things the skills prohibit: an ambient particle background (`BgEffect`, `UI_BACKGROUND_ANIMATION`) and decorative effects. The result is flat, quiet surfaces, editorial hierarchy carried by type and whitespace, no gradients, no glow or glass, and no emoji in the interface (Lodestar's emoji-style file-type SVGs are replaced by Phosphor icons). **Kept from Lodestar's language:** the composition bar and its fixed category→colour mapping, and the dense expandable result rows.
- **Colour is user-owned** (every skill). **Decided: "Riverstone" is the default and official palette**, with a **large selectable theme collection in light and dark** (§10.18). Every theme uses the same schema as Riverstone (`background`, `surface`, `text`, `primary`, `accent`, 6 `categorical`). A build-time theme compiler derives every other token and enforces the contrast gates.

### 10.1 Stack and performance budget (smallest/fastest first)
- **Stack:**
  - SvelteKit 2 + Svelte 5 runes, `adapter-static` SPA (`ssr = false`), embedded in the binary and precompressed with brotli.
  - Tailwind v4 + daisyUI 5 (CSS-only, purged to the classes actually used).
  - **Bits UI** (headless; imported per component) for Dialog, Popover, Tooltip, Select, Combobox, Slider, Tabs, Toggle Group and Menu.
  - `@tanstack/svelte-virtual` for the results list, file tree and tables.
  - Phosphor icons compiled at build time to inline SVG (`unplugin-icons`), so there's no icon runtime.
  - In-house SVG chart primitives (target ≤ 6 KB), plus **uPlot** (~20 KB gzipped) lazy-loaded only on the Insights pages and the swarm-history panel.
  - No date library (`Intl.DateTimeFormat`/`RelativeTimeFormat`), and compile-time i18n (paraglide-js, tree-shaken) for en + zh.
- **Budgets (gzipped, CI-gated with `size-limit` and Lighthouse CI):**

| Item | Budget |
|---|---|
| Search route JS (initial) | ≤ 45 KB |
| CSS | ≤ 20 KB |
| Fonts on first paint (Atkinson Hyperlegible Next variable, Latin subset) | ≤ 35 KB |
| **Total first load** | **≤ 100 KB** |
| Detail-drawer chunk | ≤ 15 KB |
| Insights chunk (incl. uPlot) | ≤ 45 KB |
| LCP | < 1.0 s on a LAN; < 2.5 s on a mid-tier phone over 4G |
| INP / CLS | < 100 ms / < 0.02 |
| Scrolling a 10k-row virtual list | 60 fps |

- **Techniques:**
  - Search-as-you-type is debounced 120 ms; `AbortController` cancels stale requests, and the server also checks a cancel token.
  - Rows have fixed heights per density, so virtualization never needs to measure them.
  - Torrent details are prefetched on hover/focus or j/k after 150 ms idle.
  - File trees with more than 2k files are built in a **Web Worker**. Lodestar built them on the main thread.
  - SSE inserts are batched per animation frame.
  - Offscreen drawer sections use `content-visibility: auto`.
  - Animation uses transform/opacity only (§10.2).
  - Asset filenames are content-hashed and served with immutable caching.
  - The query AST and its validation come from the server (`/api/v1/parse`) so the Rust parser stays the single source of truth. The client carries only a tiny tokenizer for syntax highlighting, not a WASM copy of the parser.

### 10.2 Foundations (design tokens)
- **Spacing (`layout-spacing`):**
  - Tailwind spacing tokens = the skill ramp `0, 2, 4, 6, 8, 10, 12, 16, 20, 24, 28, 32, 36, 40, 48, 52, 56`.
  - Inside a component: 4–12. Repeated patterns (row internals, facet groups): 8–16. Separation between layout regions: 24–48.
  - The query bar gets the roomiest surround because it's the focal point. Result rows and facets stay locally dense. Groups are separated by whitespace before borders are considered.
  - **Density modes** change component spacing only:
    - *Compact:* 36 px single-line rows.
    - *Comfortable* (default): 64 px two-line rows.
    - *Cards:* fixed height per breakpoint, for visual browsing.
- **Grid (`layout-grids`):**
  - The app shell is an **asymmetric column layout**: a fixed 264 px facet rail, a fluid results column (min 560 px), and a 480–640 px detail drawer (resizable, width persisted). Results get the most space because they matter most.
  - Settings, help and syntax pages use a **manuscript** column (max 720 px; prose ≤ 70 ch).
  - Insights uses a **modular 12-column** grid with unequal spans (§10.9.6).
  - Result-row columns share edges with the table-mode header, and numeric columns are right-aligned.
- **Typography (decided by you, overriding `typography-ui`'s font list as its precedence allows):**
  - **UI: Atkinson Hyperlegible Next** (Braille Institute, SIL OFL), variable weight. It's self-hosted as a woff2 Latin subset (≤ 35 KB, preloaded); a Latin-Extended subset loads on demand. Fallback: `system-ui, sans-serif`. CJK falls back to system fonts (`"PingFang SC", "Microsoft YaHei", "Noto Sans CJK"`).
  - **Mono: Atkinson Hyperlegible Mono** (OFL). Lazy-loaded, ≤ 25 KB subset, used for hashes, file paths, query field tokens, the info-dict viewer and keyboard shortcuts. Fallback: `ui-monospace, monospace`.
  - Its hyperlegible letterforms (distinct I/l/1, O/0) suit hash and path reading. Use `font-variant-numeric: tabular-nums` (verified against the font's features in M5).
  - No serif in the app chrome. The logo lockups keep their own wordmark artwork.
  - Scale (6 steps, size/line-height in px):

    | Step | Size/line | Use |
    |---|---|---|
    | caption | 12/16 | badges, meta |
    | small | 13/18 | row metadata, facet labels |
    | body | 14/20 | default UI text |
    | body-lg | 16/24 | query input, drawer subtitles |
    | title | 20/26 | drawer and section headings |
    | display | 28/32, −0.02em | page headings, KPI values |

  - Weights 400/500/600 only. **Tabular numerals** for sizes, counts, seeders, trackers and times.
- **Geometry (`component-geometry`):**
  - Radii **16** for dialogs, sheets and the drawer edge; **12** for cards, banners, toasts and panels; **8** for buttons, inputs, the query bar and segmented controls; **4** for tags, badges, chips and composition-bar end caps. No pill shapes on containers or primary buttons.
  - Borders are 1 px using the `base-300` token, and only where whitespace isn't enough.
  - Control heights are 32 px (dense desktop toolbars and facets) and 40 px (default). Under `pointer: coarse`, every target grows to **≥ 44 × 44**.
  - Buttons keep their width while loading.
  - Defined once in the daisyUI theme (`--radius-box: 12px`, `--radius-field: 8px`, `--radius-selector: 4px`) plus a single override for dialog/drawer = 16, never repeated in page code.
- **Icons (`iconography-ui`):**
  - **Phosphor Bold**, with Fill only for selected/on states (favorite, active filter). Roughly 40 icons.
  - 16 px in rows, 20 px in the toolbar and navigation.
  - Icon-only buttons get an `aria-label` and a tooltip; decorative icons are `aria-hidden`.
  - Each file category has an icon, always paired with a text label where its meaning matters.
- **Motion (`motion-ui`):**
  - Hover/focus/press: 150 ms. Tooltips and popovers: 150 ms in (ease-out), 100 ms out (ease-in).
  - Drawer/sheet: **300 ms ease-out in, 200 ms ease-in out**, transform only.
  - New-results banner: 200 ms. Press feedback: `scale(0.98)`.
  - **Results and facet updates swap instantly with no animation.** Filter changes never re-animate the page.
  - The only staggered reveal is on Discover's first load (80 ms steps, at most 8 items, nothing critical withheld).
  - `prefers-reduced-motion` → immediate changes (opacity crossfades ≤ 100 ms at most).
  - No ambient or decorative motion.
- **Colour use:** status is always shown as **icon + text**, never colour alone. The category-group→slot mapping (§10.18) is identical in the composition bar, facets, legends and charts. AA contrast (4.5:1 text, 3:1 UI boundaries and data marks) is enforced per theme by the theme compiler and re-verified by axe in CI.

### 10.3 Information architecture and navigation
- **Five stable top-level destinations:** **Search** (home), **Discover**, **Alerts**, **Insights**, **Settings**.
  - The top bar holds the wordmark, navigation, a global status pill ("Crawling · 1.2k/h", icon + text, linking to Insights) and the command-palette trigger.
  - The current location is marked with an underline and `aria-current`.
  - An overflow menu holds Import, Backup, Syntax guide and About.
- **The URL is the state:**
  - `/search?q=…&sort=…&density=…&group=…`
  - `/t/{hash}` opens the detail as a drawer when coming from Search, or as a full page from a direct link.
  - `/compare?h=a,b,c`, `/discover`, `/alerts/{id}`, `/insights?range=7d&compare=1`, `/insights/{stage}`, `/settings/{section}`
- Narrow widths: navigation collapses into a menu button. Search stays first-class, and the status pill becomes icon-only, keeping its accessible text.

### 10.4 Search screen
The primary purpose is finding the right torrent; the dominant action is grab (copy magnet or send to client).

- **Layout (xx-large):** the query bar and active-filter strip run across the top. Below them, three regions: the facet rail, the results toolbar + list, and the detail drawer.
- **Query bar:**
  - 48 px tall, 16 px text, radius 8.
  - Syntax tokens are distinguished by **weight + mono font + colour** (field names, operators, quoted phrases, ranges).
  - Parse errors get an underline **and** a text message underneath.
  - **Autocomplete** (combobox/listbox pattern): fields, values with counts (from the term dictionaries), units, saved searches and recent queries. Tab accepts.
  - **Smart chips:** recognized tokens such as `2160p` are offered as "res:2160p" chips. A "Natural phrasing" action converts text like "4k movies from 2020 over 10gb" into chips, previewed before they're applied.
  - The **Builder** toggle opens an inline row builder (field / operator / value) that round-trips with the text through the server AST.
  - A "Syntax" link opens the cheat-sheet popover.
- **Active-filter strip:** one removable chip per structured clause, plus "Clear all". Typed clauses and facet clicks share **one state, the AST**, so the strip always explains why the results look the way they do.
- **Facet rail (global scope, applies to results):**
  - Counts are **disjunctive**: each facet's counts ignore its own selection, so you can multi-select. They're labelled "≈" when sampled.
  - **Default groups, in decision order:**
    - Type
    - Discovered: a binned histogram with brush, plus presets 24 h / 7 d / 30 d / 1 y / Custom. A "Backfill" marker explains early-crawl dates.
    - Size: a log-binned histogram with brush; bins are explicit and labelled.
    - Extension: top 8 plus "Show more" with search.
    - Resolution
    - Seeders: presets ≥ 1 / ≥ 5 / ≥ 50, plus an "Include never-scraped" toggle.
    - Trackers reporting
    - Junk: "N hidden · Show".
  - **"More filters":** Language, Release group, Codecs, HDR, Year, Season/Episode, File count, Largest file, Composition thresholds (with a count-or-bytes **parameter**), Private, v2, Has NFO / subtitles / cue, Ambiguous, Imported/backfill, In library, **Network** (default `bt` only), **Source**, **Verified** (unverified hidden by default), **Official** (§18).
  - A selected value is shown four ways: checked box, medium weight, value text and a chip in the strip.
- **Results toolbar (parameters, deliberately separate from filters):** result count ("1,284 results" or "≈ 2.4M results"), sort, "Group versions" toggle, density control, select mode, Save search, Export. All of it persists in the URL.
- **Result row (Comfortable):**
  1. **Title line:** the effective title with highlighted terms. The raw name appears as a mono subline when it differs, with "resolved via files" (or folder/structure/plugin).
  2. **Facts line:**
     - up to 5 parsed badges (4 px tags);
     - the **composition bar** (64–120 px) with dominant-share text such as "82% video";
     - size (GiB, tabular);
     - files;
     - seeders/leechers ("—" if never scraped; the scrape age shows on focus/hover and in the drawer);
     - trackers "4 / 20";
     - discovered (relative, with a backfill marker);
     - an "In library" badge.
  3. A **"Matched in files: …/S01E03.mkv"** line when the match came from file names.
  4. **Row actions:** Copy magnet (icon + label in Comfortable), Send, More. They show on hover/focus and **always on touch**. No information is hover-only: every value is text in the row or drawer.
- **Table mode:**
  - A real `<table>` with a sticky header, sortable columns, a column chooser and saved views.
  - Numbers are right-aligned with explicit units.
  - Horizontal scrolling is contained within the table at narrow widths.
- **Selection and bulk actions:** `x` or a checkbox selects rows. A bulk bar offers Copy magnets, Send, Export, and Compare (2–5 selected).
- **States** (each designed separately, per `application-interface-design`):

| State | Treatment |
|---|---|
| Empty (no query) | Headline stats ("12.4M torrents · +1,240 in the last hour"), recent and saved searches, and three syntax examples. Next step: focus the search box. |
| First load | 8 skeleton rows at their final heights |
| Refining | Previous results stay. A top progress hairline and `aria-busy`, but no skeleton flash for anything under 300 ms. |
| No results | Zero-result recovery from per-clause counts ("Removing `trackers:>=3` gives 1,240"), plus "Clear all filters" |
| Error | Inline banner (what happened + Retry). The last results stay visible. |
| Partial | "≈" counts and sampled facets are labelled as such |
| Index rebuilding | Banner with progress; the old index keeps serving |
| Cold start | "The index is still growing — 84,210 torrents, +2.1k/h. Older content appears over time." with a link to the bitmagnet import |
| Live | "12 new results · Show" banner. The list never reorders under you, and a throttled polite `aria-live` announces it. |

### 10.5 Torrent detail (drawer; full page at `/t/{hash}`)
- **Drawer behaviour:**
  - A drawer, not a modal, because it keeps the list visible.
  - Focus moves to the title. Esc closes it and returns focus to the row. j/k keep moving through results and update the drawer.
  - Resizable, with the width persisted.
- **Header:**
  - Effective title (20 px) and raw name (mono 13).
  - **One primary action:** "Send to qBittorrent" if a client is configured, otherwise "Copy magnet".
  - Secondary actions sit in a menu: Favorite, Hide, Labels, Open full page, Fix parse.
- **Sections in story order** (below-the-fold sections are lazy):
  1. **Summary facts:** size, files, discovered, last seen, seeders/leechers with scrape age, trackers reporting (n of m checked), piece size, private/v2, and junk score with its reasons.
  2. **Contents:**
     - The **composition** is shown as a **sorted horizontal bar list** with category, files, bytes and %. This replaces the donut from draft 1: `analytics-chart-selection` says to avoid donuts beyond a few parts and to prefer sorted bars for exact comparison.
     - The **file tree**: virtualized ARIA tree with folder sizes, a filter box, matched files highlighted, a toggle to hide padding, and per-file checkboxes for **"Send selected files"**.
  3. **Parsed metadata:** title, year, season/episode, resolution, codecs, languages, group, edition. Each shows its provenance ("from: name / files / NFO / plugin"), with a "Fix parse" action.
  4. **Swarm:**
     - A seeders/leechers line over time. Missing samples are **gaps, not zeros**. Axis units are shown, and tooltips use the full date.
     - A per-tracker **table**, because this is exact lookup.
     - The DHT estimate, labelled as an estimate.
  5. **Related:** other versions (same group), identical-payload clones, other torrents containing the same file.
  6. **Raw:** a pretty-printed info-dict viewer (lazy, mono).
  7. **Notes and labels.**

### 10.6 Compare view
- A full page, not a modal, with 2–5 aligned columns:
  - name;
  - parsed attributes (differences marked with an icon **and** the word "differs");
  - size, files, seeders, trackers, discovered;
  - composition bars on a **shared scale**;
  - a file-list diff (only in A / only in B / common).
- Each column has its own actions.
- Narrow widths re-architect it into an attribute-first list (attribute → value per torrent) instead of squeezed columns.

### 10.7 Discover
- It answers "what's new or rising?" with three sections:
  - **Newest**, with category tabs;
  - **Rising**: the top N by swarm trend, with the trend explained in text;
  - **Recently resolved**: ambiguous torrents that have just been identified.
- It reuses the result row and the facet rail (collapsed by default).

### 10.8 Alerts (saved searches)
- **List view:** name, query (mono), new hits since last viewed, channels, last hit time. Opening one shows its results with a "New since …" divider.
- **Creating one:** "Save search" in the results toolbar opens a dialog with visible labels for name and notification channels.
- Pausing is a toggle with no confirmation. **Deleting asks for a specific confirmation** ("Delete '4K nature docs' and its history?").

### 10.9 Insights: the process story (`dashboard-storytelling`)
**Insights** replaces the earlier "Crawler" dashboard as a top-level destination. Its overview page tells the whole story of the process, from DHT discovery to a search result in your hands, and one drill-through page per stage holds the detail.

- **Audience and decisions:** the self-hoster. Is it working? Is it growing? **Where are torrents being lost?** Is the index good and current? Is anything about to break, and what do I do about it?
- **Type:** an exploratory, progressive dashboard (`dashboard-storytelling`). The first viewport orients you, and controlled vertical scrolling follows the story order.
- **Global context bar** (always visible, sticky):
  - time range (24 h / 7 d / 30 d / All / Custom), with the timezone shown;
  - a **"Compare to previous period"** toggle, which is a parameter, not a filter;
  - a "Last updated" time;
  - crawler **Pause/Resume** and the **intensity** control (Low / Balanced / Aggressive, with its effect explained). These affect the whole process, so they sit in the global bar.

#### 10.9.1 Orientation: the stage strip
A horizontal strip across the top shows the five stages, each with one live headline number:

**Discover** (unique hashes harvested) → **Fetch** (metadata retrieved) → **Understand** (stored and classified) → **Index** (searchable) → **Serve** (searches answered)

- This is a process explainer in the `infographic-design` sense: it explains a workflow next to its metrics, and it isn't used for quantitative comparison.
- It doubles as section navigation: activating a stage scrolls to its section (anchor links, keyboard accessible).
- **Text equivalent:** an ordered list with the same numbers.
- **Narrow screens:** it becomes a 2-row grid; the stage order is preserved.

#### 10.9.2 Overview story (visual inventory)
| Level | Question | Visual (`analytics-chart-selection`) | Role |
|---|---|---|---|
| 1 Status | Where are we now? | KPI row, max 5: **Searchable torrents** (Δ vs previous period); **New searchable in range**; **End-to-end yield** (harvested → searchable, completed cohorts only); **Time to searchable from first discovery** (p50 / p95; includes finding peers and fetching, so it's hours, unlike the §3 metadata → searchable target, which the Index page shows as commit lag); **Health** (Running / Paused / Degraded + reason, as icon + text) | Primary |
| 2 Movement | How is it changing? | Line: newly searchable torrents per hour/day (by event time), with **event annotations**: restart, upgrade, intensity or config change, import, reindex, network change. A **separate, aligned** line shows unique hashes harvested; no dual axis. Shaded comparison band when "Compare" is on. | Primary |
| 3 Drivers | Where are torrents lost? | **Stage funnel by discovery cohort:** Harvested → Peer found → Metadata fetched → Stored → Searchable, showing counts and stage-to-stage conversion %. Next to each stage, a 100% stacked bar of **loss reasons**: no peers found, timed out, refused, encryption failed, oversize, invalid, blocked, evicted (queue full), gave up (retry horizon passed). | Primary |
| 4 Segments | What is arriving, from where? | Sorted horizontal bars: searchable by category group and by content type; yield by harvest strategy (BEP 51 / passive / active lookup); yield by IP family (v4 / v6); ambiguous names resolved by method (files / folder / structure / plugin) | Supporting |
| 5 Exceptions | What needs attention? | Issue list, each with icon + text + a fix action: yield below the trailing 7-day baseline, NAT saturation, ephemeral-port pressure, unreachable port, scrape bans, index lag > 30 s, parser confidence falling, disk projection < 14 days, cohort completion stalled, FUSE/network path | Primary when non-empty |
| 6 Serve | Is it useful? | KPIs + small multiples: searches per hour, zero-result rate, search latency p95 (server-measured), alerts fired, sends to download clients | Supporting |
| 7 Detail | What exactly? | Links to each stage's detail page (§10.9.5) and to the data export | Detail |

#### 10.9.3 Analytical integrity rules
- **The funnel uses discovery cohorts.** A hash belongs to the cohort of the UTC hour it was first harvested. Later stages count progress *of that cohort*, so the stages describe the same population and the conversion rates are honest.
  - Counting "seen today" against "fetched today" would mix different torrents, because fetching lags discovery by hours or days.
  - A cohort is **final** once every hash in it has reached a terminal state, or once the retry horizon (default 72 h) has passed. Cohorts that aren't final yet are **hatched and labelled "incomplete"**, and they're excluded from the yield KPI.
- **Two time bases, always labelled.** The funnel and yield use *discovery cohort*; the trend, "new searchable" and Serve use *event time*. Each chart's subtitle says which one it uses.
- **"Unique" is approximate before storage.** Harvest uniqueness comes from the time-rotated "seen" filter (§5), so a dead hash can be counted again after filter rotation. The Discover number is labelled "unique (≈)". From "Stored" onward, counts are exact.
- **Final cohorts never change.** Removals after storage (retention policies, a later junk re-score) are lifecycle events, not funnel losses: they're shown on the Index page and annotated on the trend.
- **Imported torrents** (bitmagnet import) never went through discovery, so they're excluded from the funnel. They appear as "Imported" in the Status totals and as an annotation on the trend.
- **Category filters only apply from "Understand" onward,** because hashes have no category until their metadata arrives. When the global content filter is active, the funnel greys out the earlier stages and says why.
- **Latency percentiles** come from log-bucketed histograms and are labelled approximate (bucket resolution ≤ 10%).
- **Missing data is a gap, not a zero** (e.g. while the crawler is paused); paused periods are annotated.

#### 10.9.4 Interaction map
| Source | Trigger | Action | Targets / scope | Clear |
|---|---|---|---|---|
| Context bar time range | select | global filter | every visual on Insights and its detail pages | reset to default (7 d) |
| Compare toggle | toggle | parameter | adds comparison bands and Δs to Status, trend, Serve | toggle off |
| Trend chart | brush | sets the global time range | page | Reset chip |
| Stage strip | click/Enter | navigate (scroll to section) | — | — |
| Funnel stage / loss reason | click | drill-through to that stage's detail page, pre-filtered to the reason | new page; breadcrumb back | Back |
| Category / content-type bar | click | drill-through to Search (`discovered:<range> type:<x>`) | Search | Back |
| Harvest-strategy bar | click | drill-through to the Discover detail page | page | Back |
| Exception | click | navigate to the fix (settings section or detail page) | — | — |
| Zero-result rate | click | drill-through to the Serve detail page (top zero-result queries, if logging is on) | page | Back |

- There's no chart-to-chart cross-filtering anywhere, which keeps behaviour predictable.
- Every active selection is shown as a chip in the context bar and can be reset.
- URL state: `/insights?range=7d&compare=1`, `/insights/{stage}?range=…&reason=…`.

#### 10.9.5 Stage detail pages (`/insights/{discover|fetch|understand|index|serve}`)
Each detail page follows the same pattern: three to four KPIs, one primary trend, then drivers and the detail tables. Each has a breadcrumb back to the overview with the context preserved.

- **Discover:**
  - DHT nodes and routing-table health; reply ratio (the NAT-saturation signal);
  - harvest by strategy and BEP 51 sample yield; IPv4/IPv6;
  - virtual-ID keyspace coverage;
  - the seen-filter load factor.
- **Fetch:**
  - queue depths and age; outcome breakdown over time;
  - TCP vs uTP and MSE vs plaintext success;
  - fetch latency histogram; per-IP limiter rejections;
  - ephemeral-port pressure (Windows).
- **Understand:**
  - category and content-type mix; parser confidence distribution (histogram);
  - ambiguous-name resolution rate; junk score distribution;
  - blocklist drops (counts only; blocked content is never shown);
  - recent "Fix parse" corrections.
- **Index:**
  - DB/index/file-index sizes against the §3 budget, with a growth projection;
  - NRT commit lag; segment count and merges;
  - file-index coverage (smart mode);
  - swarm data: scrape coverage and freshness, trackers reporting (per-tracker table: health, rate limit, ban state);
  - rebuild/reindex status.
- **Serve:**
  - searches per hour, latency percentiles, zero-result rate and cancel rate;
  - facet sampling rate ("≈" share);
  - alerts fired and notification delivery outcomes; sends to download clients;
  - top queries and top zero-result queries, **only when query logging is enabled** (off by default, stored locally, retained 30 days, clearable).

#### 10.9.6 Chart craft, states and layout
- **Chart craft:**
  - Bars start at zero; the funnel's widths are proportional to counts, and its labels show both the count and the conversion %.
  - Axes carry units, and tooltips are headed by the full date.
  - **One formatter per metric** (§10.12), shared by KPIs, axes, tooltips and tables.
  - Every chart has **"View as table"**, a one-sentence text summary, and **CSV export**.
- **States:**
  - **Starting up:** fewer than one completed cohort. Shows the `searching_crawling` illustration with "Your first discoveries will appear here within minutes" and live counters.
  - **Paused:** the `offline_idle` illustration plus a Resume action.
  - **Partial cohorts:** hatched and labelled "incomplete".
  - **Stale** (> 2 min since the last rollup flush): a banner with the reason.
  - **Error:** a panel-level error that keeps the rest of the page usable.
- **Layout** (`layout-grids`, modular 12-column):
  - KPIs span 12; the trend 8 + exceptions 4; the funnel 12; segments 6 + 6; Serve 12.
  - **Narrow screens:** stage strip, KPIs (2×2 + 1), then **exceptions**, funnel (vertical), trend, segments, Serve. Narrative order is preserved, and exceptions move up because they're actionable.
- **Live updates:** SSE `insights` deltas every 30 s (suspended while the tab is hidden). Updates never re-animate the page (`motion-ui`); numbers change in place.
- **Print layout:** the overview prints as a one-page report (context bar, KPIs, trend, funnel, exceptions), with the URL and generation time in the footer.

#### 10.9.7 Digest
- An optional **weekly or daily digest** (email / ntfy / Discord / RSS), generated from the rollups at a quiet hour (default 03:00 local).
- It contains the context line, the five KPIs with Δs, the funnel conversion summary, the top 3 segment movers, and any open exceptions, with links back to Insights.
- Plain-text plus simple HTML, with no images. It follows the copy rules (§10.14).

### 10.10 Settings and onboarding
- **Settings:**
  - A manuscript layout (max 720 px). Section navigation sits on the left at x-large and becomes a select on narrow screens.
  - Forms have visible labels, help text next to each field, inline validation, and per-section Save with a "Saved" confirmation.
  - Sections: General, Crawler, Network & VPN, Storage & retention (with size-projection previews), Search & ranking, **Sources** (§18.8: every discovery, importer, enrichment, network and output plugin, with priority, agents, schedule, egress, consent and purge; Auto-balance), **Companions** (§18.10: installed and detected companions, health, versions, logs, updates, Set up / Remove), **Workers** (§19.7: add/join, status, placement, budgets, drain, revoke, purge-by-worker), Trackers, Integrations, Notifications, **Insights & digest** (digest schedule and channels, query logging on/off and retention, clear usage data), Security, Appearance (theme, density, motion, shortcut settings), About.
- **First-run wizard** (stepped progress, one primary action per step):
  1. Setup token
  2. Admin password
  3. Network check (live port-reachability test)
  4. Intensity
  5. Storage & retention
  6. Import from bitmagnet (optional)
  7. Download client (optional; recommended: qBittorrent, installed or detected by the companion manager, §18.10)
  8. Done
- One **"How it works"** step uses a static SVG sequence (DHT → metadata → index → search) in the spirit of `infographic-design`, with no AntV dependency and a text equivalent.

### 10.11 Component inventory (one canonical primitive per role)
- **Controls and fields:** Button (primary / secondary / quiet / danger × dense / default, with a loading state), IconButton, Field (Input, Select, Combobox, Textarea, Checkbox, Radio, Toggle, Slider, RangeInput with units), SegmentedControl.
- **Labels:** Tag/Badge, Chip (removable), Kbd, Mono (identifier text), RelativeTime (with an absolute-time tooltip and text).
- **Overlays:** Tooltip, Popover, Menu, Dialog, Drawer/Sheet.
- **Feedback:** Toast, Banner (info / success / warning / error, icon + text), Skeleton, EmptyState.
- **Lists and tables:** Table (sortable, sticky, column chooser), VirtualList, Tree (virtual, ARIA).
- **Data display:** KPI, CategoryBar (composition), HBarList, Histogram (+ brush), Sparkline, LineChart (uPlot wrapper), DataTableAlternative.

Rules (`application-interface-design`):
- Shared primitives live in `web/src/lib/ui`; feature compositions live in `web/src/lib/features/*`; dependencies only point one way.
- Variant names are semantic, and geometry comes from tokens.
- Each shared primitive gets keyboard and state tests.
- Promote a new primitive only after the same pattern appears in two places.

### 10.12 Data display contracts (`data-visualization-design` spirit; `analytics-chart-selection` craft)
- **Metric contracts** live in `docs/metrics.md`:
  - seeders = max across sources within the freshness window;
  - trackers reporting = trackers with s + l + c > 0 out of m checked;
  - size = bytes, displayed in **IEC units (KiB/MiB/GiB)**, labelled consistently;
  - ingest rate = new stored torrents per hour, not harvested hashes.
- **One formatter per metric** (`Intl.NumberFormat`), used in facets, rows, the drawer, tooltips, tables and KPIs.
- **Null ≠ zero:** never scraped → "—"; a real 0 → "0"; estimates → "≈"; sampled → "≈" + a note.
- **Dates:** relative in rows, absolute in the drawer and in tooltips. The timezone is a setting, and the dashboard shows which one is in use.
- **Histograms:** explicit, consistent bins (size uses log bins with labelled edges), zero baselines, and empty bins shown as empty.
- **Validation hooks:** `data-testid` on counts, KPIs and chart containers; deterministic ordering; filter state held in the AST; stale responses can't overwrite newer ones.

### 10.13 Accessibility (WCAG 2.2 AA)
- **Structure:** landmarks, a heading hierarchy, and a "Skip to results" link.
- **Widget patterns:**
  - Results are a listbox/grid with roving tabindex.
  - The query box follows the combobox pattern, and the file tree follows the tree pattern.
  - Dialogs trap focus and restore it on close; nothing else traps focus.
  - Focus is always visible.
- **Announcements:** result counts and new results go to a polite, throttled `aria-live` region.
- **Shortcuts:** single-key shortcuts (`j/k/o/c/s/x`) only fire when focus is outside text inputs, and can be **turned off or remapped in Settings** (WCAG 2.1.4).
- Supports 200% zoom and text-spacing overrides, 44 px targets on touch, and reduced motion.
- **Not colour alone:** the composition bar has an accessible text summary and visible dominant-share text, and status uses icon + text.
- axe-core runs in CI on every screen and in every theme.

### 10.14 UX writing
- **Labels:** outcome-oriented ("Copy magnet", "Send to qBittorrent", "Save search", "Show 12 new results").
- **Truthful states:** "Never scraped", "Seen 3 h ago", "≈ 2.4M", "Reported by 4 of 20 trackers checked", "First seen during backfill".
- **Errors:** what happened, then what to do.
- **Empty states** offer one next step.
- No AI-copy clichés and no placeholder names.
- **Glossary** in `docs/ui-glossary.md`, used consistently: torrent, file, resolved title, raw name, discovered, last seen, seeders, trackers reporting, junk, version group, clone.

### 10.15 Responsive behaviour (`responsive-layout`; Fluent reference ranges, switching where the composition actually needs to change)

| Range | Composition | Techniques |
|---|---|---|
| xxx-large ≥ 1920 / xx-large 1366–1919 | Rail + results + persistent drawer (3 regions); results get extra width, and prose stays capped | Resize |
| x-large 1024–1365 | Rail + results; the drawer **overlays** the results (560 px) and keeps the list visible | Reposition |
| large 640–1023 | Rail collapses into a "Filters (3)" button that opens a left sheet; drawer overlay is 480 px or full width | Show/hide, re-architect |
| medium 480–639 / small 320–479 | One column with a sticky query bar. **Filters open in a bottom sheet with staged "Show 1,284 results"** (the live count is on the button). Detail opens as a full-screen view with Back. Table density falls back to Compact. Bulk actions move to a bottom bar. | Re-architect, reflow |

- Container queries let the result row and drawer adapt to their own width.
- **Test widths:** 320, 390, 768, 1024, 1366, 1920, 2560. Also check long names, CJK text, 200% zoom, an empty index and 50k-file trees.

### 10.16 Keyboard map
| Key | Action |
|---|---|
| `/` | Focus search |
| `j` / `k` | Move selection |
| `Enter` | Open detail |
| `o` | Open magnet |
| `c` | Copy magnet |
| `s` | Send to client |
| `x` | Select row |
| `Esc` | Close / clear |
| `⌘K` | Command palette |
| `?` | Shortcut sheet |

Single-key shortcuts can be turned off or remapped (§10.13).

### 10.17 Review and validation process
- **`design-review`** runs at the end of each UI milestone, with findings reported as blocking / important / polish. Blocking findings gate the milestone.
- **`analytics-validation` matrix** against a fixed fixture dataset:
  - result count = sum of disjunctive-facet expectations, computed independently from SQL and Tantivy;
  - histogram bins reconcile to source counts;
  - **Insights reconciliation** (metric contracts in `docs/metrics.md`):
    - per-hour/day "stored" and "searchable" rollups equal independent `COUNT(*)` over `torrents` (by `discovered_at` for stored; by the first `index_watermark_hour` row covering each id for searchable);
    - "Searchable torrents" equals the Tantivy document count at the last commit;
    - funnel stages never increase from one stage to the next within a cohort;
    - per-cohort harvested = Σ(terminal reasons) + in-progress;
    - event-time totals equal Σ hourly rows after daily compaction (compaction loses nothing);
    - Discover/Fetch counters equal the Prometheus `/metrics` counters within one flush interval;
    - `rollup --repair` restores exact stored/searchable counts after a simulated crash;
    - each drill-through from Insights lands on a page (Search or a stage page) whose counts match the clicked mark;
  - filter, brush and reset sequences restore the baseline.
- **Playwright tests:**
  - visual snapshots for each breakpoint × theme × density;
  - keyboard-only end-to-end flows (search → open → send; save alert; first-run wizard);
  - a reduced-motion run.
- **Gates:** axe-core, `size-limit` budgets, and Lighthouse CI (LCP/INP/CLS).
- **Brand asset gates:**
  - `SHA256SUMS.txt` verifies the source pack;
  - the pipeline fails if any processed scene is over 45 KB (AVIF 540w) or any image lacks `width`/`height`;
  - a size-limit check confirms no brand raster is in the first-load bundle;
  - a visual snapshot of every illustrated state in light/dark at each breakpoint;
  - a check that illustrations are hidden when "Show illustrations" is off or `Save-Data` is set.
- **Theme gates:** the theme compiler's contrast and ΔE checks for all 48 themes; Playwright snapshots of the key screens in Riverstone light/dark plus a rotating sample of 6 other themes per CI run (all 48 nightly); axe run per theme nightly; a test that the pre-paint script causes no flash of the wrong theme.

### 10.18 Colour and theming

**Default/official palette: Riverstone** (as supplied)

| Token | Light | Dark |
|---|---|---|
| background | `#F3F6F5` | `#141F25` |
| surface | `#FFFFFF` | `#22313A` |
| text | `#243C44` | `#E5EEED` |
| primary | `#527F91` | `#85AFC0` |
| accent | `#829F8A` | `#A0C3A7` |

Categorical (official, as amended): `#527F91` `#829F8A` `#C4A765` `#BF8173` `#8C9BA7` **`#9381B6`**. Seafoam `#92A9A0` was replaced by a muted plum. You suggested `#9C8AA5`; it fixes normal vision but nearly matches slate under deuteranopia (ΔE_ok 0.9). `#9381B6` (OKLCH 0.64 / 0.08 / 300°) keeps every pair at ΔE_ok ≥ 5.5 for normal, deuteranopic and protanopic vision in both modes, and is ≥ 3:1 on both surfaces (no light-mode data variant needed). The brand files `tokens/colors.json`/`theme.css` get the same update.

**Contrast audit of the supplied values (WCAG 2.x):**
- **Dark mode passes everywhere it's used.** Text 14.2:1 on background; primary 7.1:1; accent 8.7:1; every categorical colour ≥ 3.06:1 on surface.
- **Light-mode text passes:** 10.7:1 on background, 11.6:1 on surface.
- **Light-mode `primary` (`#527F91`) is just short for normal-size text:** 4.02:1 on background and 4.37:1 on surface. White text on a primary button is 4.37:1. All of these need 4.5:1.
- **Light-mode `accent` (`#829F8A`) is 2.65–2.89:1.** That fails text (4.5:1) and UI boundaries (3:1). It's fine as a fill behind `text` (4.03:1, which passes for large or bold labels) or as decoration.
- **4 of the 6 categorical colours are under 3:1 on the light surface:** `#829F8A` 2.89, `#C4A765` 2.32, `#8C9BA7` 2.85, `#92A9A0` 2.50.
- **Originally, two categorical colours were near-duplicates:** sage `#829F8A` vs seafoam `#92A9A0` (ΔE_ok ≈ 1.9; ≈ 1.5 under colour-vision deficiency). This is now **resolved** by the plum replacement above. The remaining weakest pair is sage vs slate (ΔE_ok ≈ 5.1 normal, ≈ 4.1 deutan), which is acceptable.

**Resolution: keep the official values as the brand values; derive role tokens for anything that must pass.** Text, buttons and data marks use **ink/data variants** that keep each hue and only shift its lightness in OKLCH.

| Derived token | Riverstone light | Riverstone dark | Rule |
|---|---|---|---|
| `primary-ink` (links, primary buttons, focus ring) | `#4A7789` (4.5:1 on bg; white on it 4.89:1) | `#85AFC0` (as supplied) | ≥ 4.5:1 on bg and surface |
| `primary-content` (text on primary) | `#FFFFFF` | `#141F25` (7.1:1) | higher-contrast of surface/background |
| `accent-ink` (accent text/icons) | `#5B7763` (4.53:1) | `#A0C3A7` (as supplied) | ≥ 4.5:1 |
| `accent` fill (selected chips, tints) | `#829F8A` with `text` on it | `#A0C3A7` with `background` on it | label contrast ≥ 4.5:1 or large/bold |
| `muted-text` (meta, captions) | `#567079` (4.84:1 bg / 5.26:1 surface). The brand `muted` `#61787D` is only 4.30:1 on background, so it's used on surfaces only. | **`#B2C3C5`** (brand token; 9.19:1 / 7.34:1) | ≥ 4.5:1 on both |
| `field-border` (inputs, essential boundaries) | `#8796A2` (3.04:1) | `#8C9BA7` (4.69:1) | ≥ 3:1 on surface |
| `divider` (decorative separation only) | **`#DCE4E1`** (brand `border`) | **`#354851`** (brand `border`) | no minimum; never the only cue |
| `brand-star` (logo/compass star only, never a UI state colour) | `#D49A4A` | `#E0A95F` | brand-only; not used for meaning |
| `hover` / `selected` tint | `#F1F5F6` / `#E7EDF0` | `#2A3B45` / `#30434D` | text on top ≥ 4.5:1 (8.7–10.6:1) |
| `info` | = `primary-ink` | = `primary` | always icon + text |
| `success` | = `accent-ink` | `#A0C3A7` | always icon + text |
| `warning` | `#896D2A` (from `#C4A765`) | `#C4A765` (5.78:1) | ≥ 4.5:1 |
| `error` | `#B45253` (terracotta shifted ~12° redder, more chroma) | `#DC8080` (4.75:1 surface) | ≥ 4.5:1; distinct from the software-category colour |
| data marks (light) | `#527F91` `#7E9B86` `#AE9150` `#BF8173` `#8796A2` `#9381B6` | supplied values (`#9381B6` for slot 6) | ≥ 3:1 on surface |

**daisyUI 5 mapping:** `base-200` = background (the page), `base-100` = surface (panels, rows, drawer), `base-300` = divider, `base-content` = text, `primary` = `primary-ink`, `primary-content`, `accent`/`accent-content`, `neutral` = `#8C9BA7`-family, and `info`/`success`/`warning`/`error` from the derived tokens. Charts read `--data-1 … --data-6` and `--data-other`.

**Category → colour mapping (6 slots for 10 categories):** facets and filters keep all 10 categories. Colour encodes **6 groups plus a neutral "Other"**, which also keeps the composition bar easy to read.

| Slot | Colour (light data variant) | Group (categories) |
|---|---|---|
| 1 | `#527F91` blue | Video |
| 2 | `#C4A765` → `#AE9150` ochre | Audio |
| 3 | `#BF8173` terracotta | Software & executables |
| 4 | `#8C9BA7` → `#8796A2` slate | Documents (document, ebook, subtitle) |
| 5 | `#829F8A` → `#7E9B86` sage | Images |
| 6 | `#9381B6` plum | Archives & disc images |
| — | `muted-text` grey | Other |

With the plum swap, all six colours are distinguishable. Colour is still never the only cue:
- a **2 px surface-coloured gap** between segments;
- **fixed segment order** (Video, Audio, Images, Documents, Archives, Software, Other);
- visible dominant-share text ("82% video");
- an accessible text summary;
- the sorted bar list with names in the drawer;
- a **"High-legibility data" option** that adds subtle patterns to segments.

**Theme collection: 24 families × light + dark = 48 themes.** All use Riverstone's schema and are compiled and gated identically. Families based on community palettes keep their original licences (MIT/open) and credit them in `docs/themes.md`.

| Group | Families |
|---|---|
| Official | **Riverstone** (default) |
| Accessibility | High Contrast (AAA 7:1 text), Okabe-Ito (colour-vision-safe categorical set), Paper & Ink (near-monochrome) |
| Community-inspired | Nord, Solarized, Gruvbox, Catppuccin (Latte/Mocha), Rosé Pine (Dawn/Main), Tokyo Night (Day/Night), Everforest, Kanagawa (Lotus/Wave), Dracula (Alucard/Dracula), One (Light/Dark), Ayu |
| Originals (muted, Riverstone-adjacent) | Tidewater (cool blues), Sandstone (warm neutrals), Moss (greens), Ember (rust/clay), Lavender Field (dusty purples), Graphite (cool greys, minimal accent), Sepia (low-blue reading), Midnight OLED (true-black dark / "Daylight" light) |

**Theme compiler** (`web/scripts/themes.ts`, runs at build time):
- **Input:** `themes/*.json` in the exact Riverstone schema.
- **Derivation:** OKLCH lightness shifts only, so hues are preserved. Inks for primary/accent/status, muted text, field border, divider, tints, primary-content, light/dark data variants, and the error hue shift.
- **Gates (the build fails on any):**
  - text ≥ 4.5:1 on background and surface (High Contrast requires ≥ 7:1);
  - every `*-ink` ≥ 4.5:1;
  - field border and focus ring ≥ 3:1;
  - every data colour ≥ 3:1 on surface;
  - categorical minimum pairwise ΔE_ok ≥ 5 under normal vision. Below that it's a warning, plus a deuteranopia/protanopia simulation report. Riverstone is grandfathered with the mitigations above.
- **Output:**
  - Riverstone light + dark go into the **critical CSS** (~1.5 KB).
  - All other themes go into **`themes.json`** (~5 KB gzipped), fetched lazily the first time you open Appearance or switch themes, then applied as CSS custom properties on `:root`. It doesn't count toward the 20 KB CSS budget.

**Theme selection UX (Settings → Appearance, plus a command-palette action "Change theme"):**
- **Mode:** System / Light / Dark. System follows `prefers-color-scheme` and switches live. Choosing the family and the mode are independent, and there's an advanced option to pick different families for light and dark.
- **Picker:** a radio group of preview cards, each showing a mini result row, chips, a composition bar and a button drawn with that theme's tokens. It's keyboard-navigable, the name is text (not only a swatch), and there's a "Contrast: AA / AAA" badge. Selecting a card applies it instantly, previewing the whole UI; Undo is available.
- **Filters** for the picker: All, Light-first, Dark-first, Accessibility, Originals, Community.
- **No flash on load:** a tiny inline pre-paint script (< 1 KB) reads the theme id and cached variables from `localStorage` and sets them before first paint. The choice is also saved to the server settings so it follows the account across devices.
- **Custom theme:** duplicate any theme or start from the schema. You edit the 5 base colours + 6 categorical colours, with a **live contrast report** (pass/fail per derived token, colour-blind simulation), and can import/export JSON in the Riverstone schema. Saving a theme that fails AA needs an explicit "Use anyway" and shows a persistent warning badge on the picker card.

### 10.19 Brand assets (`Lodestar_Final_Assets`)
**Pack contents** (all 63 files verified against `SHA256SUMS.txt`):
- **Tagline:** "Every node is a new discovery".
- **`brand/`:** brand board (reference) and the mascot master (1254² transparent PNG).
- **`logos/`:** horizontal, stacked and wordmark lockups in light/dark (transparent PNG).
- **`icons/`:**
  - compass symbol as **SVG (509 B each, light/dark)** plus PNG 64–512;
  - mascot app icons in light/dark (256/512/1024) and full mascot (256/512/1024).
- **`favicons/`:** `.ico`, PNG 16–512 (the compass on a dark disc), apple-touch, android-chrome, `site.webmanifest`.
- **`tokens/`:** Riverstone `colors.json` and `theme.css`.
- **`visuals/`:** 14 opaque **724×1086 portrait PNGs (~1.0–1.25 MB each)** covering 9 states in light and dark. Aliases are intentional (`found_nodes` → `searching_crawling`, `offline_idle` → `welcome_onboarding`).

**Repository placement:**
- Originals go in `brand/source/`, committed as plain files (under GitHub limits; LFS is blocked by the cloud proxy), with `SHA256SUMS.txt` kept and checked in CI.
- Processed outputs are generated into `web/static/brand/` and embedded in the binary.
- Brand tokens go in `web/src/lib/theme/riverstone.json`, which also seeds the theme compiler.

**Asset pipeline** (`web/scripts/assets.ts`, uses `sharp`, runs at build time and is cached):
- **State scenes:** AVIF and WebP at 360 / 540 / 724 px wide, served via `<picture>` + `srcset` with explicit `width`/`height` (no layout shift), `loading="lazy"` and `decoding="async"`.
  - Target ≤ 45 KB per AVIF at 540w.
  - All 14 originals (~15 MB) become ≈ 1–1.5 MB of variants in the binary.
  - **Never part of the 100 KB first load.**
- **Logo lockups** (raster today): trimmed and converted to AVIF/WebP at 1× and 2× of their display sizes (e.g. a 240 px wordmark → ~5 KB). **Request:** vector SVG versions of the wordmark, horizontal/stacked lockups and the mascot. When they arrive they replace the rasters automatically, giving sharper output and smaller files.
- **Compass SVG:** minified and converted so its fills read CSS variables (`--brand-surface`, `--brand-primary`, `--brand-accent`, `--brand-star`), with the Riverstone values as fallbacks. One inline mark then adapts to all 48 themes, at ~0.5 KB.
- **Icons for packaging:**
  - Windows: a multi-resolution `.ico` (16–256) made from `mascot_app_dark_256`, used for the MSI, service, tray and Start-menu shortcut.
  - macOS `.icns` for later.
  - The 256 px PNG for the Unraid CA template `<Icon>` (served from the repo's raw URL).
  - The README header uses the horizontal lockup in light/dark via `<picture>` + `prefers-color-scheme`.
- **Favicons and PWA:**
  - The compass **SVG favicon** (with a `prefers-color-scheme` swap), the `.ico` fallback and the apple-touch icon go in `<head>`.
  - `site.webmanifest` is kept. Its icon paths are fixed to the served `/brand/favicons/…` location, and `theme-color` is set per theme with `<meta name="theme-color" media=…>`.
  - **Request:** a maskable (safe-zone) 512 px icon for Android installs.

**Where each asset is used** (`minimalist-visual-style`: imagery only where it supports comprehension or brand expression; `application-interface-design`: empty and error states explain the situation and offer one next step):

| Asset/state | Used in | Not used in |
|---|---|---|
| Compass mark (inline SVG) | Top-bar logo mark + product name in UI text; favicon; loading splash on very first boot | — |
| Horizontal / stacked lockup | First-run welcome, sign-in/setup-token screen, About, README, docs | App chrome, while working |
| Wordmark + tagline | Welcome and About screens | Search UI |
| Mascot app icon | Windows installer/tray, Unraid CA, PWA install, README badge | In-app UI |
| `welcome_onboarding` | First-run wizard step 1; `offline_idle` = crawler paused/idle full state | — |
| `searching_crawling` / `found_nodes` | Empty-index cold start (Search "The index is still growing" full-state version, when < 1k torrents); Insights "Starting up" state | Banners and rows |
| `indexing` | Index rebuild screen, bitmagnet import progress, first index build | — |
| `no_results` | Zero-result state, placed **after** the recovery suggestions in reading order | Partial/"≈" states |
| `connection_error` | API unreachable / server restarting full-page state; wizard network-check failure | Inline errors (banners stay text-only) |
| `updating_repair` | Upgrade/migration in progress, reindex blue/green swap, rollback, maintenance | — |
| `empty_state_explore` | Search idle with no query (alongside stats and recent searches), empty Discover/Alerts/Collections | — |

**Illustration rules:**
- **Copy and the primary action come first** in DOM and reading order. Illustrations are decorative (`alt=""`) because the message is always in the text next to them.
- **Framing:** a 12 px radius frame, no shadow or border glow, no animation (`motion-ui`), and never full-bleed page backgrounds (minimalist: no decorative backgrounds).
- **The variant follows the active theme's mode** (light/dark), not the OS setting. Non-Riverstone themes show the same Riverstone-toned art inside a neutral surface frame.
- **Responsive** (`responsive-layout`):
  - ≥ x-large: asymmetric two-column (art 5 / text 7 of 12 columns, vertically centred).
  - large: art 240 px tall above the text.
  - medium/small: art 160 px tall, and **hidden when viewport height < 600 px** (show/hide), so the next step stays above the fold.
- **"Show illustrations"** toggle in Appearance. It's off automatically in low-resource mode and when `Save-Data` is on.
- **Brand gold** (`brand-star`) appears only in the logo, compass and art. It's never used for status or data, so it can't be confused with the ochre "Audio" colour or warnings.
- **Typography:** in-app text uses Atkinson Hyperlegible Next/Mono. The lockups keep the wordmark as artwork, so brand fidelity holds where the logo appears.

**Tagline and copy:** "Every node is a new discovery" appears on Welcome, About, README and the empty-index state, not in working chrome. The brand board's description ("a DHT crawler and client that explores the distributed hash table…") seeds the README/About copy, edited for UX-writing rules.

## 11. Deployment

- **Allocator & size:** use **mimalloc** in all builds (musl's allocator is a severe bottleneck for multithreaded Rust). CJK segmentation dictionaries are an optional download on first enable, not embedded, so the base image stays at about 30 MB instead of 100+ MB.
- **Docker:** a multi-arch (amd64/arm64) image, static musl build on `distroless/static`. One container, with `/data` as a volume. Ports: `8090/tcp` (web) and `6881/udp+tcp` (DHT/peers). It supports `PUID`/`PGID`/`TZ`, and the `HEALTHCHECK` runs `lodestar healthcheck`. A `docker-compose.yml` example is included. The docs cover bridge networking with UDP mapping vs `network_mode: host` for better DHT reachability, plus IPv6.
- **Unraid:** a Community Applications template (`packaging/unraid/lodestar.xml`) with icon, WebUI `http://[IP]:[PORT:8090]`, `/mnt/cache/appdata/lodestar` → `/data`, and port mappings. Appdata belongs on the cache SSD/NVMe pool, not the array, because index I/O is random. The template uses the **pool path (`/mnt/cache/...` or the named pool), not `/mnt/user/...`**: `/mnt/user` goes through Unraid's FUSE `shfs` layer, which is slow for mmap-heavy workloads and risky for SQLite WAL. The app detects a FUSE/network filesystem at startup and warns. There's also an optional gluetun/VPN variant of the template. Defaults are `PUID=99`/`PGID=100` (nobody:users). Optional **split paths**: `/data/hot` (indexes + hot columns, must be on SSD/NVMe) and `/data/cold` (an attached `cold.db` holding file-tree blobs and the optional raw archive). Caveat: cold data is written continuously as torrents arrive, so putting it on the parity array keeps disks spinning and slows writes. The recommended layout is both on the cache pool, with only backups going to the array.
- **Windows:** an `x86_64-pc-windows-msvc` build. The **MSI** (cargo-wix) installs to Program Files, puts data in `%ProgramData%\Lodestar`, and registers a **Windows Service** (windows-service crate). It also adds firewall rules for the DHT port and a Start-menu shortcut that opens the UI. A portable zip with an optional tray icon covers non-service use, and a winget manifest is planned. **Code-signing** is needed to avoid SmartScreen and Defender friction. The installer offers (opt-in, explained) a **Defender exclusion for the data directory**: real-time scanning of index segment and WAL writes can cut throughput several-fold. **Ephemeral-port exhaustion:** hundreds of short-lived outbound TCP metadata connections pile up in TIME_WAIT. The defaults (dynamic range 49152–65535, 120 s TIME_WAIT) cap sustained churn at roughly 130 new connections/s. Mitigations: a connection-rate limiter sized to the OS range, `SO_LINGER(0)` aborts for failed handshakes, and connection reuse where peers allow it. The dashboard shows a warning, with the `netsh int ipv4 set dynamicport` remedy documented. The service runs at **below-normal process priority** so desktop use stays smooth. Windows mmap semantics prevent deleting files that are still mapped, so segment garbage collection must retry deletes. CI includes a Windows soak test for merge/GC and hot-column remapping.
- Also: Linux/macOS tarballs and a systemd unit. One `config.toml` with environment-variable overrides (`LODESTAR_*`); most settings are editable in the UI and persisted in the DB.
- **CLI subcommands:** `serve` (default), `reindex`, `backup` (SQLite online backup; indexes are rebuildable), `import <dir|.torrent>`, `vacuum`, `healthcheck`, `stats`, `companion <list|install|remove|compose>` (§18.10; `compose` prints the Docker service for a companion), `worker --join <url> --token <t>` (§19: run as a remote worker).
- **Worker mode** (§19): the same image and binary. Docker: `lodestar worker` with `LODESTAR_JOIN`/`LODESTAR_TOKEN`; Unraid: a "Lodestar Worker" template variant; Windows: the installer's "Join an existing Lodestar" option, installed as a service. The coordinator's cluster port (default `8091/tcp`, mTLS only) is the only thing workers need to reach; the UI/API port stays private.
- **Companions** (§18.10): managed child processes on Windows and bare-metal Linux; on Docker/Unraid, generated compose services or pre-filled CA templates, plus auto-detection and auto-configuration. Managed-container mode via a restricted Docker socket proxy is advanced and off by default.

## 12. Security & operations

- First-run setup requires a **one-time setup token printed to the logs/console** (the Jupyter/Portainer pattern), so nobody else on the LAN can claim a fresh headless instance. It then creates an admin password (argon2). Session cookies. API keys for Torznab and integrations. Optional trusted reverse-proxy header auth. CSP and security headers. Rate limiting on the API.
- **Upgrades:** an automatic SQLite snapshot before any schema migration, with `lodestar rollback`. Docker tags: `x.y.z`, `x.y`, `latest`, `nightly`. Optional in-app "update available" check (no telemetry, ever).
- **Cluster security** (§19.6): mutual TLS with a cluster CA created on first use; one-time join tokens (15-minute expiry); per-worker certificates with immediate revocation; workers receive only the have-filter, assignments, sealed credentials and leases; 0.5% re-verification sampling with quarantine and purge-by-worker.
- **Companion supply chain** (§18.10): a signed catalogue pinned per release, with SHA-256 and upstream signatures verified before anything runs. Every install needs confirmation; companions run with the least privilege, bound to localhost or the internal network; credentials are random and encrypted at rest.
- **Content safety:** the CSAM hash/keyword blocklist is **on by default** and applied before storage. Matches are dropped, never stored or indexed.
- Observability: `tracing` structured logs, Prometheus metrics for every pipeline stage, and the built-in dashboard.
- Graceful shutdown flushes batches, commits the index and persists the DHT state. Crash-safe: SQLite is the source of truth, and the index catches up on startup from a `last_indexed_id` watermark.

## 13. Repo layout (Cargo workspace)

```
crates/
  bencode/          zero-copy decoder/encoder (+ proptest fuzz)
  dht/              KRPC, routing table, BEP 5/32/42/43/51/33, harvest strategies
  peer-wire/        BEP 3/10/9 metadata exchange; uTP (BEP 29) later
  metainfo/         v1/v2/hybrid info-dict model, file tree, extras, .torrent parsing
  tracker-scrape/   UDP (BEP 15) + HTTP (BEP 48) batch scrape
  release-parser/   name parser + golden corpus tests
  classify/         extension registry, composition, content type, junk score, grouping/MinHash
  store/            SQLite schema & migrations, blob codecs, zstd dicts, hot columns
  search/           Tantivy schemas, tokenizers, query language, ranking, facets, percolator, regex prefilter
  pipeline/         crawler → fetch → enrich → store → index orchestration, backpressure
  api/              axum REST/SSE/Torznab/OpenAPI, auth
  integrations/     download clients, notifications
  sources/          plugin API (manifest, SourceCtx, events) + crawl scheduler (budgets, DRR, auto-balance)
  sources-*/        one crate per plugin family: importers (incl. defunct-site dumps), torznab (Prowlarr/Jackett, usenet),
                    feeds (nyaa/animetosho/nostr/lemmy/json-apis), irc (predb, private announces), clients (your download clients),
                    archive (IA/academic), enrich (srrdb/dat/predb/wikidata/tmdb/anidb/…), outputs (torznab/stremio/export), i2p, ed2k, ipfs
  cluster/          coordinator ↔ worker protocol (postcard over mTLS WebSocket), leases + fencing, have-filter replication,
                    placement, politeness token leases, spool; the built-in local worker uses the same interfaces
  companions/       companion manager: signed catalogue, verify/install/supervise (native), compose/CA templates, detection, API adapters
  lodestar/         binary: config, CLI, Windows service, embeds web/build
web/                SvelteKit 2 + Svelte 5 SPA, Tailwind v4 + daisyUI 5, Bits UI, TanStack Virtual, uPlot (lazy), Phosphor (compiled);
                    src/lib/ui (shared primitives) · src/lib/features/* · src/lib/charts (SVG primitives); vitest + Playwright + axe + size-limit
docs/               … + metrics.md (metric contracts), ui-glossary.md, design-tokens.md
packaging/          docker/, unraid/, windows/ (wix), systemd/  (icons generated from brand/); companion compose snippets and CA template links (i2pd, qBittorrent, Prowlarr, SABnzbd, aMule, Kubo, gluetun)
brand/source/       Lodestar_Final_Assets originals + SHA256SUMS.txt; processed by web/scripts/assets.ts
bench/              synthetic dataset generator (10M torrents), criterion benches, size reports
docs/               architecture.md (this plan), query-language.md, operations.md, relevance.md
.github/workflows/  ci (fmt, clippy, test on linux/windows/macos), bench gates, release (multi-arch docker, MSI, tarballs)
```

## 14. Milestones

| M | Deliverable | Exit criteria |
|---|---|---|
| 0 | Workspace, CI, `bencode`, `metainfo`, `store` schema + file-blob codec, synthetic data generator | Size benchmark shows ≤ budget for blobs; round-trip property tests pass |
| 1 | **Source plugin API + crawl scheduler v1 + provenance (§18)**, with the DHT node as plugin #1 (BEP 5/42/51 + passive), tracker-announce peer finding + PEX, TCP + MSE metadata fetch, hostile-input limits + fuzzing, persistence, metrics, **Insights rollup/cohort/event counters emitted from the start** (§7), a basic status page | Sustained new-torrent ingest on a home connection at "Balanced" without router/NAT saturation; no unbounded memory over 24 h; measured harvest and metadata success rates replace §3's rough cold-start estimate |
| 2 | Tantivy torrent index, tokenizer, query language v1, REST search, minimal UI. **Add-on: `lodestar import-bitmagnet <postgres-url>`**, a read-only, resumable, batched import of `torrents`, `torrent_files`, `torrent_contents`/`content` titles, `torrents_torrent_sources` and dates. bitmagnet keeps no file rows for torrents over its `save_files_threshold` (default 100), so those are flagged `files_partial` and queued for background DHT metadata re-fetch. Imported torrents keep bitmagnet's `created_at` as `discovered_at`, with `age_confidence=imported`. | p95 targets met on the 10M synthetic set; NRT < 5 s; importer round-trips a sample bitmagnet DB with counts reconciled and no writes to the source |
| 3 | Enrichment: extension registry, composition, release parser, effective title, content type, junk score, file index | Parser golden corpus ≥ 95% field accuracy; `file:(…)` queries work |
| 4 | Swarm health: BEP 33, UDP/HTTP tracker scrape scheduler, hot columns, trackers count, history; scrape and Serve rollups | Seeders/trackers filters and sorts within targets |
| 5 | Full UI per §10: tokens/theme, primitives, Search (facets, builder, chips, states), detail drawer + file tree, Compare, Discover, Alerts, **Insights** (overview story, 5 stage pages, digest, exports, print layout), Settings + first-run wizard, keyboard, SSE live | `design-review` has no blocking findings; `analytics-validation` matrix (incl. Insights reconciliation) passes on the fixture set; reporting overhead within the §3 budget; Playwright (breakpoints × themes × densities, keyboard-only, reduced motion) green; axe clean; `size-limit` and Lighthouse budgets met |
| 6 | Packaging: Docker multi-arch, Unraid template, Windows MSI + service, first-run wizard | Clean install → crawling → search on all three targets in CI/smoke tests |
| 7 | Torznab, download clients, notifications, grouping/"same file", regex prefilter, uTP, `.torrent` import, opt-in plugins | Feature tests; uTP raises metadata success rate (measured) |
| 8 | Hardening: relevance suite + NDCG gate, perf/size gates, docs, 1.0 | All gates green; docs complete |

**MVP cut line:** this scope is large for a small team. **v0.5 = M0–M3 + the minimum of M5 + Docker**: a crawler with MSE, search with the query language, effective titles and composition, and the core UI. Swarm health (M4), Windows MSI, Unraid CA, Torznab, plugins, uTP, compare and grouping follow in v0.6–1.0. Each milestone ships something usable.

**v0.5 also includes the importer framework** (M2 add-on): the bitmagnet importer plus Rutracker dumps, MagnetDB (if access is granted), RARBG/magnetico/bitmagnet exports and DMM hashlists, so the cold start is skipped.

**Remote workers (§19):**
- **M3:** the **worker boundary**: prepare/commit split, claim/grant leases, and the coordinator's built-in local worker. It lands before enrichment so all processing runs on the worker side from the start, and single-machine behaviour is unchanged.
- **v0.6:** **remote workers v1**: join/mTLS, have-filter replication, DHT (`sharded(keyspace)`), fetch and processing on workers, Settings → Workers, per-worker Insights. It shares the signed-update machinery with the companion manager.
- **v0.8:** placement for singleton, account and overlay plugins on workers, and companions hosted on workers.

**Sources roadmap:** the full ranked source roadmap, from v0.5 through the overlay networks (I2P v1.1, eD2k/Kad v1.2, IPFS v1.3), is in §18.3.

**Later (v1.x):** Saved search → auto-send rules (opt-in). The micro-piece prober. Embedded WireGuard.

## 15. Risks & mitigations

- **uTP maturity in Rust:** ship TCP-only first and add uTP behind a flag; measure the gain.
- **Mutable stats vs immutable index segments:** use hot-column arrays (§7) and never reindex for swarm changes.
- **Release parser accuracy:** a golden corpus with a test for every regression (sourcing below).
- **DHT etiquette, ISP throttling, legal exposure:** polite defaults, intensity caps, metadata only, blocklists before storage, clear docs.
- **Windows:** UDP throughput and AV false positives → signed binaries, Windows CI runner, service-mode tests.
- **Single SQLite writer:** a dedicated writer task with batched transactions; WAL readers never block it; benchmark scrape-update bursts.
- **Index rebuild time at 50M docs:** parallel segment building in `reindex`; measure it in M2. Blue/green directories mean it never causes downtime.
- **Tracker scrape cost & bans:** a full sweep of 10M hashes × 20 trackers is about 2.7M UDP requests per pass, enough to get banned. Scraping is **prioritized and budgeted**: new torrents once; viewed, favorited, saved-search-hit and recently searched torrents often; everything else on a slow decay schedule, all within per-tracker rate limits. A tracker that returns zeros for unknown hashes (opentracker) counts as "reporting" only when s+l+c > 0. So the trackers count is a *sampled, aging* signal, and the UI shows its age.
- **Release-parser training data:** our crawl is unlabeled. Seed the corpus from permissively licensed test suites (check guessit/PTN licenses before copying) plus hand-labelled samples, and grow it from user corrections ("fix parse" in the detail view writes a local override and an optional exportable test case).
- **"Discovered" ≠ release date:** handled by the backfill flag and the estimated-year field (§6), but it's the most likely source of user confusion. The copy and docs must be explicit.
- **Reporting slowing the core system:** Insights reads only rollups. Exports and compaction run in chunked, throttled background jobs, and the overhead budget (§3) is benchmark-gated. The failure mode to watch is a long-running read transaction starving WAL checkpoints; reads are kept short and chunked by design, and a test checks the WAL size stays bounded during an export.
- **Misleading process numbers:** funnels computed by event time would show false conversion rates. The cohort design, "incomplete" labelling and "unique (≈)" disclosure (§10.9.3) are mandatory, and reconciliation tests (§10.17) guard them.
- **Plugin sprawl:** 30+ plugins could swamp one maintainer. Mitigations: one plugin API, contract tests with recorded fixtures, per-family crates, the roadmap order (§18.3), and a rule that a plugin which stays broken for a release gets disabled by default with its health shown, rather than blocking the release.
- **Legal exposure from restricted and overlay sources:** off by default, with consent classes, VPN egress, isolated tiers, one-click purge and the non-negotiable CSAM blocklist (§18.7). Private-tracker results never leave the instance.
- **Scheduler starvation or unfairness:** deficit round robin with min shares, hard caps, importer write-share limits, property tests (§18.9) and Insights visibility of shares and auto-balance moves.
- **Distributed-systems bugs** (double counting, lost keys, split brain): the coordinator is the single source of truth (no consensus protocol), leases carry fencing tokens, results are idempotent by batch id, the local worker shares the remote code path, and the multi-worker chaos tests (§19.8) gate releases.
- **A rogue or compromised worker:** mTLS, least data, re-verification sampling, quarantine, purge-by-worker, and an authoritative blocklist check at commit.
- **Scope/maintainability:** a large Rust codebase for one maintainer. Mitigate with strict crate boundaries, the MVP cut line, and reusing mature crates (tantivy, axum, rusqlite, tokio, encoding_rs, mimalloc, utoipa) wherever they aren't on the critical path.

## 16. Self-review log (issues found in draft 1, now folded in above)

| Sev | Area | Issue in draft 1 | Resolution |
|---|---|---|---|
| High | Perf | Claimed exact counts + facets "in one pass" **and** "cost grows with the page size, not the hit count". These contradict each other; a broad query (e.g. `1080p`, 5M hits) would blow the 30 ms budget. | Counts capped/estimated, facets in a separate cancellable request, sampled at > 1M hits, result cache (§8) |
| High | Perf | Seeder/freshness priors inside the score break WAND pruning, so every match gets scored | Two-phase ranking: re-rank the top ~1k (§8) |
| High | Crawler | No MSE/PE encryption, so a large share of peers would refuse metadata | MSE in M1 (§6) |
| High | Size | The size budget left out the file index; `swarm_history` daily rows would reach billions | Honest ~2.3 KB/torrent all-in; "smart" file index; ring-buffer history only for active/interesting torrents (§3, §7, §8) |
| High | Data truth | A fresh crawler makes "discovered" date filters misleading for old content | Backfill flag + estimated release year + explicit labels (§6, §10) |
| High | Ops | DHT fan-out can saturate home-router NAT tables; no VPN story | Conntrack-safe defaults and warnings; interface binding, kill-switch, gluetun docs (§5, §11) |
| Med | Search | Cursor pagination over NRT commits → duplicate/missing rows; no stale-request cancellation | Searcher leases; server cancel tokens + client abort (§8) |
| Med | Search | Phrase over concatenated paths could match across two file names; regex verification needed per-candidate SQLite reads | Position gap between files; name stored in the docstore (§8) |
| Med | Search | A tokenizer/parser change forces a full reindex with downtime | Versioned index directories, background rebuild, atomic swap (§8) |
| Med | Swarm | Tracker scrape sweeps get you banned; "trackers count" semantics fuzzy; "0" vs unknown | Budgeted, prioritized scraping; s+l+c > 0 rule; "never scraped" sentinel; age shown (§7, §15) |
| Med | Metadata | Legacy charsets, oversize/hostile info dicts, duplicate v2-truncated hashes | encoding_rs detection, hard limits + fuzzing, unified v1/v2 identity (§6) |
| Med | Deploy | Unraid `/mnt/user` FUSE + mmap/WAL; Windows Defender + mmap deletion; musl malloc; embedded CJK dicts bloat the image | `/mnt/cache` default + FUSE detection; opt-in Defender exclusion + GC retry + soak test; mimalloc; dicts on demand (§11) |
| Med | Security | Headless first-run could be claimed by anyone on the LAN; no pre-migration backup | One-time setup token; auto snapshot + rollback (§12) |
| Med | UX | Query language power was locked behind syntax; dead-end empty states; no version comparison; live updates could reorder the list | Smart chips, natural-phrasing parse, zero-result recovery, compare view, in-library badges, stable live feed, freshness labels, mobile staged filters (§10) |
| Low | Scope | No MVP line for a very large scope | v0.5 cut line + v1.x backlog (§14) |
| Low | Accuracy | "BM25F" wording (Tantivy sums per-field BM25) | Corrected (§8) |

**Final-pass findings (draft 2 → 3):**
| Sev | Area | Issue | Resolution |
|---|---|---|---|
| High | Size | The raw info-dict archive was "kept by default" at an estimated 0.5–1.5 KB. But `pieces` makes real info dicts 10–60 KB, i.e. hundreds of GB at 10M torrents. | Never store `pieces`; the archive is opt-in and `pieces`-stripped; re-derivation uses the file blob + extras (§3, §6, §7) |
| High | Correctness | SQLite rowids can be reused after deletes, which would hand stale hot-column stats and index docs to a new torrent and break the percolator watermark | Monotonic `AUTOINCREMENT` ids (§7) |
| Med | Correctness | Torrent and file indexes commit separately, so a crash could make `file:(…)` reference unindexed torrents | Shared id watermark, idempotent catch-up (§8) |
| Low | Consistency | Contradictions: "only `id` stored" vs name stored; file paths "without positions" vs positional gap field; Unraid path given as both `/mnt/user` and `/mnt/cache`; auth details had drifted into the content-safety bullet; MSE missing from the M1 row; history-size arithmetic (80 → 160 B); completed counts presented as a release-year source; unsourced cold-start percentages | All reconciled in place |

**Cross-check against the Gemini blueprint (draft 3 → 4):** it independently arrived at the same core design (Rust single binary, Tantivy, zstd-dictionary file trees, mmap swarm array, cuckoo dedup), which is useful confirmation.

*Adopted:* the opt-in NFO/header micro-piece prober (with consent + "unverified" labelling); structural fingerprints; `ambiguous` flag + resolution source; exact `payload_sig` + v1 per-file md5/sha1/ed2k identity; `has:`/`sig:`/`!`/`age:` syntax; the swarm-consistency fake-seeder signal; rediscovery-driven scrape priority; community tracker-list auto-refresh; SOCKS5 + embedded WireGuard (v1.x); Windows ephemeral-port exhaustion handling + below-normal priority; Unraid PUID 99/PGID 100 and optional hot/cold path split.

*Rejected or corrected:*
- **Factually wrong:** it extracts `creation_date`, `created_by`, `comment`, `encoding`, `announce-list` and web seeds from DHT-fetched torrents, and builds filters on them (`created:`, `embedded_trackers:`). BEP 9 returns only the info dictionary, and all of those keys live outside it. The BEP 10 handshake carries the *peer's* client name, not the torrent's `created by`. These fields exist only for imported `.torrent` files (§3).
- **Unmeasured numbers:** 5–22 ms p99 at 50M, 14.8 GB vs 118 GB, "87% smaller", 25k infohashes/s, 20k swarm updates/s and the 7-week timeline aren't measurements. 25k hashes/s would also be abusive DHT behaviour. Our budgets stay targets gated by benchmarks.
- **Mechanism errors:**
  - Tantivy doesn't run "SIMD intersections over Roaring Bitmaps" on postings; it uses its own block-compressed posting lists.
  - A "single monotonic score inside a custom collector" that includes swarm priors defeats block-max WAND, so our two-phase re-rank stays.
  - An MPHF can't index a growing hash set; an in-RAM SwissTable costs about 1.4 GB at 50M torrents. The SQLite unique index stays.
- **Fjall LSM instead of SQLite:** it gains write throughput we don't need. It loses SQL, mature backup tools and a home for relational user state, which would mean a second store anyway. Not adopted. (rkyv zero-copy is moot behind zstd; rejected.)
- **Tauri tray app + WebView2 UI:** a second app build to maintain for little gain over a tray icon that opens the browser. Deferred.
- **Decided:** keep **Svelte 5** (not React; Lodestar's components are reference designs, not ported code). **Bring back the bitmagnet importer** as an M2 add-on (§14).

**Reassessment against `ui-design-skills` (draft 4 → 5):** §10 was rewritten into a full UI plan. Changes to earlier decisions:

| Earlier plan | Skill | Change |
|---|---|---|
| Composition donut in the detail view | `analytics-chart-selection` (donut only for a few parts; sorted bars for exact comparison) | Sorted horizontal bar list with files/bytes/%; the compact category bar stays in rows |
| Bundle "< 200 KB" | user priority (smallest/fastest) + `data-visualization-design` performance spirit | ≤ 100 KB first load, ≤ 45 KB route JS; charts lazy-loaded |
| Unspecified fonts, icons, radii, motion and spacing | `typography-ui` (fonts overridden by user: Atkinson Hyperlegible Next/Mono), `iconography-ui`, `component-geometry`, `motion-ui`, `layout-spacing` | Atkinson Hyperlegible Next/Mono, Phosphor Bold, 16/12/8/4 radii, 150–250/300–500 ms motion, Fluent spacing ramp |
| Lodestar particle background and decorative effects | `motion-ui` (no ambient motion), `minimalist-visual-style` | Removed |
| Single-key shortcuts always on | `application-interface-design` accessibility + WCAG 2.1.4 | Disable/remap setting; inactive while typing |
| Facet counts | `analytics-validation` | Disjunctive facets with independent reconciliation tests; "≈" when sampled |
| Crawler dashboard as a list of metrics | `dashboard-storytelling` | Status → movement → drivers → segments → exceptions → detail, with an interaction map and drill-through to Search |
| Instant filters everywhere | `responsive-layout` (re-architect) | Desktop instant; phone bottom sheet with staged Apply and a live count |
| Charts depend on hover | `analytics-chart-selection` / `data-visualization-design` | Every chart gets "View as table" + a text summary; missing data shown as gaps |

**Colour decided (draft 5 → 6):** **Riverstone** is the default/official palette, with 24 families × light/dark selectable (§10.18).
- The contrast audit found that light-mode `primary` (4.0–4.4:1), `accent` (2.7–2.9:1) and 4 of the 6 categorical colours (2.3–2.9:1) fall short of AA in their text/graphic roles. Fixed with hue-preserving derived ink/data tokens; the brand values are unchanged.
- Sage and seafoam are near-duplicates (ΔE_ok ≈ 1.9), mitigated with gaps, fixed order, text and patterns.
- 10 categories collapse into 6 colour groups + a neutral Other for colour encoding only.

**Decided (draft 6 → 7):** `minimalist-visual-style` adopted. Seafoam replaced by plum `#9381B6` (tuned from the suggested `#9C8AA5` for colour-blind separation). The brand asset pack is integrated (§10.19). From the brand tokens, adopted: `border` as the divider, dark `muted`, and `brand-star`. The brand board's older swatches (Stone/Slate/Teal/…) are superseded by `tokens/colors.json`.

**Insights process-story dashboard (draft 7 → 8):** "Crawler" became **Insights**, a progressive dashboard telling the whole story **Discover → Fetch → Understand → Index → Serve** (§10.9), with five stage drill-through pages, a weekly/daily digest, CSV export and a print layout.
- It reads only incremental **rollup and discovery-cohort tables** (§7): ≤ 50 ms queries regardless of corpus size, ≤ 50 MB/year of storage, and an overhead budget of search p95 +≤ 5% and ingest −≤ 2% (§3).
- The funnel uses discovery cohorts rather than event-time counts, so conversion rates are honest. Unfinished cohorts are labelled "incomplete", and pre-storage uniqueness is labelled approximate.
- Counters are emitted from M1, so history exists when the UI lands in M5.
- The Insights chunk budget is 45 KB, up from the crawler dashboard's 40 KB, for the funnel and stage pages (§10.1).
- Review follow-ups: counters-only flushes while ingest is idle; an hourly index watermark so searchable counts can be rebuilt exactly; post-storage removals kept out of the funnel so final cohorts never change; evicted/gave-up loss reasons and a "new to us" definition of harvested, so cohort conservation holds.

**Sources, plugins and network tiers (draft 8 → 9):** every source is now an on/off **plugin** under one API and crawl scheduler (§18).
- **Ranking:** sources are ranked by 0.4 × rarity/breadth + 0.3 × volume + 0.3 × ease, with risk deciding default state and consent class.
- **Roadmap:** importers in v0.5; Prowlarr, Nyaa, BEP 48, Internet Archive and official lists in v0.6; enrichment and outputs (including a Stremio addon) in v0.7; Anna's Archive, Nostr and Lemmy in v0.8; then I2P (v1.1), eD2k/Kad (v1.2) and IPFS (v1.3).
- **Priority and agents** are shares of one global budget (deficit round robin, min/max shares, hard politeness caps, optional auto-balance by marginal utility including unique finds), not raw thread counts, because sources share the same bottlenecks.
- **Data model:** provenance bitset, claims, an unverified tier and network tiers.
- **Safety:** CSAM blocklist universal and non-disableable; private-tracker results local-only.
- **Rejected:**
  - BiglyBT DHT, Tribler/Rats gossip, LAN discovery and BEP 44/46 sniffing: low yield or no standard protocol;
  - Soulseek and DC++: community rules ban crawlers;
  - Telegram: ToS, account required, mostly direct files;
  - Tor onion indexes and BitTorrent over Tor: IP leaks, harm to Tor, CSAM prevalence;
  - Freenet/Hyphanet: same problems.
- **Gemini corrections recorded:** Nostr uses secp256k1 Schnorr, not Ed25519; BEP 48 full scrapes are HTTP-only and often disabled; PreDB carries no info hashes.
- **Review follow-ups:**
  - Tracker-announce peer finding is consent-gated and off by default, because an announce lists our IP in the swarm.
  - `private_source` keys never reach public DHT lookups, public trackers or PEX (BEP 27).
  - The shared id sequence is an explicit `id_seq` allocator, and promoted items keep their ids.
  - Budgets are per torrent at a given corpus size, with importers previewed.
  - Scheduler v1's M1 scope is pinned down: fetch-pool DRR, hard caps and user-interest priority.
- **More sources, live connections and the companion manager (draft 9 → 10):**
  - **Catalogue grew to 22 ranked sources, each with a Mode (live / poll / import):**
    - Usenet NZB indexers (a new `usenet` tier);
    - Archive Team copies of defunct torrent sites;
    - adult trackers / Sukebei (hidden by default);
    - private-tracker IRC announces;
    - AnimeTosho;
    - direct JSON APIs (YTS, EZTV, apibay);
    - your own download clients.
  - **Enrichment** gained No-Intro/Redump/TOSEC DATs and a live PreDB IRC feed.
  - **Public indexers** are reached through Prowlarr rather than scraped directly.
  - **New rejections:** DDL warez blogs, Japanese P2P networks, Chinese closed ecosystems, Matrix rooms, WebTorrent WSS trackers, peer co-membership inference, and source directories (useful by hand only).
  - **Live connections:** eD2k shared-file browsing (files kept, sharers discarded) and IPNI gossipsub.
  - **New universal rule, "No profiles of people":** peer addresses are never stored with what they share.
  - **Companion manager (§18.10):** fully managed installs on native platforms from a signed, pinned catalogue; on Docker/Unraid, guided compose/CA templates with auto-detection and auto-configuration, because a self-installing container would need the Docker socket, which is effectively root. Managed-container mode is advanced and off by default.

**Remote workers (draft 10 → 11, §19):** "split roles" (v1.x) is replaced by a coordinator plus any number of remote workers running the same binary.
- **What runs where:** workers run crawling (sharded DHT keyspace), metadata fetch and CPU processing (prepare); the coordinator keeps the writer (commit), index, rollups and UI.
- **Why more machines help:** BEP 42 limits each IP to 8 node-id prefixes, so more public IPs (workers on other networks or VPN exits) widen DHT coverage. Workers behind one home router don't.
- **Exactly-once accounting** comes from fleet-wide leases with fencing tokens, with "harvested" counted at grant on the coordinator.
- **Global politeness caps** are enforced through token leases; the have-filter is replicated to workers.
- **Security:** mTLS join tokens, revocation and re-verification sampling.
- **Timing:** the boundary lands in M3, so enrichment is born on the worker side; remote workers ship in v0.6.

**Still open (decide during M0–M2 spikes, with measurements rather than guesses):** reusing librqbit crates vs our own DHT; whether the trigram field earns its size at 10M+ names; the smart-file-index thresholds; the actual cold-start harvest and metadata success rates.

**M0 implementation decisions:**
- **Size budget split and first measurement.** See §3: 1.0 KB for the store and 0.5 KB for the torrent index, with blobs ≤ 0.6 KB. Measured with `bench`'s deterministic generator at 100k torrents (seed 1, 31.3 files each):
  - file-tree blob: 1,011 B raw → **344 B** stored (zstd 19 + a 112 KiB trained dictionary, 2.9×);
  - whole SQLite file: **565 B** per torrent (blobs 412 B including page overhead, `torrents` rows 119 B, hash index 31 B).
  
  Synthetic names repeat more than real ones, so real data will compress somewhat worse. M1 re-measures on a real crawl. The CI gate stays either way.
- **zstd level 19 by default.** Compared with level 12, it saves 6% of blob bytes (344 vs 366 B), but store throughput drops from 9.8k to 3.4k torrents/s. Live ingest is tens of torrents per second, so 19 wins. Bulk paths (the bitmagnet import, `reindex`) can choose a lower level through `StoreConfig`.
- **The schema grows per milestone.** Migration v1 holds only the canonical torrent tables, `settings` and `zstd_dicts`. Each milestone adds its own tables as a new migration, written by the code that uses them, rather than shapes guessed now:
  - `blocklist` in M1;
  - `swarm*` and `trackers` in M4;
  - `saved_searches`, `alerts`, `user_items` and `collections` in M5/M7.
  
  Migrations refuse a newer database and snapshot an existing one before changing it (§12).
- **Columns beyond the §7 sketch:**
  - `torrents.name_raw`/`name_enc` keep the raw name bytes, but only when they aren't plain UTF-8.
  - `torrents.info_size` records the info-dict size (§6).
  - `torrent_files.dict_id` names the zstd dictionary. The frame's own dictID field is switched off to save bytes.
- **Duplicate lookup by any swarm hash** is two indexed lookups joined with `UNION ALL`. Written with `OR`, SQLite scanned the whole table, which made ingest 17× slower at 100k torrents. A test pins the query plan.
- **Text encoding.** There's one encoding decision per torrent, in this order: `.utf-8` keys, then valid UTF-8, then the `.torrent` `encoding` hint, then chardetng over all names at once. Encoding ids come from an append-only table of `encoding_rs` encodings. On synthetic data, chardetng misreads about 1.5% of short legacy-GBK names, guessing ISO-8859-5. The raw bytes are always kept, so those names can be re-decoded later.
- **Parsing policy:**
  - bencode has a lenient mode (unsorted keys and leading zeros accepted) and a strict, canonical-only mode. Duplicate keys and trailing bytes are rejected in both.
  - metainfo reports recoverable anomalies as warnings and doesn't reject the torrent for them: piece-count mismatch, an inconsistent hybrid, suspicious path segments, bad per-file hashes and malformed v2 entries.
  - Total size is capped at `i64::MAX` so every value fits SQLite.
- **Fuzzing ships with M1**, as §14 schedules. The M0 parsers and the blob codec are covered by property tests: round-trips, strict-implies-canonical, and no panics on arbitrary or corrupted input.

**Still open after M0:** chardetng accuracy on real legacy-encoded names (measure in M1, and consider an encoding allowlist if ISO-8859-x misreads show up).

**M1 implementation decisions (DHT node):**
- **Single-owner actor.** One task owns the sockets, routing tables, transaction table, lookups and rate limiters, with no locks. Counters are shared atomics.
- **Sending uses a non-blocking std clone of each socket.** tokio's `try_send_to` fails until the reactor has seen the socket become writable, and that silently dropped a fresh node's bootstrap packets. Sockets are created with socket2, and IPv6 sockets are IPv6-only, so an IPv4 and an IPv6 socket can share port 6881.
- **Routing hygiene:**
  - Nodes that only queried us wait in a replacement cache, and get a verification ping when their bucket has room.
  - A responder that answers under a different id than the one queried is neither routed to nor trusted.
  - Lookups skip our own ids and addresses. Other nodes list us in their replies, and querying ourselves cost one of the K lookup slots, which the in-process network test caught.
  - The one-entry-per-IP rule skips BEP 42's exempt (private and loopback) addresses.
- **Virtual ids.** We answer each peer with the virtual id closest to its id, so a given peer always sees us under the same id. All ids are regenerated (BEP 42) when the majority-voted external IP changes.
- **Thin tables keep bootstrapping.** Bucket refresh only runs after 15 minutes. So while a table holds fewer than 32 nodes, the self-lookup is repeated every 30 ticks (30 s by default) until the table fills.
- **No live DHT from the development sandbox.** The cloud sandbox has no DNS and no outbound UDP, so the node is verified against the §17 in-process network. That network checks Kademlia convergence: every node knows its 8 true closest nodes, except where a full bucket keeps older good nodes. The M1 exit criteria that need the real network (sustained ingest at "Balanced" without NAT saturation, measured harvest and metadata rates) have to be measured on a real deployment.

**M1 implementation decisions (peer wire, `lodestar-peer-wire`):**
- **One crate call is one peer.** `fetch(addr, info_hash, opts)` tries one peer under one deadline (8 s total, 4 s to connect) and returns the verified info dict or a typed error. Each error maps to a §7 loss reason: unreachable, timed out, refused, encryption failed, invalid, oversize. Choosing peers, retries and socket budgets belong to the shared metadata-fetch pool (§18.4), which calls `fetch` once per socket token.
- **MSE by default, with plaintext fallback.** The default mode (`Prefer`) offers RC4 and plaintext inside MSE. If the peer drops our DH key, we retry plain BitTorrent while the deadline allows. `Require` (RC4 only) and `Disable` exist for egress policies and tests. MSE's 768-bit DH and RC4 only obfuscate the protocol; they aren't treated as security.
- **Verification accepts either key type.** The info dict must hash to the key by SHA-1 (v1 or hybrid) or by truncated SHA-256 (a pure v2 swarm key, BEP 52).
- **Hostile-peer limits:** metadata over 16 MiB is refused before allocating; an extended message is at most one 16 KiB piece plus 1 KiB of header; other messages are skipped up to 4 MiB; pieces must have been requested and have the exact length; at most 64 requests in flight and 200 PEX peers kept.
- **Nothing about the peer is stored with the torrent.** The peer's client name (from its BEP 10 handshake) and its PEX list come back with the fetch, for aggregate client stats and peer finding only (§18.7).
- **Interop:** besides the in-crate fake peer, the fetcher was checked by hand (harness not in the repo) against libtorrent 2.1 seeding v1, hybrid and v2 torrents with encryption forced, enabled and disabled. Every combination succeeded except the two that can't (a seeder that forces encryption with ours disabled, and a seeder with encryption disabled with ours required).
**M1 implementation decisions (store v2: provenance, ids, Insights, queue):**
- **Migration v2** adds `id_seq`, `sources`, `torrents.sources`/`first_source`, `rollup_hour`, `cohort_hour`, `events` and `fetch_queue`. Tables nothing writes yet arrive with their first writer, each as its own migration: `rollup_day`/`cohort_day` with nightly compaction, `index_watermark_hour` with search (M2), and `torrent_claims`/`unverified_items` with the first claim-bearing source.
- **`id_seq` starts above every id `torrents` ever used,** including deleted ones (from `sqlite_sequence`). Ids are taken one at a time inside the writer's transaction, so a duplicate or a rollback costs no id. `torrents.id` stays `AUTOINCREMENT` as a second guard.
- **Source ids are 0–63 and never reused,** even after a purge, so a stale bit can't be attributed to a different plugin. `register_source` is idempotent per plugin id and records the running version. A torrent rediscovered by another source gains that source's bit, with an update only when the bit is new.
- **Cohorts carry the source:** `cohort_hour` is keyed by (cohort, source, stage, reason), so the §10.9 funnel can be split by source as §18.5 asks. Metric, dimension, stage and reason codes are opaque to the store. The pipeline defines them (append only).
- **Counters are additive only.** One `Store::write` commits torrents, counters, cohort counts and events in one transaction. With no torrents it's the counters-only flush §7 calls for. Gauges such as scheduler shares go to Prometheus for now, and to rollups once the auto-balancer needs their history.
- **The fetch queue is persisted without peers:** hash, source, cohort, strategy, priority, attempts and the `private_source` flag (so a private key stays out of public peer finding across restarts). Peer addresses are transient (§18.7) and stale after a restart anyway.
- Cost on the 50k synthetic corpus: +4 B per torrent (594 → 598 B).

**M1 implementation decisions (sources crate, scheduler v1):**
- **The plugin contract is dyn-safe.** `SourcePlugin::run` returns a boxed future, so plugins live in one `Vec<Arc<dyn SourcePlugin>>`. A plugin gets a `SourceCtx`: its `SourceId`, an `Emitter`, a shutdown signal, and (for peer finders) the pool's peer requests. Politeness caps, the JSON-schema config, the KV state store and the per-host HTTP client join the manifest and context with the first plugin that needs them (Nyaa, §18.3). The DHT needs none of them.
- **Two ways to emit.** `try_emit` never waits and counts what it drops; live sources use it, so the DHT can't stall. `emit` waits for room; importers use it, so a dump backs up into the importer and not into memory.
- **The pool is one task that owns everything:** per-key state, per-source ready queues, the interest queue, timers and connections per IP. Fetches are child tasks keyed by task id, so a panicking fetch is still accounted for, as a failure of kind `Invalid`, rather than lost.
- **DRR with unit cost.** Each fetch costs 1 and a source's turn adds its weight (Low/Normal/High/Max = 1/3/6/10). Keys a user is waiting on are served before any source. The caps are max sockets (400), connections per IP (2), queued keys (200k), peers per key (32) and outstanding lookups (256).
- **Peer finding:**
  - A key with no untried peers asks every peer finder (`get_peers`) and waits 30 s.
  - If no peers turn up, the next lookup follows after 2 min, then 8 min, then 32 min.
  - After 4 lookups the key is lost: `no peers` if none was ever found, otherwise the last fetch failure (timed out, refused, encryption failed, invalid, unreachable).
  - Oversize is terminal at once, because it's a property of the torrent.
  - `unreachable` (peers found, none accepted a TCP connection) is a loss reason the §10.9 list lacked. Its code is 10.
- **`private_source` keys never reach a peer finder.** A public sighting of the same key makes it public. A private sighting never makes a public key private.
- **Eviction** drops the oldest key that's still looking for peers. Keys with peers, keys being fetched and user-interest keys stay. If none qualifies, the incoming key is dropped. Either way it's counted once as `evicted`.
- **Funnel accounting happens in the pool:** harvested, peer found and fetched, or lost with a reason, exactly once per key. Restored keys aren't counted as harvested again. `FunnelCounts` accumulates the counts in memory for the writer to flush.
- **The queue survives restarts without peers** (store `fetch_queue`). Restored keys start with a fresh lookup, and the lookup that was in flight at shutdown isn't counted as used.

**M1 implementation decisions (pipeline crate):**
- **Four stages, one task each, with bounded channels between them:** plugins, then intake, then the fetch pool, then the writer.
  - Intake checks the have-filter: a "no" goes to the pool, a "yes" goes to the writer as a rediscovery.
  - The pool waits on the writer. That's the backpressure, so memory stays flat when the disk is slow.
  - The pool-to-writer channel holds 64 entries, because each one carries a whole info dict.
- **The have-filter is a keyed Bloom filter:** 10 bits per hash, 7 hashes, about 1% false positives, over every stored swarm hash (v1 and truncated v2).
  - It never answers "no" for a stored hash.
  - The writer confirms every "yes" in the same transaction that adds the source's bit. A false positive goes on to the pool, so nothing new is lost to the filter.
  - Past capacity, the writer rebuilds it at twice the size from the store.
  - This replaces the cuckoo filter §5 sketched. Deletion isn't needed until retention policies exist, and a rebuild covers it then.
- **The writer owns the store.** Each transaction runs on the blocking pool with the store moved in and back out. Batches are up to 256 dicts or 250 ms.
  - Funnel and rollup counters are drained into every transaction, or into a counters-only one every 5 s when they're dirty (§7).
  - The fetch queue is saved every 5 minutes by a helper task that asks the pool. The writer never waits on the pool, so they can't deadlock.
- **The blocklist runs in the writer, before storage,** on the key and on every swarm hash of the parsed dict, so a v2 alias can't slip past (§18.7). M1 has hash entries only. Name patterns come with classification (M3).
- **A dict that hashes to its key but doesn't parse as metainfo** is lost as `invalid`.
- **The peer wire grows the metadata buffer as pieces arrive, never up front.** A peer that claims 16 MiB costs nothing until it sends the bytes. Pre-allocating would have let hostile peers pin 400 × 16 MiB.
- **Shutdown order:** plugins (15 s each, then aborted; the DHT saves `state.bin`), then intake, then the pool (returns its queue), then the writer. The writer drains, records a `stop` event, saves the queue and checkpoints the WAL.
- **Plugin state:** registration and source ids come from the store. Which plugins run comes from the binary's config until the settings UI exists. Stored states (paused, disabled) aren't enforced yet.
- **Metric codes:**
  - `rollup_hour` metrics: harvested (by source and strategy), rediscovered, dropped, stored.
  - `events`: start and stop.
  - The DHT's strategy codes are sample 0, get_peers 1, announce 2 and lookup 3.

**M1 implementation decisions (the `lodestar` binary):**
- **Commands:** `serve` (the default) and `healthcheck`. The other §11 subcommands arrive with the features they serve. Arguments are parsed by hand, because two commands and one flag don't justify a parser crate.
- **Config:** one `config.toml`, found at `--config`, else `$LODESTAR_DATA_DIR/config.toml`, else `data/config.toml`. Every field has a default, and unknown keys are errors, so typos surface.
  - Environment overrides: `LODESTAR_DATA_DIR`, `LODESTAR_HTTP_BIND`, `LODESTAR_DHT_PORT`, `LODESTAR_DHT_ENABLED`, `LODESTAR_INTENSITY` and `LODESTAR_LOG`.
  - Defaults follow §11: web on `0.0.0.0:8090`, DHT and peers on 6881, data in `lodestar.db` and `dht-state.bin`.
  - Unless `fetch.max_sockets` is set, the intensity preset also sets the fetch pool's socket cap: Low 100, Balanced 400, Aggressive 1000.
  - Blocklists are files of hex hashes.
- **HTTP (axum, read-only):** `/` is the status page, plus `/api/status`, `/metrics` (Prometheus text, every stage) and `/healthz` (200 while every source plugin runs).
  - The status page is one embedded HTML file using the §10.18 Riverstone role tokens, light and dark, with no external requests. It's served with a CSP and renders peer-supplied names with `textContent` only. The Svelte UI (M5) replaces it.
  - **There's no authentication yet.** Nothing on these endpoints changes state, but torrent names are visible to anyone who can reach the port. The §12 setup token and auth arrive with the real API, before anything writable is exposed.
- **The status page reads funnel and recent torrents through a read-only SQLite connection** (`Store::open_reader`) beside the writer. The torrent total is the count at start plus what the writer has stored, so polling never scans the table.
- **`healthcheck` uses plain std networking,** because the distroless image has no curl. It exits 0 only on a 200 from `/healthz`.
- **Shutdown on SIGTERM or Ctrl-C:** HTTP drains first, then the pipeline stops in order, saving the queue and DHT state. The test suite runs the real binary through start, health, status and SIGTERM.
- **mimalloc** is the global allocator (§11). Release builds keep line tables for backtraces: 58 MB unstripped, 6.9 MB stripped. Packaging strips or splits them.

**M2 implementation decisions (search index):**
- **Torrent-level paths are words only, without positions.** On the 50k synthetic corpus, path positions alone took 372 B per torrent: 690 B in total against the 512 B index budget. A smaller gap (8) and indexing each directory name once only got that to 600 B.
  - Dropping positions brings the whole index to **256 B per torrent**, leaving room for M3's title fields and the trigram field.
  - Phrase and NEAR within one file name move to the file index (M3), which keeps positions on each file's `name`, so nothing in the query language is lost.
  - Directory names are indexed once each, not once per file.
- **Numeric fields are fast-only, with no inverted index.** Tantivy runs range queries on fast fields, and the term dictionaries cost about 45 B per torrent for nothing.
- **The tokenizer is one pure function, shared by indexing and queries:**
  - It splits on punctuation and on camelCase and letter/digit boundaries.
  - Compounds sit at the position of their first part: `s01e02`, `x265`, `webdl` from `WEB-DL`, `51` from `5.1`, `h264` from `H.264`, `dd+`.
  - Folding: NFKD (so full-width forms fold too), marks stripped, a few letters expanded (ß, æ, ø, ł…), lowercase. CJK and Hangul skip NFKD, which would split syllables into jamo.
  - CJK runs become overlapping bigrams until the optional dictionary segmenter (§11).
  - Words over 24 chars aren't split (hashes and junk), and tokens over 64 bytes are dropped.
  - Each value is capped at 512 tokens, and path text at 20k tokens per torrent.
  - It's fuzzed (`fuzz/tokenizer`) and property-tested. Offsets always fall on char boundaries of the original text.
- **A query word matches** its parts as a phrase on `name` (or all its parts, on `path`), or any compound it forms. So `web-dl`, `webdl` and `WEB DL` all find `WEB-DL`.
- **The watermark lives in the Tantivy commit payload,** atomically with the documents it covers, rather than in SQLite. With one index, that's strictly safer: a crash can lose work but never skip or duplicate a torrent.
  - When the file index arrives (M3), the cross-index watermark goes to SQLite as §8 describes.
  - Indexing follows `Store::ids_after` in id order. That works because the single writer commits ids in order.
- **Near-real-time indexing:** a `LiveIndexer` thread reads through its own read-only SQLite connection. It polls every 500 ms and commits every 2 s or 5k documents. The test asserts a new torrent is searchable within the 5 s budget.
  - The index directory is `data/index/v{schema}-t{tokenizer}`, so a schema or tokenizer bump builds a fresh index.
  - `index_watermark_hour` (searchable time per hour, §7) comes with the API slice, where the writer can record it.
- **Measured** on 50k synthetic torrents: indexing runs at about 6,200 torrents per second.

## 17. Verification (once built)

- `cargo test --workspace` (unit, property, and golden tests), `cargo clippy -D warnings`, `cd web && npx vitest run && npx playwright test`.
- UI gates (§10.17):
  - `npx size-limit` (first load ≤ 100 KB gzipped);
  - Lighthouse CI (LCP/INP/CLS);
  - axe on every screen × theme;
  - Playwright visual snapshots at 320/390/768/1024/1366/1920/2560;
  - the `analytics-validation` fixture matrix (facet counts, histogram bins, and Insights rollups/cohorts reconciled against independent SQL, the Tantivy doc count and `/metrics`);
  - **source plugin gates (§18.9):** contract tests, scheduler property tests, politeness, provenance/purge, universal blocklist across every plugin kind, private-source isolation, consent gating;
  - **cluster gates (§19.8):** a multi-worker chaos test (worker kills, partitions, coordinator restarts) with exactly-once funnel reconciliation, fencing, fleet-wide politeness caps, placement failover, revocation, re-verification sampling, spool replay, version skew, and single-machine parity;
  - the **reporting-overhead benchmark**: search and ingest benchmarks rerun with Insights polling + the digest job + an export running, checked against the §3 overhead budget;
  - a `design-review` pass with no blocking findings.
- **In-process DHT test network:** spawn about 50 nodes plus fake peers serving known info dicts, and assert harvest → fetch → store → index → `search` returns them within 5 s.
- `cargo bench -p search` against the synthetic 10M dataset checks the p95 table and the size report against budgets. CI fails on regression.
- Relevance suite: `cargo run -p search --bin relevance` checks NDCG@10 against the stored baseline.
- Smoke: `docker compose up` → open `http://localhost:8090` → the dashboard shows nodes and a growing torrent count within minutes → a query with `file:(ext:mkv size:>2GB) res:2160p seeders:>=1` returns results with matched files highlighted. A Windows CI job installs the MSI, starts the service and polls `/healthz`.

## 18. Sources: plugins, scheduling, network tiers and roadmap

Lodestar's reach comes from **many sources, each a plugin you can switch on or off**. The Mainline DHT crawler (§5) is just the first. This section defines the plugin model, ranks every candidate source, sets the roadmap, and specifies how sources share the machine (priority, agents, budgets).

### 18.1 Plugin model
- **Compiled in, not dynamically loaded.** Every plugin is a Rust module behind a cargo feature (`src-i2p`, `src-ed2k`, `src-ipfs`, `src-annas`, …).
  - Official builds include all plugins, **disabled by default except the Mainline DHT**.
  - A `minimal` build flavour leaves out the overlay-network plugins entirely.
  - There are no third-party binaries or ABI issues. WASM-sandboxed community plugins are a post-1.0 idea.
- **Plugin kinds:**

| Kind | Emits | Examples |
|---|---|---|
| `discovery` | keys (info hash / ed2k hash / CID) plus optional claims | Mainline DHT, BEP 48 full scrape, Nyaa RSS, Nostr, Prowlarr |
| `importer` | the same as `discovery`, but run as a resumable **job** with progress | bitmagnet DB, Rutracker dumps, MagnetDB, RARBG/magnetico exports, DMM hashlists |
| `peer-finder` | peers for a key that has none yet | tracker announce (BEP 15), PEX (BEP 11), DHT `get_peers` |
| `enrichment` | name hints, IDs, alt titles, out-of-info-dict fields | torrent cache, srrDB, PreDB, Wikidata, TMDB, AniDB, MusicBrainz, debrid status |
| `network` | a whole network tier: its own keys, metadata fetch and link format | I2P BitTorrent, eD2k/Kad, IPFS |
| `output` | exposes Lodestar to other tools | Torznab server, Stremio addon, bitmagnet-format export |

- **Manifest (per plugin):**
  - id, version and kind; networks; what it emits;
  - the **external endpoints** it contacts (domains/protocols, shown before enabling);
  - its **risk level and consent class** (§18.7);
  - **hard politeness caps** that the user can't exceed (e.g. Nyaa: ≥ 5 min between polls; BEP 48: ≤ 1 full scrape per tracker per day);
  - a JSON-schema config, from which the settings UI is generated;
  - health (status, last success, last error); metrics dimensions (fed into the Insights rollups); purge support.
- **Runtime contract** (sketch):
  ```rust
  trait SourcePlugin: Send + Sync {
      fn manifest(&self) -> &Manifest;
      async fn run(&self, ctx: SourceCtx) -> Result<()>; // until cancelled
  }
  // SourceCtx: budget tokens (acquire per resource class), an emitter
  // (Discovery / Claim / NameHint / Enrichment / Peers), a KV state store
  // (cursors, `since`, ETags), an HTTP client with per-host limiter and the
  // plugin's egress (direct / VPN interface / SOCKS), cancellation, metrics.
  ```
- **A discovery event** carries: network; key; optional peer hints; optional **claims** (name, files, trackers, IDs such as `imdb:`/`tmdb:`/`anidb:`, description, publisher identity); provenance (source id, external reference, time); confidence.
- **Distribution:** every manifest declares where the plugin may run with remote workers: `everywhere`, `sharded(keyspace)`, `sharded(unit)`, `singleton` or `coordinator` (§19.5).
- **Lifecycle:** enable / pause / disable / purge. **Disabling keeps the data; purging removes it.** Purging deletes items found *only* by that source, plus all of that source's claims. Items other sources also found are kept, with the source's bit cleared.

### 18.2 Source catalogue, ranked
**Score** = 0.4 × rarity/breadth of content + 0.3 × user base/volume + 0.3 × ease of incorporating, each rated 1–5. **Risk** doesn't change the score; it decides the default state and the consent class (§18.7).

| Rank | Source | Kind | Mode | Rarity/breadth | Volume | Ease | **Score** | Risk |
|---:|---|---|---|:-:|:-:|:-:|:-:|:-:|
| — | **Mainline DHT** (BEP 5/51/passive, §5) | discovery | live | 3 | 5 | 3 | core | Med |
| 1 | **Prowlarr/Jackett** (your indexers via Torznab/Newznab; private results local-only; also covers the public indexers, see below) | discovery | poll | 5 | 4 | 5 | **4.7** | Med |
| 2 | **Rutracker catalogue dumps** (published XML exports: titles, hashes, categories, descriptions) | importer | import | 5 | 4 | 4 | **4.4** | Med |
| 3 | **Anna's Archive / LibGen** (torrent lists + metadata dumps mapping files → titles/ISBNs) | importer + discovery | import/poll | 5 | 5 | 3 | **4.4** | High |
| 4 | **Usenet NZB indexers** (your accounts, via Prowlarr's Newznab support; `usenet` tier, local-only) | network | poll | 5 | 4 | 4 | **4.4** | Med |
| 5 | **MagnetDB** research dataset (DHT 2018–2024, 28.6M+ torrents, 950M+ files, IMDb-matched; access terms TBC) | importer | import | 4 | 5 | 4 | **4.3** | Med |
| 6 | **Internet Archive** (item torrents + metadata API) | discovery | poll | 4 | 4 | 4 | **4.0** | Low |
| 7 | **Archive Team / Internet Archive copies of defunct torrent sites** (KickassTorrents hourly dumps, Pirate Bay archives, Demonoid, isoHunt-era datasets) | importer | import | 4 | 4 | 4 | **4.0** | Med |
| 8 | **Nyaa RSS** (info hash, category, trusted/remake flags) | discovery | poll | 3 | 4 | 5 | **3.9** | Med |
| 9 | **RARBG backup + magnetico/bitmagnet community exports** | importer | import | 3 | 4 | 5 | **3.9** | Med |
| 10 | **Adult trackers / Sukebei** (via Prowlarr or the Sukebei feed; the **Adult** media category, hidden unless "Show adult content" is on, §20.9) | discovery | poll | 3 | 5 | 4 | **3.9** | Med |
| 11 | **eD2k/Kad** (eMule/aMule network; keyword publications + optional shared-file browsing) | network | live | 5 | 4 | 2 | **3.8** | Med |
| 12 | **Private-tracker IRC announce channels** (autobrr-style, your accounts; local-only) | discovery | live | 5 | 3 | 3 | **3.8** | Med |
| 13 | **AnimeTosho** (Nyaa mirror with file lists, attachments and media info; Torznab-style feed) | discovery + enrichment | poll | 3 | 3 | 5 | **3.6** | Med |
| 14 | **DMM hashlists** (Debrid Media Manager, crowd-curated, IMDb-linked) | importer | import | 2 | 4 | 5 | **3.5** | Med |
| 15 | **Direct JSON APIs** (YTS, EZTV, apibay): thin optional plugins for continuous polling; Prowlarr already covers them | discovery | poll | 2 | 4 | 5 | **3.5** | Med |
| 16 | **BEP 48 full scrape** (HTTP trackers that allow it) | discovery | poll | 2 | 4 | 4 | **3.2** | Low |
| 17 | **Your download clients** (qBittorrent / Transmission / Deluge APIs; Sonarr/Radarr webhooks): complete `.torrent`s with real trackers and creation dates | discovery + enrichment | live | 2 | 3 | 5 | **3.2** | Low |
| 18 | **I2P BitTorrent DHT** (i2pd companion) | network | live | 5 | 1 | 2 | **2.9** | High |
| 19 | **Academic Torrents + official distro/project lists** | discovery | poll | 2 | 2 | 5 | **2.9** | Low |
| 20 | **IPFS** (Amino DHT harvest + IPNI gossipsub + UnixFS directory listings) | network | live | 3 | 3 | 2 | **2.7** | High |
| 21 | **Nostr NIP-35 / DTAN relays** (WebSocket subscriptions) | discovery | live | 3 | 2 | 3 | **2.7** | Med |
| 22 | **Lemmy/fediverse torrent communities** (ActivityPub follow, pushed to us) | discovery | live | 2 | 1 | 4 | **2.3** | Low |

**Mode:** *live* sources hold a connection and receive new items as they appear; *poll* sources are fetched on a schedule within their politeness caps; *import* sources run as one-off or periodic jobs.

**Public indexers come through Prowlarr.** Prowlarr ships definitions for hundreds of public indexers (The Pirate Bay via its JSON API, 1337x, TorrentGalaxy, YTS, EZTV, Knaben, Rutor, LimeTorrents, regional trackers and more), so the Prowlarr plugin reaches them without separate plugins. Direct plugins (rank 15) exist only for sources with stable JSON APIs worth polling continuously. Scraping HTML pages directly is brittle and against those sites' terms, so it isn't done.

**Accelerators** (unranked, because they raise the yield of every key-bearing source): **tracker announce (BEP 15) for peer finding** when the DHT finds none (off by default and consent-gated, because an announce lists our IP in the swarm; see §6), and **PEX (BEP 11)** during metadata fetches.

**Enrichment plugins, by value:**
1. **srrDB** (exact scene file names, sizes and CRCs, including the contents of RAR sets)
2. **No-Intro / Redump / TOSEC DAT files** (exact names, sizes and CRCs for known games, ROMs and old software; torrent file lists match them exactly, so ROM sets and abandonware are identified with confidence. The DATs are metadata only.)
3. **Torrent cache** (fetch the `.torrent` by hash: real `creation date`, `created by`, `comment`, `announce-list`, web seeds)
4. **Wikidata** (cross-linked IDs, multilingual alternative titles)
5. **TMDB / TVDB**
6. **AniDB / AniList** (plus AnimeTosho's file and media info, rank 13)
7. **MusicBrainz**
8. **PreDB** (HTTP dump/API, plus an optional **live IRC feed** of releases as they're packed)
9. **OpenLibrary**
10. **IGDB**
11. **Discogs**
12. **Steam app list**
13. **Debrid cache status** (your account: an "instantly available" badge)

**Outputs:** Torznab server; **Stremio addon** (browse your index from Stremio on TVs and phones); bitmagnet-format export.

**Watch list:** **Indexarr**, a decentralized, gossip-synced indexer with a Torznab API. Its site didn't resolve when checked in Oct 2026; re-evaluate when it can be verified.

**Rejected** (reasons in the §16 log):
- BiglyBT/Azureus DHT, Tribler/Rats gossip, BEP 14 local discovery, BEP 44/46 sniffing (at most an experiment);
- Soulseek and DC++ (community rules ban crawlers), Telegram (account required, ToS, mostly direct files);
- Tor onion indexes and BitTorrent over Tor (IP leaks, harms the Tor network, high CSAM prevalence), Freenet/Hyphanet;
- direct-download warez blogs and file-host link sites (not P2P, malware-ridden, dead links, nothing to hash);
- Japanese P2P networks: Winny, Share, Perfect Dark (closed or encrypted protocols, prosecutions in Japan);
- Chinese closed ecosystems: Xunlei/Thunder cloud, Baidu Pan shares (walled gardens, account required);
- Matrix public rooms (room rules against bots, CSAM risk, low volume); WebTorrent WSS trackers (they don't broadcast other swarms' hashes, so they aren't a source);
- inferring what a BitTorrent peer shares by seeing its IP:port across swarms (profiling people; see "No profiles of people", §18.7);
- source directories such as FMHY and the r/Piracy wiki are lists of sources, not data. They're useful for finding candidates by hand, not for crawling.

### 18.3 Roadmap (score order, adjusted for dependencies)
| Release | Sources added | Why here |
|---|---|---|
| **M1 foundation** | Plugin API, scheduler v1 (see §18.4 for its scope), provenance; **Mainline DHT** as plugin #1; **PEX**; tracker-announce peer finding (consent-gated, off by default) | Everything else depends on the plugin API and scheduler. The accelerators raise yield for every later source. |
| **v0.5** (M2–M3) | **Importer framework**: bitmagnet DB (already planned), **Rutracker dumps**, **MagnetDB** (if access is granted), **Archive Team copies of defunct sites**, **RARBG/magnetico/bitmagnet exports**, **DMM hashlists**. Enrichment (M3): **No-Intro/Redump/TOSEC DATs** | The largest breadth for the least effort, and it solves the cold start. All use one streaming importer framework plus background DHT verification. DAT matching is cheap and fits the M3 enrichment work. |
| **v0.6** (M4 + early M7) | **Companion manager v1** (§18.10: qBittorrent, Prowlarr + FlareSolverr, gluetun). **Prowlarr/Jackett** (local-only private results; public indexers included), **your download clients**, **Nyaa RSS**, **AnimeTosho**, **BEP 48 full scrape**, **Internet Archive**, **Academic/official lists**; enrichment: **torrent cache** | The top-scoring live sources, once claims/provenance and the unverified tier exist. Prowlarr and a download client need companions, so the manager lands here. The full scrape ships with the M4 tracker scraper. |
| **v0.7** (M7) | **`usenet` tier** (+ SABnzbd/NZBGet companion), **private-tracker IRC announces**, **direct JSON API plugins** (optional). Enrichment: **srrDB, PreDB (+ live IRC feed), Wikidata, TMDB/TVDB, AniDB/AniList, MusicBrainz, OpenLibrary, IGDB, Discogs, Steam, debrid status**. Outputs: **Torznab server, Stremio addon, bitmagnet export** | Turns breadth into findability: ambiguity resolution, multilingual titles, IDs. Usenet rides on the Prowlarr plumbing from v0.6. |
| **v0.8** | **Anna's Archive / LibGen** (restricted consent), **Nostr NIP-35**, **Lemmy** (ActivityPub push). Adult sources need no special tier (§20.9) and can arrive with Prowlarr in v0.6 | Anna's Archive scores high but needs the consent framework and a restricted tier. Nostr and Lemmy are lower volume. |
| **1.0** | Hardening across all plugins | Contract tests, politeness audits, purge correctness |
| **v1.1** | **Network-tier infrastructure + I2P** (i2pd companion) | The first overlay network, at your request, for rare content |
| **v1.2** | **eD2k/Kad** (+ shared-file browsing, aMule companion) | The deepest well of rare content; a new protocol stack |
| **v1.3** | **IPFS** (+ IPNI gossipsub, optional Kubo companion) | Directory listings are worth having, but each harvested CID yields little |

### 18.4 Crawl scheduler: priority, agents and budgets
**Yes, sources get priorities and agent counts, expressed as shares of one global budget rather than raw thread counts.** All sources compete for the same bottlenecks, and more raw concurrency can *lower* total yield: NAT/conntrack saturation, tracker bans, writer contention.

- **Resource classes** (the global budget is set by the intensity preset, §5):
  - `udp_pps` and `udp_outstanding` (DHT, Kad, IPFS);
  - `tcp_sockets` (metadata fetch, eD2k, Bitswap);
  - `bw_down` / `bw_up`;
  - `http_rps` per host;
  - `cpu_enrich` (parsing and classification workers);
  - `writer_share` (fraction of each writer batch);
  - `disk_growth` per day (optional cap).
- **Per-source settings:**
  - **Enabled.**
  - **Priority:** Low / Normal / High / Max, i.e. weights 1 / 3 / 6 / 10, with a numeric weight in the advanced view.
  - **Agents:** the plugin's concurrency unit, labelled in that plugin's own terms:

    | Plugin | One "agent" is |
    |---|---|
    | Mainline DHT, Kad, IPFS | one virtual node ID |
    | Nostr | one relay connection |
    | Prowlarr | one indexer queried in parallel |
    | importers | one parse/verify worker |
    | metadata fetch | a block of 50 sockets |

  - **Min/max share** of each resource class (guarantees and ceilings).
  - **Schedule windows** (e.g. importers and full scrapes 01:00–07:00).
  - **Egress** (direct / VPN interface / SOCKS) with an optional per-plugin kill-switch.
- **Hard caps always win.** A plugin's politeness caps (manifest) and the global budget can't be exceeded by any priority or agent setting. The UI shows each source's **predicted resource use** and warns when settings exceed what the budget can give.
- **Fair sharing:** each resource class is divided by **deficit round robin** over enabled sources, weighted by priority within their min/max shares.
- **Scheduler v1 (M1) is deliberately small:**
  - Per-source queues feed the shared metadata-fetch pool, scheduled by weighted DRR under the global hard caps (outstanding UDP queries, TCP sockets).
  - User-interest fetches jump the queue.
  - Everything else waits until there's more than one live source: the other resource classes, min/max shares, schedule windows, per-plugin egress and auto-balance arrive in v0.6+.
  - The **metadata-fetch pool is shared**: keys from every source queue there, scheduled per source, so a flood from one importer can't starve live DHT discoveries.
  - **User-interest fetches** (you opened an unverified item, or a saved search matched one) jump the queue.
  - **Importers default to Low priority and ≤ 50% of `writer_share`,** so live crawling and search are never starved.
- **Auto-balance** (optional; global toggle with per-source pinning):
  - Every 15 minutes, it measures each source's **marginal utility per resource unit**: new searchable items + 2 × **unique finds** (items no other source has) + 1 × ambiguous names resolved, divided by the resources used.
  - It shifts weights multiplicatively toward higher-utility sources, within your min/max bounds, with 10% hysteresis so shares don't oscillate.
  - Pinned sources keep their manual settings. Every change is logged as an Insights event ("auto-balance moved 8% of fetch budget from DHT to Nyaa").

### 18.5 Provenance, claims and verification
- **Provenance:**
  - `torrents.sources` is a u64 bitset of contributing sources, plus `first_source` (u8), in a column that costs ~9 bytes per torrent.
  - A `sources` table holds plugin ids, versions and state.
  - In-flight keys carry their source id alongside cohort and strategy (§7).
- **Claims:** `torrent_claims(torrent_id, source_id, data BLOB)` stores what a source *asserted*: name, claimed files, IDs, description, publisher. It's stored only for sources that supply claims, zstd-compressed.
  - **Verified metadata** (BEP 9 info dict, a Kad file record, an IPFS root block) is always the truth. Claims are hints and weaker ranking signals.
  - **A claimed file list that contradicts the verified one** adds the junk reason "claim mismatch".
- **Unverified tier:** keys with claims but no metadata yet (dump titles, dead swarms, Nostr events) live in `unverified_items(id, network, key, source_id, claims BLOB, first_at, last_at)`, not in `torrents`, so the verified corpus stays clean.
  - They're indexed with `verified:no`, **hidden from search by default**, shown with a "Show unverified" toggle and an "Unverified" badge.
  - They're queued for low-priority verification fetches, and promoted when metadata arrives.
- **One id space across networks:** all networks allocate ids from a single monotonic sequence, so hot columns, the search index and the percolator watermark (§7/§8) work unchanged.
  - SQLite's `AUTOINCREMENT` sequence is per table, so the shared sequence is an explicit allocator: a one-row `id_seq` table, advanced inside the writer's transaction.
  - Every item table inserts explicit ids from it.
  - An unverified item **keeps its id** when it's promoted to `torrents`, so index documents, hot-column slots and saved-search hits stay valid.
- **Budgets scale with the corpus, not the source count.** Importers can multiply the corpus (MagnetDB alone is 28.6M+ torrents), so the §3 size and RSS budgets are per torrent at a given corpus size.
  - Hot-column arrays are sized by the highest id; they're sparse files, and only pages for live ids are touched.
  - The settings UI shows the projected size before an importer runs (§10.10).
  - Unverified items with no claims worth keeping (bare dead hashes from a dump) are kept only as a capped verification queue, not as rows.
- **Rollups:** the Insights rollups gain `source`, `network` and (with remote workers, §19) `worker` dimensions (§7, §10.9).
- **Worker provenance:** `torrents.worker` (u16, the worker that processed it; 0 = the coordinator's local worker) enables purge-by-worker and quarantine (§19.4).

### 18.6 Network tiers
Every item has a `network` field: `bt` (clearnet BitTorrent), `bt-i2p`, `ed2k`, `ipfs`, `usenet`. **Adult** isn't a network; it's a **media category** (§20.9), hidden unless "Show adult content" is on.
- **Search** gets a Network facet (default: `bt` only) and `network:`, `source:`, `verified:` and `official:` query fields.
- **Each tier** has its own link format and client rules:
  - I2P magnets go only to I2P-capable clients (BiglyBT, I2PSnark, qBittorrent's I2P mode);
  - `ed2k://` links go to aMule/eMule;
  - `ipfs://` CIDs come with an optional gateway link and a warning;
  - `usenet` results are NZB links sent to SABnzbd/NZBGet.
- **One-click purge per tier.**
- **I2P (v1.1):**
  - An **i2pd companion** (§18.10: a separate container on Docker/Unraid, a managed process on Windows/Linux), reached only via SAMv3 on an internal network or localhost.
  - Hidden mode or near-zero transit bandwidth, so we don't act as a relay.
  - The I2PSnark-style DHT is crawled over I2P datagrams, and metadata is fetched over SAM streaming.
  - Adult content is excluded by default on this tier.
- **eD2k/Kad (v1.2):**
  - A Kad DHT node harvests **keyword→file publications** (so names come for free) and source records.
  - Results are ed2k file entries (MD4 hash, name, size, source count).
  - **Cross-network linking:** files whose `ed2k` hash appears in a torrent's per-file hashes (§6) are linked to that torrent.
  - **Shared-file browsing (opt-in):** eD2k clients that allow it answer a "view shared files" request with their whole shared list, often a source of rare files. **Only the files are kept; who shared them is discarded** (§18.7). Asking reveals our presence to that peer, which is covered by the Overlay consent.
- **IPFS (v1.3):**
  - Passive Amino DHT server nodes record `ADD_PROVIDER`/`GET_PROVIDERS`.
  - The root block is fetched over Bitswap (client-only, never re-provided or cached) and classified as directory, file root or chunk; chunks are dropped.
  - Directories are walked with depth/size caps into our file-tree format, so composition, `file:` queries and ambiguity resolution all work.
  - IPNI ingestion is optional: advertisement chains over HTTP, plus **live** announcements on the `/indexer/ingest/mainnet` gossipsub topic.
  - **A phishing/malware classifier is mandatory** (HTML with login forms, wallet-drainer patterns), and IPFS content is **never rendered in-app**.

### 18.7 Safety, legal and privacy rules
- **Universal and non-negotiable:**
  - The **CSAM hash and pattern blocklist** runs on every source, importer and network tier before storage, and **can't be disabled**.
  - Metadata only; content is never downloaded, except through the separately consented micro-piece prober (§6).
  - Every external connection goes through the plugin's egress setting.
- **Consent classes:**

| Class | Applies to | Enabling requires |
|---|---|---|
| None | Mainline DHT, Internet Archive, Academic/official lists, BEP 48, Lemmy | a toggle |
| External | public web services (Nyaa, Nostr, torrent cache, Wikidata, …) | a toggle + the endpoint list shown |
| Account | Prowlarr/Jackett (incl. Usenet), private-tracker IRC announces, your download clients, debrid, keyed APIs (TMDB, IGDB, Discogs) | credentials (encrypted at rest, never exported); results local-only |
| Restricted | Anna's Archive/LibGen and other High-risk content sources; tracker-announce peer finding (it lists our IP in swarms) | a consent screen explaining the legal exposure; VPN strongly advised |
| Overlay | I2P, eD2k/Kad, IPFS | a consent screen + VPN recommendation; isolated tier; one-click purge |

- **No profiles of people.** Peer IPs and IDs are used only briefly, to fetch metadata or count a swarm, and are **never stored alongside what they share**.
  - The index holds facts about content, never "this IP has these files". Swarm statistics stay aggregate counts.
  - That keeps Lodestar from becoming a surveillance tool, matters legally (IP addresses are personal data under GDPR and similar laws), and limits the damage if an instance's data leaks.
  - It applies to every network, including eD2k shared-file browsing.
- **Private-source isolation:** items found only through private-tracker indexers or Usenet accounts (Prowlarr), private-tracker IRC announces, or your own download clients are flagged `private_source`. They're **never** served over Torznab or Stremio, included in exports, or sent into public peer finding (DHT `get_peers`, public tracker announces or PEX), because tracker rules typically forbid redistribution and a public lookup would leak the hash (BEP 27). Their metadata comes only from the private source itself.
- **Politeness:** manifest caps, robots/ToS checks recorded in each plugin's docs, exponential back-off on errors, and a visible "blocked by source" state rather than retry storms.

### 18.8 UI and Insights
- **Settings → Sources** (a new primary settings section; it absorbs the plugin and enrichment settings):
  - Grouped by kind. Each row shows the toggle, a status pill (running / paused / scheduled / error, as icon + text), 24-hour yield and **unique finds** with a sparkline, risk/consent badges, priority, agents (with their unit and predicted resource use), schedule, egress, and **Purge**.
  - **Global Auto-balance** toggle with per-source pinning.
  - **Importers** open a Jobs view with progress, pause/resume and ETA.
  - **Consent dialogs** follow §18.7.
- **Search:** Network, Source, Verified and Official facets; provenance on the detail drawer ("Found by: DHT · Nyaa · Rutracker dump", claim-vs-verified differences).
- **Insights:**
  - The Discover stage gains a **per-source breakdown**: funnel, yield/hour, **unique finds** (the rarity signal), cost (requests, bandwidth, CPU), and auto-balance events.
  - Each overlay tier gets its own segment.
  - A new **"Sources" detail page** ranks sources by marginal utility, the same number auto-balance uses.

### 18.9 Verification
- **Plugin contract tests:** run each plugin against a fake `SourceCtx` and recorded fixtures; check that it honours cancellation, budgets and caps.
- **Scheduler property tests:**
  - shares stay within ε of their weights over time;
  - hard caps and min shares hold under any setting;
  - no enabled source is starved;
  - user-interest fetches are prioritised;
  - auto-balance never violates bounds or oscillates under steady load.
- **Politeness tests:** per-host limits, back-off, schedule windows.
- **Provenance and purge tests:** the bitset is correct; purge removes only a source's unique items and its claims; the rollups stay reconciled after a purge.
- **Safety tests:**
  - a blocklisted fixture hash injected through *every* plugin kind and network is dropped before storage;
  - `private_source` items never appear in Torznab, Stremio or export output, and their hashes never reach a DHT lookup, a public tracker or PEX;
  - unverified items are hidden by default;
  - consent gating can't be bypassed through the API.
- **Companion tests (§18.10):** an install whose checksum or signature doesn't match is refused; companions bind to localhost or the internal network only; companion traffic follows the configured egress and kill-switch; a missing companion puts its dependent plugins in a "Needs setup" state rather than failing; managed-container mode can only create or remove containers on the catalogue list.
- **No-profiles test:** no table or log line stores a peer address together with content keys (schema check plus a log scan in the e2e suite); eD2k shared-file browsing stores files without the sharer.
- **Overhead benchmark:** with all v0.6 sources enabled, the §3 search and ingest budgets still hold.

### 18.10 Companion manager
Lodestar is the **central directory for every source**. When a source or a download needs another program (an overlay router, an indexer manager, a download client), Lodestar **installs or detects it, configures it, and supervises it**. How it installs depends on the platform.

| Companion | Needed for | Release |
|---|---|---|
| **qBittorrent (nox)** | Downloading BitTorrent results; newer versions also have experimental I2P support, so one client may cover both | v0.6 |
| **Prowlarr** (+ optional FlareSolverr) | The Prowlarr source plugin (public and private indexers, Usenet) | v0.6 |
| **gluetun** (containers) / embedded WireGuard (native, §5) | Routing companion traffic through your VPN | v0.6 |
| **SABnzbd or NZBGet** | Downloading `usenet` results | v0.7 |
| **i2pd** | The I2P tier: crawling and I2P downloads | v1.1 |
| **aMule (daemon)** | Downloading eD2k results. The Kad crawler is built into Lodestar; aMule is only for downloads | v1.2 |
| **Kubo** (optional) | Fetching IPFS content locally instead of through a gateway | v1.3 |

- **Native (Windows, bare-metal Linux): fully managed.**
  - Lodestar downloads the companion, verifies it, installs it (per-user or as a service), generates random credentials, configures it through its API, and **supervises** it as a child process: start/stop/restart, logs, health checks, resource limits (Windows Job Objects; cgroups where available).
  - **Signed companion catalogue:** published with each Lodestar release. It pins versions, download URLs per platform/architecture, SHA-256 hashes and upstream signatures where they exist, plus licence and default-config templates. **Nothing runs unless its checksum matches.**
  - **Every install needs your confirmation.** Updates are pinned and you're notified; nothing auto-updates without verification. Uninstall is clean.
  - **Licensing:** companions are downloaded on demand, never bundled into Lodestar's binary or installer, so GPL programs (qBittorrent, aMule) stay separate.
- **Docker / Unraid: guided, not self-installed by default.**
  - A container that creates other containers needs the Docker socket, which is **effectively root on the host**, and Unraid has no supported API for installing apps from inside a container.
  - **Docker:** one click generates the companion's `docker-compose` service (network, ports, `network_mode: service:gluetun` for VPN routing) for you to apply. **Unraid:** one click opens the companion's Community Applications template with every field pre-filled.
  - **Advanced, off by default:** a "managed containers" mode through a **restricted Docker socket proxy** that can only create and remove containers on the catalogue list, with a clear warning.
- **Auto-detection and auto-configuration on every platform.** However a companion was installed, Lodestar finds it (known ports, container names, or a URL you enter), then configures it through its API (credentials, categories, save paths, interface binding) and keeps checking its health.
- **Rules:**
  - **Companions run with the least privilege:** a separate low-privilege process or container, bound to localhost or the internal network. Only the P2P ports they need are exposed.
  - **Companion traffic follows your egress settings:** interface binding on native installs, `service:gluetun` in containers. The kill-switch covers companions.
  - **Lodestar stays a metadata index.** Downloads happen in the client you choose, at your request.
  - **A missing companion never breaks Lodestar.** Dependent plugins show "Needs i2pd · Set up" instead of failing.
- **UI:**
  - **Settings → Companions:** installed and detected companions with health, version, logs, update notices, and Set up / Remove.
  - The first-run wizard gains an optional **"Download client"** step (recommended: qBittorrent).
  - Each source on Settings → Sources shows its dependencies and their state.

## 19. Distributed workers (remote clients)

One Lodestar instance is the **coordinator**: it owns the database, the search index, the UI and the API. Any number of **workers** on other machines add crawling, fetching and processing capacity. A worker is the same `lodestar` binary started in worker mode. It has no database, no index and no UI, and it **dials out** to the coordinator, so workers behind NAT need no inbound ports apart from their own P2P ports. This replaces the earlier "split roles" idea (§14, v1.x).

### 19.1 What scales out, and what doesn't
| Work | Where it runs | Why more machines help |
|---|---|---|
| **DHT harvesting** | workers (+ coordinator) | **BEP 42 ties node ids to the external IP: only 8 id prefixes per address** (`dht::secure_id`). Passive harvesting sees only the traffic routed to our ids, so **every extra public IP widens keyspace coverage**. Each IP also gets its own per-IP limits. |
| **Metadata fetch** (peer wire, MSE) | workers | More sockets, more source IPs (fewer per-IP refusals), and each machine's own router/conntrack budget |
| **Processing:** parse the info dict, derive fields, build the compressed file-tree blob, classify, release-name parse, junk score, DAT/srrDB matching | workers | CPU-bound and independent per torrent; it scales linearly |
| **Shardable plugins** (importer chunks, BEP 48 per tracker, Prowlarr per indexer) | workers | Parallel parsing and polling |
| **Overlay crawlers** (I2P, Kad, IPFS) and their companions | the worker that hosts the companion | Can sit on a VPN box or a different network |
| **Writer, search index, Insights rollups, UI/API** | coordinator only | One source of truth. The ingest ceiling is a budget (§3), not a scaling target |
| **Politeness-capped sources** (trackers, HTTP feeds) | anywhere, but caps are **global** | More workers never multiply a source's request rate |

**Honest limits:**
- Workers behind the **same home router share its NAT/conntrack table and its public IP**. Several workers on one LAN add CPU and sockets, but not DHT coverage or router headroom. Put workers on **different networks or VPN exits** for crawl gains.
- DHT yield is bounded by real network activity, so returns diminish after a handful of IPs. Insights (§19.7) shows the marginal gain per worker so you can tell when another one stops helping.

### 19.2 Topology and protocol
- **Connection:**
  - Each worker holds one long-lived connection to the coordinator's **cluster port** (default `8091/tcp`): WebSocket over TLS with **mutual TLS**, frames in `postcard`, batches zstd-compressed.
  - The UI and API port stays LAN-private. Only the cluster port may be exposed, ideally over a private overlay (Tailscale/WireGuard).
- **Messages:**

| From → to | Message | Purpose |
|---|---|---|
| W → C | `Hello` | protocol version, capabilities (cores, RAM, external IPs, companions present, egress), worker id |
| C → W | `Assign` | plugins to run with their config, budgets/intensity, keyspace shard, sealed credentials for assigned account plugins |
| C → W | `HaveSnapshot` / `HaveDelta` | the replicated have-filter (§19.3), the shared zstd dictionary version, the blocklist version |
| W → C | `Claim` | batched new keys (≤ 512 or every 250 ms) with source, strategy, peer hints and discovery time |
| C → W | `Grant` / `Deny` | leases with a **fencing token** and TTL; denials say "stored" or "held elsewhere" |
| W → C | `Results` | processed torrents (§19.4), idempotent per batch id |
| W → C | `Outcome` | the terminal funnel outcome per leased key (peer found / fetched / lost + reason) |
| W → C | `Counters` | rollup deltas (harvest by strategy, failures, latency buckets) tagged with the worker id |
| C ↔ W | `Heartbeat` | every 5 s: stats, clock offset, lease renewals, politeness tokens |
| C → W | `Drain` / `Stop` / `Update` | graceful drain, revoke, a signed update offer |

- **The coordinator runs a built-in local worker through the same interfaces,** over in-process channels instead of the network. A single machine behaves exactly as before, and there's **one code path** for local and remote work.

### 19.3 De-duplication and leases (exactly-once accounting)
- **Two-level de-duplication:**
  1. **Have-filter, replicated.** Each worker keeps a copy of the coordinator's have Bloom filter (§7 pipeline: 10 bits per hash, about 12.5 MB per 10M torrents). It gets a snapshot on join, then streamed inserts. A rebuild sends a fresh snapshot. Stored keys are dropped locally, with no round trip; a false positive just goes the rediscovery path as it does today.
  2. **Fleet-wide in-flight leases.** New keys are **claimed** in batches. The coordinator checks its authoritative in-flight table and the store, then grants each key to exactly one worker.
     - A key already held elsewhere is denied. The coordinator merges the second source's bit into the lease and **relays the peer hints in memory** to the holder (never stored, §18.7).
- **Accounting:**
  - **"Harvested" is counted by the coordinator at grant time,** once per key across the whole fleet, so "new to us" stays globally true and cohort conservation (§10.9.3) holds.
  - The **holder reports the terminal outcome exactly once.**
  - **Leases** carry a TTL (default 10 min, renewed in heartbeats) and a fencing token. If a worker vanishes, its leases expire and the keys are re-granted, with their cohort, source bits and strategy kept by the coordinator. A late report from the old holder carries a stale token and is ignored, so nothing is counted twice.
  - Outstanding leases are persisted with the coordinator's fetch queue, so they survive restarts.
- **Clock skew:** discovery times are stamped by workers. The coordinator measures each worker's offset from heartbeats and corrects timestamps. A worker more than 60 s off is flagged and asked to fix NTP.

### 19.4 Processing on workers ("prepare, then commit")
- The current writer path splits in two:
  - **Prepare (worker):** verify the info-dict hash (already done during the fetch), parse, run the blocklist pre-check, build the file-tree blob with the shared zstd dictionary, derive fields (composition, payload signature, structural fingerprints), and, from M3, release-name parsing, classification, junk score and DAT/srrDB matching.
  - **Commit (coordinator):** the authoritative blocklist check on the key and every swarm hash plus name patterns, id allocation (`id_seq`), insert, rollups and indexing.
- **What a `Results` record carries:** key and swarm hashes (v1 + truncated v2), the derived row, the compressed file-tree blob, `torrent_extra`, parsed/classified fields, provenance (sources bitset, first source, worker id) and the fencing token. **`pieces` is never sent** (§3), which keeps records to roughly 1–3 KB per torrent instead of 10–60 KB.
- **Trust but verify:**
  - Workers are your machines, authenticated with mTLS.
  - The coordinator still **samples 0.5% of each worker's results for re-verification** by another worker (or the coordinator), which re-fetches the info dict and compares derived fields.
  - A worker whose sample fails is **quarantined**: paused, flagged in the UI, and offered purge-by-worker.
- **Optional later optimization:** building pre-tokenized index documents on workers (Tantivy `PreTokenizedString`) to offload tokenization from the coordinator. Measure before adding it.

### 19.5 Placement and budgets
- **Each plugin manifest gains `distribution`:**

| Value | Examples | Placement |
|---|---|---|
| `everywhere` | metadata fetch, processing, PEX | every worker, within its budget |
| `sharded(keyspace)` | Mainline DHT, Kad, IPFS | each worker gets its own node-id prefixes/regions and BEP 51 traversal ranges, so walks don't overlap |
| `sharded(unit)` | importers (file chunks), BEP 48 (per tracker), Prowlarr (per indexer), Internet Archive (per collection) | units spread across workers |
| `singleton` | Nyaa, Nostr, IRC feeds, Lemmy, your download clients | exactly one active instance, chosen by the coordinator; it fails over if that worker drops |
| `coordinator` | outputs (Torznab, Stremio, export), enrichment caches that need the database | coordinator only |

- **Auto placement by default:** shardable work spreads by capacity, singletons go to the healthiest eligible worker, and overlay plugins go only to workers that have their companion. You can **pin** any plugin to specific workers.
- **Account and private sources** (`private_source`, §18.7) default to the coordinator. You can allow them on chosen workers, and their credentials are sealed per worker. **Restricted and overlay consent applies per worker**, because each machine has its own IP and legal exposure.
- **Budgets:**
  - Each worker has **its own resource budget and intensity preset**, because it has its own NIC, router and CPU. The §18.4 scheduler runs per worker for local resources.
  - **Global resources stay global:** per-host politeness caps (trackers, HTTP sources) come from the coordinator as **token leases** in heartbeats, so the fleet as a whole never exceeds a source's cap.
  - **Writer share** is managed on the coordinator.
- **Backpressure:** if the coordinator's writer falls behind, grants slow down, so workers harvest less and nothing piles up unboundedly anywhere.

### 19.6 Joining, security and lifecycle
- **Joining a worker:**
  1. **Settings → Workers → Add worker** shows a **one-time join token** (15-minute expiry) and ready-to-copy commands:
     - `lodestar worker --join https://coord:8091 --token …`
     - a `docker run` line or compose service
     - an Unraid "Lodestar Worker" template
     - the Windows installer's **"Join an existing Lodestar"** option
  2. The worker creates a key pair and the coordinator signs its certificate (a cluster CA created on first use). Both sides pin each other.
- **Least data:** a worker receives only the have-filter (hash membership), its assignments, sealed credentials for assigned plugins, and leases. It can't query the database, search or UI.
- **Revocation:** **Revoke** invalidates the certificate immediately and returns its leases. **Drain** finishes in-flight leases, then stops.
- **Spool:** while disconnected, a worker keeps harvesting and processing into a **bounded on-disk spool** (default 1 GB or 24 h), then pauses fetching. On reconnect it replays the spool idempotently, by batch id and fencing token.
- **Versions:** the protocol major version must match. The coordinator offers a signed update (§18.10 catalogue mechanism), and workers update after draining.
- **"No profiles of people" (§18.7) applies on workers too:** peer addresses never leave the worker except as transient peer hints relayed in memory.

### 19.7 UI and Insights
- **Settings → Workers:**
  - Each worker shows its status (online / draining / offline / quarantined, as icon + text), external IP and egress, version, uptime, CPU/RAM, assigned plugins and shards, throughput (claimed / fetched / processed per minute), spool size, clock offset and lease count.
  - Actions: Add worker, Drain, Pause, Update, Revoke, Purge data from this worker, edit budget/intensity, and placement pins.
- **Top bar:** the status pill shows the fleet ("Crawling · 3 workers · 4.2k/h").
- **Insights:**
  - The rollups gain a `worker` dimension. The Discover and Fetch stage pages break down by worker.
  - A **"Marginal gain"** view shows how much each worker adds (new keys per hour that no other worker would have seen), to guide where to add the next one.
- **Settings → Sources** shows each plugin's placement (Auto / pinned) and which workers are running it.

### 19.8 Budgets and verification
- **Budgets** (added to §3):
  - **Coordinator ingest** of at least **1,000 new verified torrents/s sustained** (commit + index), benchmark-gated.
  - Worker steady-state **RSS ≤ 256 MB** at defaults.
  - Results ≤ 3 KB per torrent on the wire (median).
  - Grant latency p95 < 50 ms on a LAN.
- **Tests:**
  - **Multi-worker integration test** on the in-process DHT test network: a coordinator plus N workers.
  - **Exactly-once:** no key is fetched by two workers outside a lease expiry, and the funnel reconciles (harvested = Σ terminal outcomes + in flight) after worker kills, network partitions and coordinator restarts.
  - **Fencing:** stale-token outcomes and results are ignored.
  - **Global politeness:** fleet-wide request rates per host stay under each manifest cap.
  - **Placement:** singleton failover happens within one heartbeat timeout; overlay plugins never land on workers without their companion.
  - **Security:** revocation stops a worker within one heartbeat; a worker can't reach anything but the cluster protocol; join tokens are single-use and expire.
  - **Re-verification sampling** catches a deliberately corrupted worker.
  - **Spool replay** is idempotent.
  - **Version skew** is rejected cleanly.
  - **Single-machine parity:** with only the built-in local worker, behaviour and benchmarks match the pre-cluster pipeline.
