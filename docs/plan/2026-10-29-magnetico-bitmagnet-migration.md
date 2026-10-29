# Magnetico and Bitmagnet migration into Lodestar

Date: October 29, 2026. Status: **specified; implementation and validation pending**.

Expanded wizard scope and delivery plan:
[DHT crawler import wizards](2026-10-29-dht-import-wizards.md). This document remains
the detailed foundation for the inspected Magnetico/Bitmagnet inputs; the expanded
plan governs the shared wizard and additional source profiles.

## 1. Outcome and scope

Provide one migration workflow that preserves usable catalog data from Magnetico
and Bitmagnet, makes imported records searchable, and durably schedules missing
torrent metadata for recovery. Users can pause, restart and resume without losing
committed work. Duplicate records across all three supplied sources converge on
the same torrent identity and retain their provenance.

Import completion and recovery completion are separate. Unavailable swarms never
prevent an import from finishing. Imported file lists are useful even when the
original info dictionary is unavailable, but they must not be described as
cryptographically verified or exported as a reconstructed original torrent.

This plan authorizes no actual migration, source repair or service startup. While
the CR-0 soak runs, restrict work to designs, source preparation and small script
fixtures. Do not access its database for import, change its executable/configuration,
start Docker, run large scans, extract archives, install dependencies or build.
Recovery scheduling changes retain the baseline-review gate.

The feature includes a UI, shared CLI/service engine, source adapters, durable job
state, deduplication, field mapping, index integration, and background recovery.
It does not introduce arbitrary database connectors, an external enrichment
service, a downloader, or a replacement crawler. Source removal and historical
data repair are separate operations.

## 2. Observed inputs and inspection limits

Only directory metadata, SQLite schema, ZIP directory and a 128 KiB decompressed
SQL prefix were inspected. No torrent titles or hashes belong in this report.
No complete source hash, row count, integrity scan or PostgreSQL connection was
performed. Claimed corpus counts below come from source filenames/README text.

| Supplied location | Observed format | Planned treatment |
| --- | --- | --- |
| `%USERPROFILE%\Megnetico\database.sqlite3` | 107,879,022,592 bytes, approximately 100.47 GiB; SQLite `user_version=3`; `torrents` and `files`; no WAL/SHM/journal siblings at inspection | Direct read-only, indexed, resumable Magnetico adapter |
| Same directory, `20250417_29.0M_magnetico-merge.tar` | 107,879,034,880 bytes; advertised 29M corpus | Do not import both automatically or assume byte equality. Prefer the already extracted SQLite file. TAR extraction is unnecessary for these inputs |
| `%USERPROFILE%\Bitmagnet\postgres-data` | PostgreSQL physical cluster, `PG_VERSION=16`; compose names `postgres:16-alpine`; `postmaster.pid` exists | Detect as a cluster, not an importable SQL file. Obtain a read-only connection or logical export through a separate, version-matched recovery/preparation procedure |
| `%USERPROFILE%\Downloads\2026.01.29.bitmagnet.Database.Dump\backup.zip` | 23,488,442,889-byte ZIP; one unencrypted DEFLATE member `backup.sql`, 103,805,668,425 bytes, approximately 96.68 GiB uncompressed | Stream the member directly through the safe dump adapter; avoid a second full SQL copy |
| ZIP prefix / adjacent README | Prefix says pg_dump/server 14.20, `SET client_encoding = 'SQL_ASCII'`, column-named `INSERT` statements; README says Bitmagnet 0.10.0, approximately 45M torrents, including Magnetico/RARBG/SkyTorrents imports | Support the actual insert dialect and byte encodings; expect overlap. Header/README versions do not prove every table's schema or complete-file validity |

The observed Magnetico schema includes `id`, `info_hash`, `name`, `total_size`,
`discovered_on`, `updated_on`, `n_seeders`, `n_leechers`, `modified_on`; files have
`id`, `torrent_id`, `size`, `path`, `is_readme`, `content`. It includes FTS tables
and indexes. `modified_on` has a far-future default, so it is not a trustworthy
freshness timestamp without interpretation. No explicit private flag or canonical
torrent file-order field was found.

The PostgreSQL cluster's shutdown state, extensions, schema and actual size remain
unknown. A PID file is not proof that a server is running. Never start PostgreSQL
against that original directory, delete its PID file, run migrations, or infer
that PostgreSQL 14 logical-export metadata makes the PostgreSQL 16 cluster usable
with version 14. These are separate sources.

