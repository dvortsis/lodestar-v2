# Wire parsers and small protocol crates review
Reviewed at 55e155a9.

## Coverage
- Read in full (non-test source): `crates/bencode/src/*`, `crates/metainfo/src/*`, `crates/peer-wire/src/{fetch,wire,message,mse,rc4,route,stats,log_events,lib}.rs`, `crates/utp/src/{lib,packet,shared,conn,socket,ledbat,fuzzing}.rs`, `crates/tracker-scrape/src/{lib,udp,http,address,full}.rs`, `crates/network-policy/src/lib.rs`, `crates/release-parser/src/{lib,token,ambiguous,parse}.rs`, `crates/classify/src/{lib,payload,ext,composition,structure,record,effective,content,junk,adult}.rs`.
- Skimmed: `crates/release-parser/src/lexicon.rs` (lookup tables only); the in-file unit tests; `crates/*/tests/*` (read for what they cover, and for fixed sleeps); `fuzz/fuzz_targets/{bencode,metainfo,utp,tracker_scrape,full_scrape,release,local_torrent}.rs`.
- Not read: `crates/peer-wire/src/testing.rs`, `crates/tracker-scrape/src/testing.rs`, `crates/utp/src/conn/tests.rs`, `crates/*/examples/*` (test and bench scaffolding behind the `testing` feature, which only dev-dependencies and `crawler-sim` enable).
- Call sites outside the area were read only to confirm findings: `crates/pipeline/src/worker.rs:455-580`, `crates/pipeline/src/health/service.rs:195-225`, `crates/lodestar/src/settings/{crawler,trackers}.rs`, `crates/lodestar/src/config.rs:263-272, 872`.
- Nothing was built or run. Costs and sizes below are from reading the code and type layouts, not from measurement; each finding says so where it matters.

No blocking findings. No panic reachable from untrusted bytes was found in any of the eight crates.

## Findings

### WIRE-01 `fetch.max_metadata_mib` accepts 1–64, but the info parser rejects anything over 16 MiB
- Severity: important
- Kind: gap
- Confidence: confirmed
- Where: `crates/lodestar/src/settings/crawler.rs:115-121` (range 1–64), `crates/lodestar/src/config.rs:872` (passed to `FetchOptions::max_metadata`), `crates/pipeline/src/worker.rs:573` (`Info::parse`, default limits), `crates/metainfo/src/info.rs:33, 241-246` (`max_info_bytes: 16 << 20`, `Error::TooLarge`).
- What: the fetch cap is configurable up to 64 MiB, but the prepare stage parses with `Limits::DEFAULT`, whose `max_info_bytes` is a fixed 16 MiB. The `max_items` (2M) and `max_files` (200k) caps are fixed too. `prepare_one` maps every parse error to `Loss::Invalid`.
- Failure scenario: the owner sets Max metadata to 32 MiB to capture very large torrents. A 20 MiB info dict is downloaded in full, hash-verified, then rejected as TooLarge and counted as "invalid" in Insights, not "oversize". The setting above 16 only costs bandwidth and pool memory; nothing more is stored. `docs/architecture.md:138` promises "anything larger is recorded as oversize".
- Suggested fix: build the parse `Limits` from the same setting (`max_info_bytes = max_metadata`), or cap the setting's range at 16. Map `metainfo::Error::TooLarge` to `Loss::Oversize`.
- Tracked?: not found (`runner/slices.yaml`, `docs/decisions/`, `docs/build-plan.md` searched for `max_metadata`, `max_info_bytes`).

### WIRE-02 HTTP scrape client follows tracker redirects to loopback, private and link-local addresses
- Severity: important
- Kind: security
- Confidence: confirmed (code path); impact is limited, see scenario
- Where: `crates/tracker-scrape/src/http.rs:66-93` (`client`), `:108-126` (`DenyResolver`), `:228-233` (`scrape`).
- What: the only address check is the subscribed IP blocklist (`Deny`). With no blocklist the client is built with `Policy::limited(2)`. With one, the custom policy refuses a redirect only to a blocklisted literal IP, and the resolver drops only blocklisted answers. Nothing refuses loopback, RFC 1918, CGNAT, link-local (169.254.169.254) or unique-local targets, for a literal or for a name that resolves to one. `lodestar-network-policy` is not a dependency of this crate. The integrations and sources crates do guard their redirects (`docs/architecture.md:2548-2555`; `crates/sources/src/egress_tests.rs:1076-1080`).
- Failure scenario: the owner adds `http://tracker.example/announce` (the defaults are UDP, so this needs a user-added HTTP tracker). The tracker, or anyone on-path for plain `http`, answers `302 Location: http://127.0.0.1:<port>/<any path>` or `http://192.168.1.1/...`. Lodestar issues that GET from inside the user's network on every scrape round. It is blind: the body is parsed as bencode and only a ≤200-character `failure reason` could surface in a log.
- Suggested fix: in the redirect policy and the resolver, refuse non-public targets with `lodestar_network_policy::allows` unless the configured tracker host is itself private (a LAN tracker the owner chose). Refuse `https` to `http` downgrades.
- Tracked?: not found. `docs/decisions/2026-10-22-M4.md:90-93` records the blocklist checks only; `docs/decisions/2026-11-05-X-claim-trackers.md` covers claimed tracker URLs, which are never dialled. Open PR 791 (NOS-2a) adds an SSRF guard for raw TCP in a different crate.

