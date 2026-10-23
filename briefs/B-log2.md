# B-log2: Logs API: ring buffer, flood folding, live stream, history, files
Covers the in-memory ring buffer (purged when a kind or opt-in turns off), flood folding in every output, `GET /logs/stream` (its own cursor, per-client filters, its own caps; not the `/events` hub), `GET /logs` history over the ring and the files, and the log files: list, download (redacted or not, JSONL or text, one file or a zip), and delete.

**Model:** Opus. This is privacy- and security-critical work:
- it decides what a signed-in browser can read back out of the logs, which can hold torrent details, peer activity and filter-change details;
- file download and delete touch the filesystem, with names that come from untrusted places;
- the ring sits on the logging hot path and is shared by every thread and every stream.

**Branch:** `slice/B-log2`. **`/security-review` is required** before merge (log access exposes data; file download and delete touch the filesystem; standards §5.7). **Follow `.claude/skills/steward/SKILL.md` from your first push until the PR merges.**

**Spec:**
- `runner/standards.md`.
- **Architecture §12.1, all of it.** Read the whole section, not just "API". The contract for this slice is in:
  - "In-memory ring buffer" (5,000 records, at most 4 MiB);
  - "Floods" (same target, level and message; "repeated 1,204 times in 10 s"; in every output);
  - "Turning an identifying kind or an opt-in off … also purges or redacts the records in the ring buffer";
  - the opt-ins' "Where they go" (file and live log only) and "Files that contain them" (badge; `redact=true` strips them);
  - the canary (outputs include "ring buffer, live stream or download");
  - "API": `GET /logs`, `GET /logs/stream` (the B4 comparison, own cursor, `Last-Event-ID`, per-client filters, admin guard, heartbeats, ending with the session, closing on shutdown, **2 streams per session and 8 in all**, **a 256-record queue per client** and one `resync`), `GET /logs/catalog`, `GET /logs/files`, `GET /logs/files/{name}?format=text|jsonl&redact=true|false`, `GET /logs/archive.zip`, `DELETE /logs/files`.