## 3. Reuse and concrete gaps

Implementation anchors were inspected in `work/soak-followups`, whose packaged
source is `744b84d68c7609d8220430ffc9938c05c6d6a527`. Develop on a separate branch
from the reviewed integration stack, preserving unrelated root and Settings work.

| Existing code | Reuse | Required change |
| --- | --- | --- |
| `crates/sources-importers/src/bitmagnet.rs` | Read-only PostgreSQL session, hash keyset pagination, conversion, source registration | Adapter interface; schema capability checks; byte/row bounds; TLS/credential handling; richer mappings; cancellation; source-bound durable checkpoints |
| `crates/lodestar/src/main.rs`, `cli/mod.rs` | Existing `import-bitmagnet` entry point | Shared job engine; secure credential inputs; dry-run/preview/status/resume. Keep a compatible alias with migration guidance |
| `crates/store/src/lib.rs` | Single writer, source merge, prepared records, transactional partial upgrade and reindex work | Explicit import quality and conflict semantics; richer unverified duplicate merge; durable job batch acknowledgment |
| `crates/pipeline/src/enrich.rs` | Local classification and corrected enrichment | Reuse for imported records, preserving user corrections and existing policy enforcement |
| `crates/pipeline/src/lib.rs` / `Store::partial_hashes` | Existing fetch and verified upgrade paths | Replace startup-only first-50,000 selection with durable due-work pagination and maintenance ownership |
| `crates/api/src/jobs.rs`, `dto/jobs.rs` | Authenticated job controls and existing UI-facing conventions | Import-specific durable state. Current registry is in-memory, retains finished jobs for ten minutes, and expires paused jobs; do not inherit those semantics for migrations |
| `web/src/lib/ui`, Settings/Sources navigation, message catalogs | Existing Svelte forms, feedback, dialogs, navigation and localization | Import screens and status details; coordinate with the separate Settings worktree |

Existing Bitmagnet code assumes a name and size, supports only 20-byte identities,
marks imported rows and missing file lists, and never stores the original info
dictionary. A single cursor file is shared across imports and is not tied to the
source database. A batch can retrieve an unbounded number of file rows. Existing
unverified duplicates generally add provenance rather than filling missing fields.
These limitations must be fixed, not hidden behind a new screen.

The reviewed partial-upgrade fix already replaces verified names/encoding/files
and archived metadata transactionally. Preserve it. A complete-looking imported
row still needs an explicit route to a later verified upgrade; do not overload
`FILES_PARTIAL` to mean every kind of uncertainty.

## 4. User workflow

Add **Import library** under Sources, with links from Settings and the command
palette. Use `/imports` and `/imports/{id}` for durable history and details. UI and
CLI operate on the same engine; closing a browser never cancels a job.

1. **Choose source.** Magnetico SQLite, Bitmagnet PostgreSQL connection, or
   Bitmagnet SQL/ZIP. Detect format from content and schema, not extension alone.
   A PostgreSQL data directory receives a clear preparation guide.
2. **Inspect.** Show recognized format/version, fields available, metadata gaps,
   source size, encoding concerns, and whether total counts are estimates. Preview
   is bounded and must not silently scan 45M rows or run exact `COUNT(*)` queries.
3. **Review.** Show duplicate policy, destination/staging volume estimates,
   recoverable fields, unsupported fields, and the recovery/network policy.
   Offer a small pilot before a full import. Default to preserving richer existing
   data and recovering missing public torrent metadata after local import.
4. **Import.** Separate reading, committed records, enrichment and indexing.
   Show new, improved, unchanged, conflicted, rejected and filtered counts. Progress
   uses bytes for streaming input and records for committed catalog work; never
   imply that 50% of SQL bytes means 50% of torrents. Unknown ETA stays unavailable.
5. **Recover.** Show eligible, queued, active, recovered-and-committed, retry-later,
   paused, unavailable and policy-blocked counts. Imported records remain usable.
6. **Finish.** Report import completion and index catch-up independently of recovery.
   Include a paginated private issue report, aggregate export and resume controls.

For very large sources, use a server-side file registered by the local CLI or an
operator-configured import directory. A browser on another PC cannot name a file
on the server with a client-side file picker. Do not require uploading 100 GiB
through the browser. UI receives opaque source IDs and safe display names; raw
paths and credentials are not exposed in shared job responses. Browser uploads,
if later added, require a separate resumable upload/quota design.