### WIRE-03 One hostile info dict can cost about 100 MiB of decoder memory, outside the fetch reservation
- Severity: important
- Kind: resource
- Confidence: likely (sizes from type layout, not measured)
- Where: `crates/metainfo/src/info.rs:32-50` (`max_items: 2_000_000`), `crates/bencode/src/decode.rs:221-257`, `crates/bencode/src/value.rs:6-30`, `crates/pipeline/src/worker.rs:469-482, 573`; also `crates/metainfo/src/torrent.rs:58-92` (a `.torrent` is decoded twice, the whole file and then its info dict, both trees alive).
- What: every decoded value takes a `Value` slot in its parent's `Vec`. `Value` is 48 bytes by layout (`Dict` is a `Vec`, a slice and a `bool`); a dictionary entry is 64. The smallest values are 2 bytes on the wire (`le`, `de`, `0:`). So a 4 MiB info dict of 2M empty lists decodes to about 96 MiB of slots, and the last `Vec` doubling can briefly hold about 150 MiB. The fetch memory work (`X-fetch-pool-bytes`, `X-fetch-meta-reserve`) accounts the raw dict bytes only.
- Failure scenario: an attacker announces an info hash on the DHT whose dict is `l` + `le`×2,000,000 + `e` (it hashes correctly, since the attacker picks the hash). The fetch reserves 4 MiB; `Info::parse` then allocates roughly 100–150 MiB before failing with "info should be a dictionary". Parsing is serial per worker, so the spike is one at a time, but it is a third to a half of the worker's 256 MB RSS budget (`docs/architecture.md:59` and `:3360`) and can OOM a container with a tight memory limit.
- Suggested fix: reserve the decode bound (48 B × min(`max_items`, len/2)) against the same byte budget before parsing, or shrink the slot (drop `raw` from `List`, box the rare containers), or reject a top-level non-dictionary before the full decode. Record the bound in §3.
- Tracked?: not found. `docs/decisions/2026-11-06-WK-7.md` and `2026-11-06-X-fetch-meta-reserve.md` bound fetched bytes, not the decoded tree.

### WIRE-04 A legacy `name` escapes the 4,096-byte cap when `name.utf-8` is present, and reaches encoding detection whole
- Severity: important
- Kind: resource
- Confidence: likely (path traced end to end; CPU cost estimated, not measured)
- Where: `crates/metainfo/src/info.rs:278-288` (cap checks `name.utf-8` only), `:629-636` (single-file draft keeps `name` as `path`), `:461-495` (`legacy_encoding` pairs), `crates/metainfo/src/text.rs:115-125` (`MAX_FED` is tested before a chunk is fed, not within it).
- What: with a valid `name.utf-8`, only its length is checked. In a single-file torrent the raw `name` (up to 16 MiB) still becomes the draft's legacy `path`. If any other unlabelled text is not UTF-8 (a BEP 47 `symlink path` on the same dict does it), `legacy_encoding` takes that file as an encoding "pair": the whole legacy name is fed to chardetng, then decoded once for the detected encoding and once for each of about 35 candidate encodings, each decode allocating up to three times its size. The result is discarded; the stored path is the short `name.utf-8`.
- Failure scenario: info dict `{attr: "l", length: 0, name: <15 MiB of 0x80…>, name.utf-8: "x", piece length, pieces, symlink path: [<0xff>]}`. Parsing does about 36 full decodes of 15 MiB (hundreds of MiB of decoder work plus the detector pass), which I estimate at seconds of CPU on the single prepare thread (`crates/pipeline/src/worker.rs:469`). A steady supply of such dicts stalls ingestion for everything queued behind them.
- Suggested fix: apply `max_name_bytes` to the raw `name` as well, or skip pairs whose legacy side exceeds `max_path_bytes`, and count bytes inside `detect`'s feed loop.
- Tracked?: not found. `docs/decisions/2026-10-30-F-metainfo.md:9` introduced the pairs ("at most 16 such files are compared") without a per-file byte bound.

