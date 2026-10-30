# Persistence acknowledgment and historical import audit

Status: **specified** for DUR-26–27. These are separate follow-up slices; neither
changes the next baseline, performs data repair, or grants updater installation
capability on Windows. Executed checks remain in the baseline ledger.

## Updater acknowledgment semantics (DUR-26)

The prepared ordering fix holds the updater mutex through serialization and
persistence. This prevents an old check from overwriting a newer pause. It does
not make a failed write visible: `save_updater` discards `bin.write` errors,
`Engine::pause` returns `UpdatesStatus`, and the service adapter wraps it in `Ok`.
An acknowledged pause can therefore disappear after restart. Unix execution of
the ordering regression and this proposed error contract are separate gates.

Use one serialized mutation path with a fallible persistence callback. Under the
existing mutex, clone the last durable state, apply the requested mutation, write
the candidate atomically, then publish it as durable and acknowledge success.
Return a typed persistence error if writing, flushing or replacing fails. Do not
map a no-op or zero selected Unix test to successful durable persistence.

For a failed request to pause, additionally inhibit automatic update work in this
process until the owner successfully saves a pause/resume state. This transient
safety hold must not be represented as a durable pause. A failed resume preserves
the existing pause and any hold. No retry may silently acknowledge the original
request: a later successful explicit save establishes a new durable outcome.
An existing unsupported platform continues to return its existing unavailable
capability; it must not acquire an in-memory-only imitation of updater support.

Expose the distinction through a fixed persistence-failure code and a status
indication that automatic updates are held only in this process. Use a safe
localized explanation that the change was not saved and may not survive restart;
never return raw filesystem paths or exception text. This needs an API/schema and
UI contract review before implementation. The About/Updates UI may only display
capabilities actually available in its backend version.

Background check timestamps use the same serialized writer. A failed timestamp
write may leave the fetched check result visible as transient information, but
must not overwrite durable pause state or be described as persisted history.
Keep installation, rollback journal and policy eligibility paths separate from
this mutation contract. Do not expand it into new scheduling or install behavior.

Required Unix fixtures:

- Hold an older save while a newer pause queues; release it and verify ordered
  acknowledgment and the newer pause after restart.
- Inject write, flush and rename failures before acknowledgment. Confirm the old
  durable file survives, the request fails, and no partial file is loaded.
- Fail a pause, confirm the process-only hold and failed acknowledgment; restart
  from the old file and explicitly show that the failed pause was not durable.
- Fail resume, preserve the pause/hold, then allow a successful save and restart.
- Race a background check with failed pause and successful retry; preserve the
  newest durable pause and make all outcomes observable without raw path leaks.
- Assert nonzero selected test counts on supported Unix. Windows unavailable
  tests verify capability reporting, not Unix persistence correctness.

## Read-only historical import audit (DUR-27)

The verified partial-upgrade fix replaces names, name bytes/encoding and archived
metadata for a partial row in the writer transaction. It deliberately does
nothing to rows already marked complete. Old complete imported rows might still
contain an imported display name or lack an archive. Their existence is a reason
to audit, not to rewrite them automatically.

Run only after the collector and crawler stop. Preserve and hash the source
database with its WAL/SHM siblings, recover a separate copy as in PRE-03, close
that recovery connection, then open the copy read-only with query-only enabled.
Record schema version, copy hashes, parser version and integrity outcome. Refuse
unsupported schemas or a failed integrity check. Never open the original through
the application's writable store or run migrations as part of the audit.

Walk imported complete rows with keyset pagination (`id > last_id ORDER BY id`),
using bounded batches and a resumable cursor tied to the immutable fixture hash.
Do not infer completeness from row order, scan only the first 100,000 rows, or use
offset pagination. Join archive presence and stored name/encoding fields without
loading file trees or invoking network fetches. Check cancellation between rows
and retain a partial-coverage status if interrupted.

Classify every examined row exactly once, in this precedence:

1. Invalid or unsupported stored encoding/archive representation: unreadable.
2. No archived info: unavailable for name comparison.
3. Archive exists but lacks required name fields: insufficient evidence.
4. Existing parser's decoded archive name or raw-name encoding differs from the
   stored columns: mismatch candidate.
5. Comparable name fields agree: consistent for the fields examined.

The raw archive intentionally omits `pieces`. It is **not** a complete original
info dictionary and cannot independently re-establish the torrent's infohash.
A mismatch is a candidate for later investigation, not proof that the archive is
the authoritative replacement. Missing archives may be expected when archiving
was disabled; current settings do not prove historical settings. Keep missing,
unreadable and inconsistent evidence distinct.

Default output contains aggregate counts, scanned/eligible totals, cursor coverage
and limitations. An optional private local finding file may retain local row IDs
and fixed reason codes for later review, but no names, infohashes or archive bytes
belong in shared reports. The audit never updates names, clears partial flags,
creates reindex work, fetches metadata or manufactures an archive from an import.
Any repair requires a separate reviewed plan with authoritative verified inputs,
transaction boundaries and rollback evidence.

Required disposable fixtures cover UTF-8 and supported legacy encodings, differing
import/verified names, archive-disabled rows, malformed archives, stripped-piece
archives, complete/partial/unverified duplicates, sparse IDs beyond 100,000 rows,
interrupted pagination/resume, unsupported schemas and recovered WAL data. Assert
the copied database hash is unchanged after the read-only pass. Large fixture
execution waits until the soak has stopped; source preparation is allowed during
collection.