Pause ingestion and pause recovery independently. Pause releases source snapshots
and transactions at a checkpoint; committed data remains. Cancel stops future work
and retains imported rows, with explicit wording. Retry/resume continues a durable
job. There is no implicit undo-all button: deleting duplicates could remove data
that predates the migration. Removing imported data needs a separate reviewed
provenance-aware deletion operation.

Reuse the existing form engine, fields, buttons, progress and status primitives.
Provide English/Chinese messages, keyboard operation, focus restoration, narrow
layouts, explicit loading/error/unavailable states, reduced-motion support and
throttled screen-reader announcements. No status conveyed by color alone. Login
expiry hides private details without stopping the server job. Reconnect resyncs
from durable status; stale responses cannot overwrite newer job revisions.

## 5. Adapter and parser contracts

All adapters emit a versioned normalized record/event stream with typed identity,
raw/display text, optional values, source attribution, quality, source cursor and
validation findings. Never construct a fabricated verified `Info` merely to fit
the current store API. Validate integer ranges, hash lengths, paths and all counts.

### Magnetico SQLite

- Support observed schema v3 plus explicitly fixture-tested compatible schemas;
  detect required columns and optional capabilities, not a version integer alone.
- Open read-only with query-only mode, trusted schema disabled, extension loading
  disabled and bounded execution. Read ordinary base tables, not FTS views/triggers.
- Require a stopped consistent snapshot. Do not use `immutable=1` for a live source
  or ignore WAL data. If needed, obtain a consistent backup/copy including WAL
  state after the soak, with its own disk gate. Never repair the original.
- Page torrents by indexed `id > cursor ORDER BY id`; read files with indexed
  `torrent_id` and stable file IDs. Avoid OFFSET and unindexed N+1 scans. Confirm
  query plans on fixtures and the inspected schema before a full import. If an
  index is absent, use a bounded spool rather than creating one on the source.
- File IDs provide stable iteration, not evidence of original bencode order or
  single-file mode. Preserve observed paths and sizes without claiming either.
- Treat malformed text as recoverable bytes when possible; a bad file row does
  not discard the parent torrent. Mark its file list incomplete and report why.

### Bitmagnet PostgreSQL

- Extend the existing adapter for capability-tested schema families, including the
  observed 0.10.0 format, absent `files_count`/`torrent_pieces`, and an explicitly
  tested legacy `single_file` layout. Fail clearly on unsupported identity/schema.
- Use structured connection settings, least-privilege read-only credentials,
  schema-qualified allowlisted queries, statement/lock timeouts and TLS with
  certificate verification for remote connections. Plaintext loopback requires an
  explicit local configuration. Do not accept arbitrary connection `options`,
  SQL, or search paths from a browser request.
- Source must be quiescent for a complete snapshot import. Keep each parent/file
  batch consistent in a read-only transaction, with no long-held transaction while
  paused. A live, changing source may be offered only as explicitly best-effort:
  inserts below the cursor and changes to earlier rows require a reconciliation
  pass; a rolling keyset read is not advertised as a complete snapshot.
- Bound both torrent and file retrieval. Stream a very large file list into a
  per-record spool; never load every file for a 1,000-torrent batch into RAM.
- Use source identity, schema signature and source generation to bind the cursor.
  A resumed job must not silently connect to another database with the same name.

For the supplied physical cluster, the later operator procedure is: establish
that its owner is stopped, preserve a consistent copy/snapshot and required WAL,
inspect version/extensions, recover only the copy with a matching PostgreSQL 16
environment, then expose a read-only connection or produce a logical export.
Do not start its crawler. Different-machine/platform physical files may need the
original compatible environment; the UI must explain this limitation. No automatic
Docker start, restore, upgrade, repair or source cleanup is part of the importer.

### Bitmagnet SQL and ZIP

- Implement a streaming parser for the supported PostgreSQL data-dump subset:
  column-named multi-row INSERT, literals/NULL, bytea forms, quoted identifiers,
  supported literal casts, and COPY text from stdin. Include safe pg_dump session
  statements and optional `ON CONFLICT DO NOTHING` as recognized syntax only.
- Do not execute SQL, shell commands, psql commands, functions, DDL, extensions,
  `COPY FROM PROGRAM`, network fetches or arbitrary expressions. Parse known data
  tables into typed records. Recognized non-data setup is ignored; unknown executable
  syntax stops with a resumable unsupported-format error, never a fallback restore.