### WIRE-05 peer-wire has no fuzz target and no property test
- Severity: important
- Kind: test
- Confidence: confirmed
- Where: `crates/peer-wire/src/message.rs:84-190` (`parse_extended_handshake`, `parse_metadata_message`, `parse_pex`, `parse_handshake`), `crates/peer-wire/src/wire.rs:69-102`, `crates/peer-wire/src/mse.rs:122-192`, `crates/peer-wire/src/fetch.rs:421-675`; `crates/peer-wire/Cargo.toml:34` declares `proptest` and no file uses it; `fuzz/fuzz_targets/` has nothing for peer-wire.
- What: `runner/standards.md:42` requires property or fuzz tests for parsers. Every other untrusted-input parser in this area has one (bencode, metainfo, utp packet and connection, UDP and HTTP scrape, full scrape, release names, classify's stored blobs). The peer-wire message parsers, the framing, the MSE initiator and the `fetch_on_stream`/`Assembly` state machine have example tests and five scripted hostile behaviours (`crates/peer-wire/src/testing.rs:35-47`) only.
- Failure scenario: a later change to `Assembly::accept`'s growth clamp or to the reserve gating introduces an out-of-bounds slice or a `clamp` with min > max; nothing generates the interleavings (duplicate pieces, out-of-order pieces, repeated handshakes, a second handshake mid-transfer) that would catch it.
- Suggested fix: add a `peer_wire` fuzz target that feeds bytes to the three message parsers, and one that drives `fetch_on_stream` over `tokio::io::duplex` with a scripted peer (as `utp::fuzzing::connection` does), asserting the `Reserve` bound.
- Tracked?: not found in `runner/slices.yaml` or the two fuzz decisions (`2026-11-05-X-fuzz-1105.md`, `2026-11-05-X-fuzz-cache-1105.md`).

### WIRE-06 uTP delivers one buffered packet past a FIN
- Severity: polish
- Kind: correctness
- Confidence: confirmed
- Where: `crates/utp/src/conn.rs:610-621`
- What: when an in-order FIN arrives while packet FIN+1 sits in the out-of-order buffer, `receive` sets `eof`, then the drain loop removes FIN+1, advances `ack_nr` and appends its payload to `readable` before the `if self.eof` check clears the buffer.
- Failure scenario: a peer sends DATA with sequence N+1, then FIN with sequence N. The application reads those bytes after what should be end of stream, and our ACK covers N+1. Only the peer's own bytes are affected, so the effect is cosmetic.
- Suggested fix: check `self.eof` before the drain loop.
- Tracked?: not found. PR 788 touches `conn.rs` (ACK and timeout handling) but not this path.

## Checked and found sound
- Bencode decoder: iterative with an explicit stack, depth and item caps, checked integer and length arithmetic, duplicate keys rejected in lenient mode too, raw spans exact (`crates/bencode/src/decode.rs`). The encoder's key-order `assert!` is reachable only from our own writers.
- Metainfo: sizes summed with `checked_add` and capped at `i64::MAX`; file count, path depth and path bytes capped before allocation and re-checked after re-encoding; recursion in `walk_tree` bounded by the bencode depth; info hash taken over the exact raw span (`crates/metainfo/src/info.rs`, `torrent.rs:73-81`). Every `encoding_rs` encoding is in the id table, so `from_encoding` cannot panic on a hostile `encoding` label (`text.rs:10-51, 73-84`).
- ut_metadata assembly: a piece must have been requested, must have the exact length for its index, cannot grow the buffer past the requested window or the announced size (`clamp(end, requested)` always has `end <= requested`), duplicates are ignored, a changed `total_size` is refused, and the result is returned only after SHA-1 or truncated SHA-256 matches (`crates/peer-wire/src/fetch.rs:519-546, 586-675`).
- Extension handshake: `metadata_size` must be 1..=`max_metadata`; id 0 is treated as disabled; a repeated handshake is ignored; data before the handshake is a protocol error (`fetch.rs:483-522`, `message.rs:84-107`).
- Framing: a zero-length extended body is refused before `payload[0]`; non-extended messages up to 4 MiB are read through a 4 KiB scratch buffer, never allocated (`crates/peer-wire/src/wire.rs:69-102`).
- MSE: public keys 0, 1, P−1 and ≥ P refused; pad lengths capped at 512; the sync search stops after 520 bytes; a method we didn't offer is refused, so `Require` can't be downgraded (`crates/peer-wire/src/mse.rs:72-79, 161-179`).
- Dial policy: `network_policy::allows` is applied before every dial and again per attempt, after mapped-IPv4 normalisation; the IPv4 and IPv6 special ranges checked against the IANA registries (`crates/peer-wire/src/fetch.rs:187-191, 250-253, 359`, `crates/network-policy/src/lib.rs:76-107`).
- uTP packet parser: all offsets checked, extension chain capped at 8, SACK length validated (`crates/utp/src/packet.rs:96-141`).
- uTP connection: 16-bit sequence arithmetic is wrap-safe with ≤ 256 in flight and a 256-packet reorder window; `flight` and `unacked_bytes` cannot underflow; unread plus out-of-order bytes stay under twice the window; the timer is always re-armed while anything is in flight; no reply is ever sent for an unknown connection, so the shared DHT port can't be used for reflection (`crates/utp/src/conn.rs`, `crates/utp/src/socket.rs:382-402`).
- uTP table: 4,096 connections, 8 per IP, closing ones counted; no production code calls `listen`, so inbound SYNs are dropped and the accept path runs only in tests (`socket.rs:404-409, 485-515`; `rg '\.listen\('` finds tests only).
- UDP scrape: connected socket plus a random 32-bit transaction id; retries and the reconnect-once path are bounded; more entries than hashes is an error (`crates/tracker-scrape/src/udp.rs:198-310`).
- HTTP scrape parse: 64 KiB body cap enforced while streaming, depth 4 and 4,096 items, counts clamped, `min_request_interval` capped at a day, errors never carry the URL (`crates/tracker-scrape/src/http.rs:146-274`).
- Full-scrape push parser: flat memory, length checked against the remaining byte budget before skipping, depth and entry caps, digits capped at 20 (`crates/tracker-scrape/src/full.rs:263-547`).
- Release parser: no regex crate, input cut at a char boundary to 512 bytes and 96 words, so cost is bounded whatever the name; every slice index traced to a token boundary; number casts guarded by digit caps (`crates/release-parser/src/parse.rs`, `token.rs`).
- Classify: one pass over the files with saturating counters; `hides_media_extension`'s slice and `scene_date`'s `words[0]` are guarded by their preconditions; `Composition::decode` and `Record::decode` are strict and property-tested (`crates/classify/src/junk.rs:364-372`, `adult.rs:269-280`, `composition.rs:131-208`).
- Tests: no fixed sleeps used as waits. The sleeps found are polling loops with 10–20 s deadlines or simulated link latency (`crates/utp/tests/socket.rs:38-46, 294`, `crates/peer-wire/tests/fetch.rs:349-356`). No ignored tests.

## Area-level concerns
- **Fuzz coverage against the parsers that exist.** Covered: bencode, metainfo (`Info`, `TorrentFile`), utp (packet and connection script), tracker scrape (UDP, HTTP), full scrape, release names; classify's `Record::decode` through `worker_spool`, `Composition::decode` by proptest. Not covered by fuzz or property tests: all of peer-wire (WIRE-05), `TrackerUrl::parse` (wraps the `url` crate; unit tests only), `network-policy` (table tests only, which suits a pure predicate).
- **Limits live in three places that don't know about each other:** the fetch cap (setting), the parse limits (`metainfo::Limits::DEFAULT`, hard-coded at the call site) and the memory reservation (raw bytes only). WIRE-01, WIRE-03 and WIRE-04 all come from that seam. One `Limits` value built from settings and passed to both the fetcher and the parser would close it.
- **PR 788 overlap (uTP RTO backoff).** While reading `conn.rs` I noted that a peer that keeps sending packets but never acknowledges our data is never given up on (`heard` suppresses the timeout count, `conn.rs:404-406`), and that a selective ACK is applied whatever its `ack_nr`. PR 788's description says it fixes both, so they are not listed as findings. On `main` the connection is still bounded by the fetch deadline plus the 5 s linger.
- **uTP connects always start at sequence 1 with a 16-bit random connection id** (`socket.rs:450-451`). That matches BEP 29, and injected data would still have to pass the info-hash check, so I did not raise it; a random initial sequence would cost nothing.
- **`Encryption::Prefer` falls back to plaintext only on an MSE error.** A peer that accepts our DH key and then stays silent uses the whole fetch deadline and gets no plaintext retry (`fetch.rs:316-321`). Real non-MSE clients close at once, so this is by design, but it has no test.