- Architecture §10.10.12: the Log files list (badge, Download with "Remove torrent details" on by default, Download all (zip), folder path with Copy), the Danger zone (delete all or only those with details, with count and size), and the live log (it opens on the ring's lines, sends its filters with the stream request, catches up from the cursor, "Load earlier" pages back through `GET /logs`).
- Architecture §10.10.7: deleting log files is **Destructive** (a dialog with count and size, **no password**). Turning on a kind or an opt-in is **Privacy** (dialog and duration, no password) and stays `PATCH /settings/logging` (B-log1). **Nothing in this slice takes the fresh password.**
- Architecture §16: the **B4 decisions** (ids `<epoch>-<seq>`, replay, `resync` reasons, caps and 429 `too_many_streams` with `Retry-After`, transport headers, "A stream ends with its session", shutdown), the **"Logging plan"** entry and the **"Review fixes (runner review of PR #86)"** live-log and canary bullets.
- `docs/owner-decisions.md` §2.11 (users control what's logged; opt-ins only in the file and the live log; secrets and sealed names never), §1.9.7 ("no profiles of people"), §1.8.8 and §1.8.9 (no audit log), §2.19.
- **`docs/decisions/2026-10-23-B-log1.md`, all of it.** In particular:
  - "Typed events bypass `tracing`", "Peer activity is a rendering rule", "URLs", "Error";
  - "The file" (its line shape, start-up buffer, rotation, `index.json`, permissions);
  - "Narrow canary limits", and "Private holds for typed events now, and for every line after B-log1b";
  - the "For B-log1b" lists: high-volume events stay `default_on: false` "until B-log2's flood folding".
- **`docs/decisions/2026-10-23-B12.md`:** "Every bad secret answers the same", "A session (cookie or proxy header) is checked first", "For the logging slice", "Never logged or echoed". Keys and feed tokens can't read logs (§12.1). Every route here is session-only, through `auth::protected`.
- `runner/slices.yaml` B-log2 (title and owns) and B-log1b (it also owns `crates/lodestar/src/logging/**`, so the runner never runs the two at once).
- **The code:**
  - `crates/lodestar/src/logging/mod.rs`:
    - `Runtime` (`apply`, `open_file`, `write_file`, `record_untyped`, `note`, the `Sink` impl);
    - `Inside`, `NOTING`, `IN_SINK`, `Switched`, `flags_of`, `CATALOGUES`, `install` and `runtime()`;
  - `logging/state.rs`: `Applied` (`file_mask`, `kinds`, `status()`), whose live column equals the file's;
  - `logging/file.rs`: `CURRENT`, `INDEX`, `Flags`, `Index`, `FileSink::{write, rotate, rotated_name}`, the `log-rotate` worker (`Job::Compress`, `Job::Prune`), `rotated()`, `prune`, `private()` (`O_NOFOLLOW`, 0600) and `folder()`;
  - `logging/format.rs`: `event_json`, `untyped_json`, `event_text`, `untyped_text`, `quote`, `one_line`, `json_line`, `Stamp::compact`;
  - `crates/log/src/`: `Record`, `Value` (`Secret` renders as a `label`), `Value::render`, `Mask` (`PRIVATE`, `has_details`), `Kind` (`identifying`, `opt_in`), `Output::Live`, `note_written`, `find`, `status`, `url::scrub_text`;
  - `crates/lodestar/src/settings/log.rs` `follow_with` (it calls `Runtime::apply` at each save and each deadline, and reports the `Switched` kinds);
  - `crates/api/src/logs.rs` (`routes`, `current`, `catalog`) and `dto/logs.rs`;
  - `crates/api/src/events.rs`: B4's hub, for its patterns: `EventsConfig`, `Slot`, `Refused`, the `stream` task, `Frames`, `frame`, `session_of` and the heartbeat's `auth.verify(&cookie)`;
  - `crates/api/src/system.rs`: `SystemApi` as an `Extension`, `type Handle`, `api()` → `503`. That's the pattern to copy;
  - `crates/api/src/export.rs` `respond`: the download headers;
  - `crates/api/src/auth/http.rs`: `protected`, `logged_in`;
  - `crates/api/src/auth.rs`: `Auth::verify`, `Auth::proxy_signs_in`, `cookie_value`, `COOKIE`;
  - `crates/api/src/extract.rs`: `same_origin_json`, `peer_ip`;
  - `crates/lodestar/src/services.rs` `Services::run` (the `.layer(Extension(events.clone()))` stack and the shutdown `signal` closure);
  - `crates/lodestar/tests/log_canary.rs` (`Server`, `exercise`, `absent`, the four runs).

## Own
- **`crates/lodestar/src/logging/**`**. New modules as you see fit (for example `ring.rs`, `fold.rs`, `redact.rs`, `files.rs`, `api.rs` for the `LogSource` impl, `zip.rs`), plus changes to `mod.rs`, `file.rs`, `format.rs`, `state.rs` and `events.rs`.
- **`crates/api/src/logs.rs` and `crates/api/src/logs/**`.** slices.yaml lists only `logs.rs`; submodules such as `logs/stream.rs` are part of it. Also `crates/api/src/dto/logs.rs`.
  - The `logs` tag's description in `crates/api/src/openapi.rs` may change; that's one line. `lib.rs`, `openapi.rs` and `dto/mod.rs` already register `logs`, so nothing else is needed there.
- **Tests and generated files:**
  - `crates/api/tests/logs.rs` (new);
  - `tests/openapi.rs`: `ROUTES` entries, plus a `{name}` substitution (`lodestar.jsonl`) beside `{hash}`/`{id}`/`{section}` in its route walks;
  - the 401 list in `tests/api.rs`;
  - the regenerated `web/src/lib/api/openapi.json` + `schema.d.ts`;
  - `crates/lodestar/tests/log_canary.rs`;
  - `docs/decisions/<date>-B-log2.md`.
- **`crates/lodestar/src/services.rs`: these lines only, in `Services::run`:**
  - `.layer(Extension(logs.clone()))` beside `Extension(events.clone())`;
  - `logs.close()` in the shutdown `signal` closure beside `hub.close(); jobs.close();`;
  - the one `let logs = crate::logging::source();` (or similar) these need.
- **`crates/log/src/**`: small additive changes only, if you need them** (for example making `torrent_kind` public). Nobody owns it now. List each change in the PR.
- **No new dependency.** `flate2` (already in `crates/lodestar`) covers gzip, deflate and CRC-32 for the zip. `tokio` is already in both crates.
- **Don't edit** (other slices own these, or open PRs touch them):
  - `crates/api/src/events.rs` (B4's hub; see Build 4's note for its proxy-session gap);
  - `crates/api/src/auth.rs` and `auth/**` (B-log1b);
  - `crates/lodestar/src/settings/**`, including `settings/log.rs`, `settings/logging.rs` and the goldens. Re-run them; regenerate a golden only if your `status()` change moves one, and say so;
  - `crates/lodestar/src/main.rs` (B-update owns it);
  - `crates/api/src/lib.rs` and `ApiState` (#48 changes `ApiState` and every test literal: that's why the source is an `Extension`);
  - `crates/lodestar/src/metrics.rs` and `config.rs` (#48);
  - `docs/user/**` and `docs/operations.md` (#91). Put the user-facing steps in the PR.
- **Nearby open PRs:**
  - **#123 B11b** (`services.rs`, around the first `Store::open`);
  - **#48 M4** (`services.rs`, `api/src/lib.rs`, `tests/api.rs`, `tests/openapi.rs`, `openapi.json`);
  - **B-update's coming PR** (`main.rs`, and one `Services::probe()` accessor in `services.rs`);
  - **#92 P-release** and **#91 DOC-install** (packaging and docs; no overlap, but #91 is where the user steps land);
  - **#103 W9** (`web/` only).

  Keep your shared lines additive, and merge `main` as each lands. **B-log1b** (pending) owns `crates/lodestar/src/logging/**` too; the runner serialises the two. If B-log1b merges first, rebase your plan on its events (`api.request`, `auth.*`, `auth.setup_token` emitted through the catalogue) and its bench.

## Build
1. **One sequence for every line.** Each record the runtime writes (typed, untyped, start-up buffered, fold summaries) gets an id `<epoch>-<seq>`.
   - The epoch is 16 hex digits from the OS RNG, made at `install`. The sequence is monotonic, handed out under the ring's lock as the record goes in.
   - **Add `"id"` to each JSONL line** (additive; `format.rs`). Lines from older builds have none. This lets history join the ring to the files without gaps or duplicates (step 5), and lets a stream cursor be the same id.
   - Record the format change in the decisions file.
2. **The ring buffer** (`logging/ring.rs`).
   - **Bounds:** at most **5,000 records** and at most **4 MiB accounted**: each entry's string bytes plus a fixed per-entry overhead (record the formula). The oldest go first when either bound is reached.
     - One entry is accounted at most 16 KiB. Longer values are cut with `…` and the entry is marked `truncated`. This applies in the ring, stream and history only; the file keeps the whole line.
   - **What it holds:** every record that passed its event's switch, or the `tracing` filter for untyped lines, whatever the file's level and whether file logging is on. It holds each record **rendered once under the live mask** (equal to the file mask: `KindStatus.outputs[2]`) as structured fields `(name, kind, shown value)`. It also holds a `shown: Mask` of the kinds that rendered as values, with a peer address beside a torrent counting as `PeerActivity`, and the event's `opt_in`.
     - **No raw value is ever stored.** So nothing hidden can be recovered or probed by a filter later.
     - Render the file line from the same structured entry, so a record is formatted once for both.
     - Call `note_written(Output::Live, …)` so the volume estimates see the live output.
   - **The setup token never enters the ring.** `auth.setup_token`'s token value (and, until B-log1b converts it, the plain `services.rs` line's token after `setup token `) is stored as `[redacted]`. Only a signed-in admin can read the ring, so setup is already done.
   - **Purge when a kind or opt-in turns off.** In `Runtime::apply`, take the kinds that turned off (the `Switched` list with `on: false`, which covers saves and deadline expiry through `follow_with`). Then, under the ring's lock:
     - set the ring's `allowed` mask first;
     - **remove** every entry of an opt-in event whose opt-in turned off (`filters.change_detail`, and any peer-activity event);
     - **rewrite** every other entry whose `shown` intersects the turned-off kinds, so those fields read `[hidden]`. A hash may read as `ref:` under this run's key if it parses as 40 hex digits. The values leave memory at once.
     - **A record rendered under the old mask that arrives after the purge** (another thread read `applied` first) is checked against `allowed` on push, and purged the same way. No window lets an off kind back in.
     - Streams get a `purge` frame (step 4).
   - **Read-time deadlines:** every API read also treats a kind as off once its `until` has passed, even in the ≤ 50 ms before `follow_with` re-applies.
   - **Cost:**
     - Pushing is O(entry) under a mutex held only for the insert, the eviction and the `allowed` check: no I/O, no formatting, no client work.
     - Wake streams (a `tokio::sync::Notify` or equivalent, callable from any thread) only when at least one stream is open (an atomic count).
     - With no listener, the ring costs one entry build and one short lock per record, inside its fixed memory bound.
     - **Logging never waits on a stream, a socket or a reader.** Readers clone `Arc`s out under the lock (at most a few hundred at a time) and filter outside it.
3. **Flood folding** (`logging/fold.rs`), before every output (console, file, ring).
   - **Identity:** the line exactly as that output shows it, without `ts` and `id`. That's the event id (or `tracing` target), level, message and every field as rendered under that output's mask. It's held as a 64-bit keyed hash (`RandomState`), never the text.
     - Folding per output's own rendering means a viewer learns nothing about values hidden from that output. For example, two searches with queries off fold on a Private console, and stay separate in a file that shows queries.
     - The file and the ring share one table, since their masks are equal.
   - **Window: 10 s from the first occurrence.**
     - The first occurrence is always written in full, at once, in every output.
     - Identical lines inside the window are counted and not written.
     - When the window ends with a count > 0, **one summary** goes out within 1 s: the source's level, component and event id (or target) and message, no fields, plus `"repeated": {"count": N, "seconds": S, "first": "<id of the first line>"}`. In text it's `… <msg> (repeated 1,204 times in 10 s)`.
     - The next identical line after the window starts a new window. A sustained flood therefore costs at most two lines per key per window.
     - The summary carries no data-kind field, so it never shows more than the first line did. Filters, levels and components apply to it as to its source.
   - **Never folded:** required events, the runtime's own notes (`NOTING`), and summaries.
   - **Error safety:** an error's first instance is never folded, and its summary is an error too. Event counters still count every occurrence (the rates and estimates are unchanged).
   - **Cap: 1,024 keys per table.** Keys whose window ended are flushed first. If the table is still full, new lines pass **unfolded**, never dropped.
   - **Flushing:** a timer flushes due summaries (it can't be the minute-long `follow` loop). Pending summaries are written before `app.stop`'s line.
   - **Tests that count identical lines:** give `Runtime` a test switch to turn folding off, or adjust those tests; don't widen the window.
   - **This doesn't help floods of distinct lines** (a hash per DHT harvest). Say so in the decisions file for B-log1b: its high-volume events stay `default_on: false`, and its bench decides.
4. **`GET /logs/stream`** (`api/src/logs.rs`, transport; ring and filtering in the binary).
   - **The source:** a `LogSource` trait in `api/src/logs.rs`, implemented in `logging/`. It's layered as `Extension(Arc<dyn LogSource>)` in `Services::run`, like `SystemApi`. Without it, every `/logs` route except `/logs/catalog` is `503 logs_unavailable`.
     - It reads the ring after a cursor under a filter, waits for new records, lists, opens and deletes files, and answers history pages.
     - `close()` ends every stream and in-flight download and refuses new ones with `503`.
   - **Guard:** `auth::protected` (a cookie session or the trusted-proxy header). Keys and feed tokens get `401 login_required` in every form (B12).
   - **Filters, applied on the server** before anything is queued:
     - `level` (minimum: `error`…`trace`, default `trace`);
     - `components` (comma-separated `Component` names, `libraries` and `system` included; unknown → `400 unknown_component`, `field: "components"`);
     - `text`: a case-insensitive substring, at most 200 characters, never a regex (`400 bad_parameter`, `field: "text"`). It's matched against the line **as shown** (message, event id or target, field names and shown values), so it can't probe hidden values.

     Each parameter is given at most once.
   - **Cursor:** `Last-Event-ID` (what `EventSource` sends on reconnect), else `?after=<id>`. The header wins.
     - With neither, the stream starts at the newest record and follows live. The UI loads `GET /logs` first and passes its newest id as `after`, which is gapless.
     - An id from another epoch → `resync {reason: "restarted"}`; one older than the ring's oldest → `expired`; a malformed one (or one from the future) → `malformed`. Each is followed by live from the head. Never an error.
   - **Backpressure:** per client, a bounded queue to the socket (B4's 16 frames), and **at most 256 matching records pending**.
     - When the client task looks and more than 256 matching records wait (or its cursor has left the ring), it sends **one** `resync {reason: "lagged", skipped, evicted}`. `skipped` counts matching records still in the ring; `evicted` counts records that left the ring unseen, whose match is unknown. Then it continues from the head.
     - The skipped records stay readable through `GET /logs`. Nothing grows per client, and a client that never reads costs its queue and a cursor.
   - **Frames:**
     - `retry: 5000` first, then one frame per record (`id: <epoch>-<seq>`, `event: line`, `data:` a `LogLine`);
     - `event: resync` (no `id:`);
     - `event: purge`, `data: {kinds, through}`: the client sets those kinds' values to `[hidden]` in lines up to `through` and drops lines whose `opt_in` is listed. That's what the ring did;
     - an SSE comment every 15 s.

     Headers: `text/event-stream`, `Cache-Control: no-store`, `X-Accel-Buffering: no`.
   - **Caps: 2 streams per session and 8 in all**, separate from B4's. Past either: `429 too_many_streams` with `Retry-After: 5`. The slot is held by the response body.
     - The session key is a hash of the session cookie, or of the proxy user for a proxy session, computed as `events.rs` `session_of` does. Copy it; don't import it. Never log either.
   - **It ends with its session.** At each heartbeat, re-check what let it in: `Auth::verify(cookie)` **or** `Auth::proxy_signs_in(peer, headers)`, with the headers and TCP peer (`peer_ip`) captured at open.
     - **Note for the runner:** `/events` re-checks only the cookie, so a proxy-header session's `/events` stream ends at its first heartbeat. Report it in the PR as a follow-up for `events.rs`; don't fix it here.
   - **Shutdown:** `logs.close()` in `Services::run`'s signal closure, before axum waits for connections.
5. **`GET /logs`, history**, newest first.
   - **Parameters:** the stream's filters plus `since` and `until` (RFC 3339 UTC, the files' format), `limit` (default 200, at most 1,000) and `cursor`.
   - **Response:** `{lines, next_cursor, more, unreadable}`.
   - **Order:** the ring (by sequence), then the files, newest first: the current file read backwards, then the rotated files by their names' stamps.
     - A file line carrying this epoch's id at or after the ring's oldest sequence is skipped: it's in the ring, or was purged from it.
     - Lines from older builds without an id are older than any line with one.
   - **The cursor** is opaque base64url (≤ 512 bytes): a version, the epoch, the position (ring sequence, or validated file name plus uncompressed byte offset) and a fingerprint of the filters.
     - Every field is validated on the way back in, and the file name goes through step 6's rules.
     - Another filter's cursor, or a bad one, is `400 bad_cursor`.
   - **The same masks as the stream:** file lines are shown through the **current** live mask at read time.
     - A field whose kind is off now reads `[hidden]` (hashes too; a `ref:` value already written stays).
     - A peer address beside a torrent field follows `PeerActivity`.
     - Lines of an opt-in event whose opt-in is off are left out.
     - `untyped` values and messages are re-run through `url::scrub_text`, and an unknown kind is hidden (fail closed).
     - The setup token reads `[redacted]` as in the ring.
     - Text matching runs after masking.
   - **Bounded work:**
     - at most **2 file reads at once** across history, downloads and archives (else `429 logs_busy`, `Retry-After: 2`), on the blocking pool;
     - a cap on decompressed bytes per request (pick it, record it). Hitting it returns a partial page with `more: true` and a cursor, never an error;
     - gzip streamed, never inflated whole into memory;
     - lines over 1 MiB, or that don't parse as the file's JSON shape, are skipped and counted in `unreadable`.

     No O(n²) paging at the default file size. Say what happens at the 1 GiB maximum.
6. **The files** (`logging/files.rs` behind `LogSource`).
   - **Which folder:** the runtime's (`Runtime.folder`, the one the file opened in this run).
     - If file logging hasn't opened this run, use the configured `file_folder` only if it's an existing directory (not a link) that, on Unix, belongs to Lodestar's account and isn't group- or world-writable. Otherwise list nothing, with the reason.
     - Never create or `chmod` the folder from the API.
   - **Names are untrusted:** the request path, the history cursor, and `index.json`, which is untrusted input.
     - A name is accepted only if it's ≤ 64 bytes and matches exactly `lodestar.jsonl` or `lodestar-YYYYMMDDTHHMMSS.mmmZ[-N].jsonl[.gz]` (what `rotated_name` and the compressor make).
     - Parse it into a typed value (`Current`, or stamp, `N` and gz) and **build the path from that value**, never by joining the string you were given.
     - It must also be in the folder's directory listing.
     - Open with `O_NOFOLLOW` (Unix; on Windows refuse a reparse point), and check the opened handle is a regular file.
     - `index.json` supplies **only flags**, looked up by a validated name. An entry that names anything else is ignored. **An unreadable index makes every file count as having details** (fail closed: the badge shows and redaction defaults on), and the listing says so.
   - **`GET /logs/files`:** `{folder, state (on|off|stopped|unavailable), index_readable, files: [{name, size, from, to, details, opt_in, current, compressed}]}`, newest first.
     - `from` is the first readable line's `ts`, reading at most 64 KiB, decompressed for gz.
     - `to` is the name's rotation stamp for a rotated file; for the current one, the last line written (in memory) or the mtime.
     - Cache per (name, size, mtime), bounded by the file count.
   - **`GET /logs/files/{name}?format=jsonl|text&redact=true|false`:**
     - **`redact` defaults to true when the file has details** (or the index is unreadable), otherwise false.
     - Always served decompressed. The current file is read up to its length at open, cut at the last newline.
     - **Headers:**
       - `Content-Type: application/x-ndjson` or `text/plain; charset=utf-8`;
       - `Content-Disposition: attachment; filename="<stem>[-redacted].<jsonl|txt>"`, built from the typed name (ASCII only, so no header injection);
       - `X-Content-Type-Options: nosniff`;
       - `Cache-Control: no-store`.
     - Unknown or refused name → `404 log_file_not_found`, `field: "name"`, never echoing it.
     - **A download holding a file open must not make rotation, pruning or delete fail.** Windows: std opens with `FILE_SHARE_DELETE`; keep it so, and test it on Windows CI.
   - **What "redact" means.** It's a transform of the copy being served. Lodestar never rewrites a log file in place, and nothing in §12.1 or §10.10.12 asks for that; deleting is how details leave the disk. Each line is parsed and rewritten through the **current** rules:
     - fields of the identifying kinds and the opt-ins (`hash` in hex, `name`, `path`, `url`, `query`, `peer`, `peer_activity`, `filter_changes`) become `[hidden]`. A `peer` beside a `hash`/`name`/`path` field counts as peer activity, and a `ref:` value stays;
     - lines of an opt-in event are dropped;
     - `client`, `size`, `error` and `label` fields stay, as Private shows them;
     - `untyped` messages and values go through today's `url::scrub_text`, so older lines get the newer URL rules;
     - the setup token reads `[redacted]`;
     - a line that doesn't parse becomes `[unreadable line]`. Redacted output never passes through bytes it couldn't parse.

     The result is what the Private preset would have written, except that torrents read `[hidden]` rather than this run's `ref:`. Another run's hashes must not be linked to this run's references.
     - `redact=false` serves the file's own bytes (JSONL), or each parsed line in text with `quote`/`one_line` escaping. An unparseable line is shown escaped.
     - `format=text` renders like the console text format, with `repeated` as its suffix.
     - Use one function for history masking and redaction, taking the mask.
   - **`GET /logs/archive.zip?format=&redact=`:**
     - every listed file, converted as above (`redact` defaults to true when any file has details), streamed;
     - written by a small in-house zip writer on `flate2` (deflate plus `Crc`, data descriptors). No new crate;
     - choose zip64 or a documented limit for members over 4 GiB, and record it;
     - a test-side reader verifies it.
   - **`DELETE /logs/files`**, body `{"which": "all" | "details"}`, same-origin JSON (`same_origin_json`):
     - **runs as a job on the `log-rotate` worker**, serialised with compression and pruning, so a file being compressed can't come back as `.gz` after it's deleted. The `.part` leftovers go too, and `index.json` is updated in the same step;
     - **the current file is never touched mid-write.** If it's to go, rotate it first under the writer's lock (`FileSink::rotate`), then delete the rotated name with the others. With file logging off or stopped there's no writer; delete under the same checks;
     - "details" means `details || opt_in`, or an unreadable index;
     - files that aren't regular (a link with a log's name) are skipped and reported, never followed;
     - **response:** `{deleted, bytes, skipped: [{name, reason}]}`;
     - log a new non-required `logging.files_deleted` (count and size only);
     - the ring isn't touched.

     A disk-full stop stays stopped until a restart (B-log1). Note that as a follow-up candidate.
7. **Data kinds, everywhere the API reads.** The stream and history show at most the live mask now in force, with read-time deadlines. Downloads with `redact=true` show at most Private; with `redact=false`, exactly what the file holds.
   - **Opt-ins appear only in the file, the ring, the stream and history, and only while on.** The console rules are B-log1's and unchanged.
   - `GET /logs/catalog` keeps its contract. `Applied::status()` gains the live output's row (enabled, level `trace`), and the kinds' `live` column is unchanged.
8. **The canary** (`log_canary.rs`): add the API outputs to every run.
   - **The outputs:**
     - **ring and history:** `GET /logs`, paged to the end;
     - **the stream:** opened before exercising, read over a socket in a thread, with one reconnect using `Last-Event-ID`;
     - **downloads:** every file in JSONL and text, `redact=true` and `false`;
     - **the archive.**
   - **Run 1 (Private):** no fixture identifier or passkey in any of them.
   - **Run 2 (one kind at a time):** a kind appears only in the outputs that may show it while it's on. After it's turned off:
     - history and a new stream show none;
     - the open stream got `purge`;
     - `redact=true` downloads show none.
   - **Run 3 (opt-ins):** filter-change detail appears in the stream and history only while on.
     - After the deadline it's gone from history and from a new stream.
     - It's in no `redact=true` download.
     - It's still in the `redact=false` download of the file written meanwhile, as a positive control: the file keeps it until deleted, and the file is badged.
   - **Run 4 (Full plus both opt-ins):** no secret or sealed name in any API output.
     - **The setup token appears in no API output except a `redact=false` download, and there only in its start-up line.**
     - Every `/logs` route (history, stream, files, one file, archive, delete) refuses an API key in all three forms and a feed token (401).
   - Never weaken a run. A narrow exemption is named in the PR.

**Runner decisions:**
- **Redacted downloads also hide client addresses:** sign-in and API client IPs (the `Client` kind) become `[hidden]` in a redacted download. Redaction exists for sharing a log, and a shared log must not carry the admin's own addresses. This is stricter than Private; record it under §2.19.
- **`/events` re-checks only the cookie at heartbeats,** so trusted-proxy sessions lose the stream after about 15 s. `events.rs` isn't yours: report it in the PR as a follow-up. The runner will schedule a fix.

## Tests
- **Ring:**
  - both bounds hold (5,000 entries; 4 MiB accounted), the oldest go first, and long entries are truncated;
  - a record from before a purge pushed after it is purged;
  - turning off a kind by save and by deadline rewrites those fields and removes opt-in entries;
  - no raw value is held (a test fixture's name with names off is nowhere in the ring's memory dump);
  - the setup token is never stored.
- **Folding:**
  - first lines are always written; N identical lines in 10 s give one line plus one summary with the right count and `first`;
  - required events and errors' first instances are never folded;
  - a Private console folds what a Full file keeps apart;
  - the 1,024-key cap passes lines through;
  - summaries flush on the timer and before `app.stop`.
- **Stream (real sockets, as `tests/events.rs` does):**
  - filters on level, components and text, with text that matches only hidden values finding nothing;
  - gapless resume by `Last-Event-ID` and `after`, and `restarted`/`expired`/`malformed`;
  - a client whose body is never polled: the writer isn't slowed, at most 256 records are pending, and one `lagged` resync comes with `skipped`/`evicted`;
  - the caps (2 per session, 8 in all, `Retry-After`);
  - ends within a heartbeat after logout or a password change, and stays up for a proxy-header session;
  - ends on `close()`;
  - `purge` frames.
- **History:**
  - the ring-to-file join has no gap or duplicate across a rotation;
  - filters, `since`/`until`, `limit` and cursors (bad and foreign ones are `400 bad_cursor`);
  - current-mask masking of older file lines;
  - unreadable lines are counted;
  - the read cap gives partial pages;
  - the `logs_busy` limit.
- **Files:**
  - **a property test that arbitrary names**, including `../`, absolute paths, NUL, `%2f`, Windows separators and drive letters, overlong names, and names that only look like the pattern, **are refused or resolve only to a listed regular file in the folder**;
  - a link named like a log is refused for download and skipped by delete;
  - a hostile `index.json` (traversal names, wrong types) only loses badges, and an unreadable one marks every file as having details;
  - the headers;
  - redaction per kind (peer activity by pairing, opt-in event lines dropped, untyped re-scrubbed, unparseable lines replaced);
  - text format escaping of control and bidi characters;
  - archive round trip;
  - delete all and details while writers run and a compression is in flight: no line lost from the new current file, no file resurrected, index consistent;
  - Windows: delete and rotate while a download is open.
- **Auth:**
  - every new route is `401 login_required` without a session and with keys or feed tokens in every form (the `tests/openapi.rs` walk covers new `ROUTES`);
  - `DELETE` refuses cross-site requests.
- **The canary's four runs**, with the API outputs.

## Done when
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass after merging `origin/main`.
- `cargo run -p lodestar --bin openapi` + `cd web && npm run api:types` leave no diff, and `python3 .github/scripts/log_lint.py` passes.
- `/security-review` has been run on the branch; its findings and fixes are in the PR.
- **One PR is preferred:** the canary and the redaction rules span the ring and the files. If it passes about 2,500 changed lines, not counting `openapi.json`, `schema.d.ts` and goldens, split it on the same branch into `(1/2)` (ring, folding, stream, history) and `(2/2)` (files, archive, delete, the canary's download parts). Each is security-reviewed, and the reason goes in the description.
- **The PR** is titled `B-log2: Logs API: ring buffer, flood folding, /logs/stream, history, files`. Its description lists:
  - the ring's bounds and accounting, and the purge rule;
  - the fold identity, window, cap and summary shape;
  - every route with its parameters, codes and caps, and the `LogSource` trait;
  - the id and cursor formats, and the `"id"` added to file lines;
  - the history read cap and concurrency limit;
  - the file-name rules and what "redact" does, field by field;
  - the zip writer's limits;
  - what each canary run now reads;
  - the `services.rs` lines and any `crates/log` change;
  - the `/events` proxy-session heartbeat gap, as a follow-up;
  - for B-log1b: distinct-line floods, and the setup-token special case to retire when it emits `auth.setup_token`;
  - the user-facing steps for the docs follow-up (S-logs and the guide): what "Remove torrent details" removes and keeps, that downloads and history hide what's off now, that deleting needs no password but shows the count, and that downloaded logs leave the machine;
  - the §2.19 refinements: per-output fold identity, purge as removal plus in-place rewrite, current-mask history, redact as a download transform only, the setup token withheld from API outputs, fail-closed badges, and the new `logging.files_deleted` event.