- Correctly tokenize quotes, escaped quotes/backslashes, E-strings, multiline text,
  comments, bytea hex/octal, NULL versus empty text, COPY escapes and terminators.
  Do not parse this format by splitting lines, semicolons or commas.
- Respect declared encoding. `SQL_ASCII` means encoding is unknown, not UTF-8.
  Preserve bounded raw bytes and encoding provenance; decode per existing text
  policy or an explicit source override. Never silently replace undecodable bytes
  and subsequently claim verified names. The prefix happened to decode as UTF-8;
  that establishes nothing about the remainder.
- Stream ZIP members without filesystem extraction. Validate names, entry count,
  advertised/actual expanded bytes, compression method, encryption and CRC. Reject
  traversal, symlink entries, nested archives and unsupported multiple SQL members.
  Do not concatenate members. Limits must accommodate this legitimate 96.68 GiB
  member while remaining finite and recorded; a tiny universal limit is unsuitable.
- Relational tables can precede their torrent rows. Use an indexed, quota-bounded
  normalized disk spool for supported relations, and keyset joins. Do not retain
  global hash/file maps in memory or assume dump order. Discard indexes/derived
  search vectors rather than rebuilding Bitmagnet internally.
- Track stream bytes separately from normalized records. At EOF, verify CRC,
  parse termination and unresolved references before declaring source extraction
  complete. Truncation/corruption yields partial results with explicit status.
  Already committed records remain labelled as belonging to an incomplete import.

Plain SQL resumes at a validated statement/tuple boundary. For the monolithic
DEFLATE ZIP, a byte offset alone cannot restore decoder state. Initial release
may re-decompress from the start and skip committed logical records on restart;
show this as replay, not new progress. Pausing without process exit can retain
decoder state. An optional checkpointed decoder is a later optimization requiring
correctness and resource tests, not a promised instant resume.

Custom/directory pg_dump archives and TAR-only SQLite sources receive clear
conversion guidance initially. The supplied ZIP and extracted SQLite are directly
supported; arbitrary PostgreSQL binary archives are not silently treated as SQL.

## 6. Field preservation, quality and deduplication

| Source data | Destination behavior |
| --- | --- |
| Valid infohash | Primary deduplication key; exact full identity, never name/size. Validate supported v1/v2 forms; never truncate a 32-byte hash into v1 |
| Name, total size, file paths/sizes | Preserve usable values and raw text provenance; validate checked sums, path components and negative/overflow values; make missing/partial state visible |
| File count / files_status / pieces metadata | Preserve reported and observed counts separately. Validate indices/count/size consistency; missing or contradictory data remains partial/unknown |
| Private flag | Preserve declared private/public/unknown. Private or restricted-source entries never enter public recovery; importing a public-looking duplicate cannot clear a restriction by itself |
| Discovery/publication/update times | Store original meaning and import time separately. Keep unknown/sentinel dates unknown. Historical import is not a new DHT discovery or a fresh sighting |
| Seeders/leechers | Preserve only as timestamped historical observations; never show as current health. Missing timestamp means age unknown |
| Bitmagnet upstream source links | Keep source labels/attribution in import provenance. A claim of RARBG/Magnetico origin is not an independently observed Lodestar discovery |
| Content mappings, external IDs, categories, tags, language/episode/release attributes | Store supported values as attributed imported enrichment, below user corrections and applicable verified/local authority. Do not replace raw torrent name with a movie title |
| Descriptions, collections, Magnetico readme text | Preserve supported bounded annotations as inert text in a versioned import-evidence store. No HTML/script execution, external-image loading or automatic URL fetching. Oversized/unsupported values get explicit findings |
| Source FTS/tsvector, bloom filters, work queues, generated columns, app settings, credentials | Do not import operational state or derived indexes. Rebuild Lodestar search/enrichment. Report skipped table/field categories |
| Complete original info dictionary, if actually available | Verify hash before promoting canonical metadata. Reconstructed dictionaries, piece counts and stripped archives are not sufficient |

Do not spend a new bit in Lodestar's 64-source bitset for every import job or
upstream label. Register the stable adapter sources (`bitmagnet`, `magnetico`),
and keep job/dataset/upstream attribution in dedicated bounded-on-disk tables.
Enforce table/field/annotation quotas and aggregate overflow counts; do not invent
unbounded metric labels or preserve arbitrary source databases as opaque blobs.

Persist three independent concepts: file-list completeness (unknown/partial/
reported-complete), verification (unverified/verified/conflicting), and recovery
eligibility/reasons. Missing raw archive or piece hashes alone does not enqueue
every otherwise usable imported torrent for a network fetch. Default repair covers
missing name/size, missing/inconsistent files and required info fields; optional
verify-all is a separate explicit operation with a cost estimate.

Valid hashes without sufficient fields for the current non-null canonical schema
go into durable `import_candidates`, visible in job details and eligible for repair.
Do not invent an empty name or zero size to force them into normal search. A
successful verified fetch promotes a candidate transactionally. Invalid identities
are rejected with a fixed reason; valid parents with malformed children survive.

Proposed additive schema includes `import_jobs`, `import_datasets`, compact
`import_membership`, `import_candidates`, imported quality/provenance/annotations,
and shared metadata-recovery eligibility. Final table names follow store conventions.
All are migration-versioned, indexed and included in backup/restore and disk
estimates. Do not infer legacy verification from archive presence; conservatively
mark older imported rows unknown through a bounded migration, without historical
repair or silently scheduling all legacy imports.

Merge precedence is deterministic: user corrections remain overlays; hash-verified
metadata controls canonical info fields; consistent richer imported data can fill
missing unverified fields; equal-quality conflicting data keeps the existing value
and records a bounded conflict. Never union incompatible file lists into a supposedly
complete torrent. A richer duplicate must be able to upgrade a previous partial
import even without raw metadata, while retaining unverified status. An import can
never downgrade a verified row, resurrect a tombstone or bypass quarantine/filter
rules. Duplicate ingestion adds provenance without double-counting stored torrents.

Verified upgrade replaces canonical name/raw encoding/files/info fields, reapplies
classification and corrections, observes archive settings, clears only resolved
repair reasons and writes reindex work in one transaction. A mismatched hash is a
failed recovery; it cannot overwrite the imported record. Metadata verification is
not proof of payload safety or current swarm availability.

## 7. Durable jobs, ownership and accounting

Use one ingestion worker initially, with separate bounded source parsing,
preparation and writer messages. Online imports pass through the existing writer
and filter pipeline; they do not open a competing application writer. The offline
CLI acquires the existing exclusive data-directory ownership and uses the same
commit contracts. Refuse offline mode while `serve` owns the directory. Legacy
cursor migration requires explicit source binding; never automatically trust the
old unqualified `import-bitmagnet.cursor` for a different source.

Durable job state records adapter/schema versions, source identity, source cursor,
phase, user controls, options, input/output byte counts, aggregate row outcomes,
staging quota, durable index watermark and last error code. Credentials are sealed
references, never embedded in cursor JSON, arguments, logs or metrics. Missing
credentials after restart yields `needs_credentials`, not a silent restart from zero.

Import phases: inspecting, ready, reading, committing, indexing, complete or
complete-with-issues. Paused, blocked, cancelled and failed are explicit controls/
outcomes with a resume checkpoint. Recovery has its own state and may outlive all
ingestion phases. Imported job history must survive the current registry's ten-minute
retention and service restart; paginated persisted history feeds the common Jobs UI.

For each destination batch, commit records/provenance/quality, repair eligibility,
reindex entries, outcome counters and destination checkpoint in the same SQLite
transaction. Acknowledge only after commit. A crash after commit but before ACK
replays idempotently; counters cannot double-increment. Counters obey a documented
conservation equation for processed parent records and distinguish file/annotation
row outcomes. Retried source batches and replayed compressed bytes are separate.

For a separate staging database, its records and parser cursor commit together
there; destination consumption and the destination cursor commit together in the
main store. These are replayable steps, not a fictitious atomic transaction across
two WAL databases. Never delete staging data until destination commit is confirmed.

Pin source handles where possible and record file identity, size, timestamps,
schema signature and chunk digests as bytes are consumed. Preview fingerprints
are provisional, not full-content hashes. Resume validates the persisted identity
and processed ranges; changed/replaced input stops with `source_changed`. Full
digests can be accumulated during the actual scan, avoiding an extra 100 GiB pass.
SQLite/postgres snapshot identity needs its own dataset identifier; same filename
or database name is insufficient. Credentials changes alone do not change identity.

Shutdown checkpoints within bounded batches, releases readers and drains committed
writer ownership. A user-paused job stays paused after restart. Previously running
jobs can resume under persisted permission and resource policy, but must revalidate
source, credentials and space first; failure blocks the job with an actionable reason.

## 8. Background metadata recovery

Implement the existing CR-3 durable partial-upgrade design, extending eligibility
to import candidates and missing fields without conflating completeness and trust.
The integration branch's `2026-10-27-durable-partial-upgrades.md` and persistence
audit remain inputs; this feature must not create a competing recovery scheduler.

- Insert durable eligibility with the imported row/candidate, but hold bulk recovery
  until local source extraction/merging finishes by default. This avoids fetching
  metadata that a later source table or duplicate already supplies. Users may
  explicitly enable overlap after preview; foreground detail recovery remains bounded.
- Keep eligibility on disk; read due work in small indexed pages ordered by due
  time and stable identity. Never load millions of hashes or revisit only the first
  50,000. Work remains eligible through pool eviction and process restart.
- Carry maintenance origin, source/privacy policy and a fenced generation through
  lookup, fetch, verification, writer and reindex. Rejected admission does not consume
  a network attempt; writer commit, not fetch completion, marks recovery successful.
- Use the existing TCP/MSE fetcher, DHT peer finders, public-endpoint checks, pause,
  consent, cancellation, socket/queue caps and filtering. No content downloads,
  imported peer-address trust, external providers or new transport are required.
- Imported unknown privacy is not automatically public. Review offers an explicit
  public-dataset recovery choice for Magnetico; otherwise unknown/private work stays
  local and pending. Declared private torrents remain excluded regardless of that
  choice. Private-source restrictions survive duplicates and enrichment updates.
- Use a small work-conserving maintenance share that cannot starve discovery or
  interactive work. Select and record numeric shares/backoff from the completed
  baseline and CR-2 ownership contract before implementation acceptance; do not
  raise global limits or choose arbitrary timeout changes in this feature.
- Retry genuine transient/no-peer failures with bounded exponential delay and
  jitter, retaining next due time and outcome. After a finite policy budget, mark
  `unavailable_for_now`, retain the record and allow manual retry or a qualifying
  fresh discovery to re-enable it. Admission rejection/paused state is a defer,
  not a failure. Repeated rejection must not become a busy loop.
- Dedupe with a hash already in the pool, attach the recovery obligation, and avoid
  resetting age/attempts or inventing discovery/cohort counts. Deletion, quarantine,
  consent withdrawal and stale generations fence late responses before commit.

Default recovery cannot promise all missing metadata: peers may no longer exist,
private swarms may be inaccessible, and some imported claims may be wrong. Useful
imported data remains retained and searchable even after the recovery budget ends.

## 9. Resource, security and storage gates

Proposed initial importer limits, to validate on isolated fixtures: one source
reader/prepare worker; at most 500 parent records or 8 MiB per writer batch,
whichever comes first; 64 MiB queued normalized data; 256 MiB incremental importer
memory target. Existing metainfo/text/file safety limits remain authoritative;
over-limit records are streamed/spooled or retained as partial with an explicit
finding, not silently truncated into complete records. These limits never change
the live soak settings. Long rows/statements must be parsed incrementally so a
legitimate 1,000-row SQL INSERT does not require a giant allocation.

Before a full job, run an isolated pilot after the soak and estimate destination
DB, files, provenance, candidate/recovery rows, index, WAL and staging growth.
Show an uncertainty range, source-estimated totals and a reserve. A compressed
input size is not a destination-space estimate. The supplied ZIP would consume
another 96.68 GiB if naively extracted; do not assume C: has room for extraction,
the destination and the index. Allow staging/destination on an explicitly chosen
volume. If the estimate is too uncertain or reserve insufficient, require a smaller
pilot or a larger volume before accepting the full job.

Monitor all used volumes, spool/WAL/index growth and job-owned logs. Stop reading
before the reserve is exhausted, persist a blocked checkpoint and let outstanding
bounded commits finish where possible. Disk-full during persistence is a failure
with uncertain uncommitted work, never a successful pause acknowledgment. Throttle
ingestion to writer/index lag. Keep source and destination rate controls separate
from crawler intensity. No automatic source/evidence deletion, vacuum or full
checkpoint under load. Any staging cleanup verifies exact job-owned paths and
retention state; never recursively delete user-supplied directories.

Protected API routes use existing authentication, same-origin JSON/CSRF defenses,
permission checks and revisions. Source registration requires operator authority;
reject arbitrary filesystem traversal, device/pipe files, symlink/reparse escapes,
unapproved UNC paths, and arbitrary network destinations. Use configured import
roots or CLI-issued source grants. PostgreSQL connection tests are explicit,
bounded operator actions, never general URL-fetch endpoints. Secrets use the
existing sealing facilities and are redacted from every error/report.

Apply mandatory content/hash filters, private visibility and quarantine before
search visibility and before network repair, including duplicate and offline paths.
Store names/descriptions as untrusted text. Logs/Prometheus labels contain bounded
reason enums and aggregate counts; names, hashes, paths, passwords and source
records appear only in access-controlled, paginated local inspection when needed.

## 10. Implementation slices and acceptance

Each slice records specified / source prepared / checks passed / accepted separately.

| Slice | Deliverable | Acceptance gate |
| --- | --- | --- |
| IMP-01 | Capability schemas, normalized events, field/quality/privacy/merge contracts, synthetic source fixtures | Explicitly covers observed inputs and all unknown/unsupported cases; schema review |
| IMP-02 | Durable jobs, source identity, checkpoints, counters, writer ownership, migrations | Crash/replay/pause/cancel tests; restart persistence; no duplicate ACK/counts or second writer |
| IMP-03 | Magnetico adapter and secure local-source registration | v3/legacy fixtures; bytes/paths/large-file bounds; source unchanged; keyset plans verified |
| IMP-04 | Refactored Bitmagnet PostgreSQL adapter and compatible CLI alias | Actual PostgreSQL fixture tests selected and executed; TLS/read-only/schema/reconnect cases; no silent integration-test skip |
| IMP-05 | Safe SQL/ZIP parser, relational staging, replay and byte accounting | INSERT/COPY/SQL_ASCII/ZIP64/CRC/truncation/malicious-syntax tests; no execution or full extraction |
| IMP-06 | Rich import merge, annotations, local enrichment, indexing, policy gates | Cross-source duplicate orders, conflicts, richer partials, user corrections, deletion/private/filter races; truthful search quality |
| IMP-07 | Durable missing-metadata recovery integrated with CR-3 | Baseline review and stable fairness/ownership contracts; >100k candidates, unavailable early rows, cancellation, stale outcomes and commit failure tests |
| IMP-08 | Import UI, durable history, CLI status/control, API/OpenAPI/messages | Same engine/semantics in both clients; reconnect/auth/revision tests; keyboard/accessibility and supported browsers |
| IMP-09 | Scale pilots, packaging/docs, operational recovery | Windows/Linux supported paths, import + index + shutdown/restart evidence; bounded memory/disk; representative corpus trial |

Build shared contracts and durability first, then adapters and merge behavior;
add the UI over that engine. IMP-07 can be specified/prepared while adapters are
built but is required for the promised automatic-recovery feature. An import-only
preview release must explicitly say recovery is unavailable, not report this plan
complete. Source preparation can proceed during the soak; execution of builds,
database-backed tests, large parser inputs and browser suites waits until it stops.

## 11. Validation matrix

- **Source preservation:** read-only SQLite/WAL snapshots, PostgreSQL permission
  rejection on writes, no source schema/index changes, source replacement mid-job,
  missing file, locked file, permissions, unsupported version and no original repair.
- **Data fidelity:** v1/v2 identity handling; hash-only records; null/zero/negative/
  overflow values; raw and legacy encodings; SQL_ASCII; Unicode/bidi/NUL; duplicate
  or missing file indices; path separators/traversal; count/size contradictions;
  single-file versus one-entry multi-file ambiguity; historical timestamps and stats.
- **Dump grammar:** multi-row and multiline INSERT, escaped quotes/backslashes,
  bytea, NULL, JSON, casts, comments, COPY; unknown columns/tables; unsupported
  executable SQL; ZIP64, extra members, CRC mismatch, truncation, decompression
  limits, malicious entry names and EOF in every parser state. Prepare fuzz seeds;
  fuzz execution waits for the appropriate environment.
- **Merge/policy:** all three source orders and repeat runs; no downgrade of verified
  data; richer imported duplicates; complete-looking but unverified rows; conflicts;
  private/public/unknown provenance; tombstones, filter denial and quarantine;
  independent discovery counts; source-bit exhaustion avoided.
- **Durability:** crashes before/after source checkpoint, staging commit, destination
  commit and ACK; concurrent control requests; disk/rename/WAL failures; index lag;
  credentials loss; paused restart; cancel with records already committed; compressed
  replay without duplicate counts. Recovery and import checkpoints remain separate.
- **Recovery:** 100,001+ partial/candidate rows with unavailable early ranges;
  sustained new discovery; pool/channel saturation; eviction before admission;
  timeout/backoff and expiry; unknown/private exclusion; verified mismatch; late
  writer/deletion races; >50k work eventually served; ownership drains on shutdown.
- **UX/security:** operator versus unauthorized access, CSRF, filesystem grants,
  credential redaction, SSRF controls, untrusted text, no SQL execution, preview
  sample bias, stale revisions, disconnected tabs, null ETA and index/recovery
  completion distinction. Test English/Chinese, focus, keyboard and narrow layouts.
- **Scale after the soak:** disposable pilots progressing from 10k to 100k to 1M
  normalized records, skewed huge file lists, then a sufficiently provisioned full
  source trial. Record elapsed time, peak/incremental memory, disk/WAL/staging/index
  growth and foreground latency. Set performance acceptance thresholds from those
  measurements; do not derive import speed or recovery ETA from the crawler's
  approximately 720 newly stored torrents/hour.

Use targeted deterministic tests first, then applicable Rust formatting/Clippy,
workspace/property tests, frontend checks/build/budgets/types/browser coverage,
documentation/template checks and fresh correctness/security review. PostgreSQL
tests currently skip when their environment variable is absent: record that as
not executed, never a pass. Linux/Unix and visual limitations stay explicit.

The feature is accepted only when the supported supplied formats can be imported
without source mutation, paused/resumed across restart, deduplicated and indexed
with truthful quality, and missing public metadata receives durable fair recovery.
Full restoration of every unavailable torrent is not an acceptance requirement.

## 12. First execution against these inputs

After the soak and feature gates, provision a separate destination and staging
volume, retaining all original sources. First use a small Magnetico SQLite pilot
to verify schema/text/files and destination growth; then a representative Bitmagnet
dump fixture/pilot to validate encoding and field mappings. A small prefix of this
dump contains enrichment tables, so it is not a representative torrent sample.

For the full migration, import the newer Bitmagnet dump as the likely broadest
catalog first, then the Magnetico SQLite corpus to add unique/richer records, and
the local Bitmagnet database after its compatible read-only preparation. This is
a provisional ordering based on source descriptions, not an assumption that one
contains another. The merge result must be safe under any order. Prefer completing
all local merges before enabling bulk recovery, and estimate overlap from bounded
samples without presenting it as an exact count.

No current disk-space sufficiency, full source integrity, complete schema coverage,
import duration or recovery success rate has been established. Those are measured
pilot/acceptance outcomes, not facts inferred from this planning inspection.

## References

- [Bitmagnet backup/restore/export guide](https://bitmagnet.io/guides/backup-restore-merge.html): its data-only, column-insert format explains the required dump adapter; direct import avoids recreating a full Bitmagnet service.
- [Bitmagnet v0.10.0 schema](https://github.com/bitmagnet-io/bitmagnet/blob/v0.10.0/migrations/00001_init.sql), [file status migration](https://github.com/bitmagnet-io/bitmagnet/blob/v0.10.0/migrations/00002_files_status.sql), [pieces migration](https://github.com/bitmagnet-io/bitmagnet/blob/v0.10.0/migrations/00013_torrent_pieces.sql), [file count migration](https://github.com/bitmagnet-io/bitmagnet/blob/v0.10.0/migrations/00016_files.sql): source contracts, not proof of every supplied database's contents.
- [PostgreSQL SQL dumps](https://www.postgresql.org/docs/18/backup-dump.html) and [pg_restore](https://www.postgresql.org/docs/18/app-pgrestore.html): logical/physical format distinctions and why arbitrary restoration must not be an automatic import fallback.
- [SQLite URI semantics](https://www.sqlite.org/uri.html): read-only and immutable connection distinctions.
- [Lodestar architecture](../architecture.md): existing importer, writer, source, privacy, indexing and UI contracts.
- Integration documents: `2026-10-27-durable-partial-upgrades.md`, `2026-10-27-persistence-and-import-audit.md`, and `2026-10-26-soak-import-upgrade.md` on `work/soak-followups`.
