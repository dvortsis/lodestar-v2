# DHT crawler review
Reviewed at 55e155a9.

## Coverage
- Read in full (non-test source): `crates/dht/src/{lib,krpc,compact,routing,secure_id,state,restart,bep33}.rs`; `crates/dht/src/node/{mod,actor (lines 1-2703),tokens,peers,sampler,lookup,hints,stream,prefixes,shard,seen,transport,testing,proof}.rs`. Followed calls into `crates/network-policy/src/lib.rs` (policy functions) and the wiring in `crates/lodestar/src/config.rs` (`Dht`, `dht_config`, `pipeline_config`) and `crates/lodestar/src/services.rs:258-283`.
- Skimmed: `crates/dht/src/log_events.rs`; the inline tests of `routing.rs`, `krpc.rs`, `secure_id.rs`, `state.rs`, `peers.rs`, `stream.rs`; test names in `node/{hint,prefix,stream,shard,join}_tests.rs` and `crates/crawler-sim/tests/sim/main.rs`; `crates/dht/tests/{network,harvest,pause,scrape}.rs` around their sleeps; `crates/crawler-sim/tests/sim/README.md`; decisions `2026-10-25-CR-0.md`, `2026-11-12-X-flake-dht-scrape-join.md`; architecture §5 and the M1 DHT decisions in §16.
- Not read: the test bodies in `node/actor.rs` (2704-4345), `node/proof/tests.rs`, `restart/tests.rs`, `node/*_tests.rs`, and `crates/crawler-sim` source beyond the README and test names (time; read tests only to judge coverage). `docs/decisions/2026-10-30-CR-0d.md` not read in full, so the restart proof was checked against its code and comments, not line by line against its spec. Nothing was built or run: every finding is from reading.

## Findings

### DHT-01 Any inbound query can spend the node's outbound query budget (verification ping before any limit)
- Severity: blocking
- Kind: security
- Confidence: confirmed (code trace; not run)
- Where: `crates/dht/src/node/actor.rs:1243-1264` (ping), `:1269-1272` (reply limit comes after), `:1082-1087` (shared `query_rate` and outstanding cap); `crates/dht/src/routing.rs:194-203` (`would_accept`)
- What: for every inbound query whose sender id `would_accept`, `on_query` sends a verification ping through `Actor::query`, which takes a token from the one `query_rate` bucket and a slot in `pending` (4 s if unanswered). This happens before the reply rate limit is consulted and has no budget of its own and no per-source limit. `would_accept` is always true for an id that falls in the last bucket (`idx == len-1 && len < 160`), and is computed before the insert, so it ignores the replacement cache and the one-entry-per-IP rule. The only dedup is "no query pending to this exact `SocketAddr`".
- Failure scenario: a host learns our ids from our replies, then sends `ping` queries with ids sharing our id's prefix from ~800 source ports (or one port, answering each ping and changing the id). At Balanced, 200 such packets a second (about 20 KB/s) take every query token and hold ~800 of 1,024 outstanding slots; `sample_tick` stops at 80% outstanding and lookups get `Throttled`. Sampling, lookups, bucket refresh and scrapes stall for as long as it lasts. With spoofed sources each packet also sends a ping plus a reply to the victim. Low needs 50 pps.
- Suggested fix: give verification pings their own small budget (a few per tick), skip them when the IP already holds an entry or a cached replacement, limit them per source IP (/64 for IPv6), and take the reply-rate decision before any work for the query.
- Tracked?: not found (grep of `runner/slices.yaml`, `docs/decisions/`, `docs/build-plan.md`). §16 records the ping itself ("get a verification ping when their bucket has room") but no budget.

### DHT-02 Transaction ids are a counter, so any queried node can predict them and forge replies
- Severity: blocking
- Kind: security
- Confidence: confirmed (mechanism); the forgery needs source-address spoofing
- Where: `crates/dht/src/node/actor.rs:712` (random start), `:1088-1099` (`next_tid + 1` per query), `:1403-1413` (a reply is accepted on tid + source address + family)
- What: the 8-byte transaction id is `start + n`. Every node we query sees the current value, so the ids of the next queries are known to it. The only other check is that the datagram's source equals the queried address, which UDP lets an off-path sender forge. The expected-node-id check (`consistent`) doesn't help when the attacker supplied the `(id, addr)` pair in a `nodes` list. `docs/decisions/2026-10-25-CR-0.md` ("Wire correlation") treats the id as a uniqueness token only.
- Failure scenario: an attacker's node returns a `nodes` list of addresses it doesn't own, in a known order (see DHT-03: up to ~2,500 per reply). We sample them in FIFO order with consecutive tids. One of its own endpoints, placed just before, reveals the counter; it then sends spoofed replies "from" each listed address with tid+1, tid+2, … Each is accepted as genuine: its `ip` field is an external-address vote from a distinct /24 (enough to win the 32-vote window and force new node ids, DHT-04), its sender enters the routing table as a verified `Contact::Response` node (bypassing one-entry-per-IP, since the addresses are arbitrary), and its `samples` are harvested.
- Suggested fix: make the id unpredictable while keeping it unique: a per-process keyed permutation of the counter (keep bit 63 clear for the sampler's proof tokens), or a random 63-bit value checked against `pending`.
- Tracked?: not found. CR-0's decision file records the counter as deliberate for uniqueness; it doesn't discuss predictability.

### DHT-03 Fixed sleeps that assume a join has finished, then assert on it
- Severity: blocking (per `runner/standards.md` §2/§5: a fixed sleep that assumes work finished)
- Kind: test
- Confidence: confirmed
- Where: `crates/dht/tests/scrape.rs:209` (500 ms, then a scrape that must find 4 seeds / 7 peers); `crates/dht/tests/network.rs:168-169` (800 ms, then `queries_in > 0`), `:184-185` (800 ms, then `denied > 0`); `crates/dht/tests/harvest.rs:291` (500 ms "let it join")
- What: scrapes are not parked behind the bootstrap (`scrape_tick`, `actor.rs:2520-2543`), so a scrape started before the first router reply runs on an empty table, finishes at once with `DhtEstimate::default()`, and the `(4, 7)` assertion fails. The two `network.rs` tests assert a positive counter after a fixed 800 ms. `harvest.rs:291` is protected by lookup parking but is the same pattern.
- Failure scenario: a loaded or slow CI runner (Windows) delays the crawler's first bootstrap reply past the sleep; the test fails and is re-run.
- Suggested fix: poll for the condition with a 10 s deadline (table size ≥ expected, `queries_in > 0`, `denied > 0`), then make the negative assertions.
- Tracked?: not found. `docs/decisions/2026-11-12-X-flake-dht-scrape-join.md` attributes the earlier `scrape.rs` abort to tokio 1.54.0 and changed no test; open PR 778 (test flakes) touches no DHT file.

### DHT-04 Reply lists are not capped, so one reply can fill the sampler frontier and pin its cooldown table
- Severity: important
- Kind: security
- Confidence: likely
- Where: `crates/dht/src/krpc.rs:286-310` (`nodes`, `nodes6`, `samples` accepted at any whole-entry length); `crates/dht/src/node/actor.rs:1461-1484` (every listed node offered to the sampler), `:1494-1498` (every sample harvested), `:82`, `:2383-2387` (a timed-out sample cools for 6 h); `crates/dht/src/node/sampler.rs:15-20, 88-104` (20,000 frontier, 200,000 states keyed by `SocketAddr`)
- What: a 64 KiB datagram can carry about 2,500 IPv4 node entries or 3,276 sample hashes; honest nodes send 8-16 nodes and about 20 samples. All of them go to the sampler frontier (FIFO, dropped when full) or the pipeline. Each fake endpoint that we then sample times out and holds one of the 200,000 cooldown slots for six hours (CR-0: no unexpired obligation is evicted). The sampler is keyed by `SocketAddr`, so one IP with many ports counts as many nodes: there is no per-IP (or per-/64) bound on frontier entries or on samples sent.
- Failure scenario: a handful of attacker endpoints (ports on one IP) each answer with 2,500 unreachable addresses. Eight replies fill the frontier; honest candidates are dropped while it is full. At Balanced (100 samples/s) the junk drains in 200 s and is refilled; about 80 replies' worth occupies the whole cooldown table for 6 h, after which `sample_tick` returns at `saturated()` and BEP 51 harvesting stops. Listing one victim IP with many ports also points our sample queries at it (bounded by our rate).
- Suggested fix: take at most K (or 16) nodes per family and a bounded number of samples (for example 64) from one reply; bound frontier entries per IP / per /64; consider a shorter cooldown for endpoints that never answered once.
- Tracked?: not found. The 200,000 / 20,000 bounds and the 6 h timeout backoff are recorded in `docs/decisions/2026-10-25-CR-0.md`; the per-reply and per-IP exposure is not.

### DHT-05 IPv6: Sybil limits are per address, and external-IP votes cross families
- Severity: important
- Kind: security
- Confidence: likely (needs an IPv6 bind; the default bind is IPv4 only)
- Where: `crates/dht/src/routing.rs:129-130, 172-184, 250-260` (one entry per `IpAddr`); `crates/dht/src/node/actor.rs:180-191` (/64 per vote), `:209-231` (vote keyed by the reported family, not the reporter's), `:1425-1427`, `:1609-1642`
- What: (a) the routing table's one-entry-per-IP rule uses the full /128, so one host with a /64 can hold every slot of the IPv6 table (8 × up to 160 buckets, and force it to split to full depth by using ids next to ours). (b) An external-address vote is filed under the family of the reported address, whatever family the reporter used. An IPv6 reporter can vote for an IPv4 address, and IPv6 reporter networks are /64s, which a single /48 supplies 65,536 of. The window is 32 votes with a majority of 17.
- Failure scenario: an IPv6 attacker puts its endpoints (distinct /64s) in our frontier (DHT-04 makes them contiguous), so one tick's samples all go to it. Their replies carry `ip` = a bogus IPv4 address; the last 32 IPv4-window votes are theirs, the majority flips, `on_external_ip` regenerates every node id for the bogus address and rebuilds both tables. Honest replies flip it back, regenerating ids again. Each flip costs a rejoin (self-lookup plus up to 32 join-refresh lookups per family) and invalidates what other nodes know of us.
- Suggested fix: key the IPv6 one-per-IP rule on /64 (as the vote already does); ignore a vote whose reported family differs from the reporter's; require the majority to hold for some time (or more votes) before changing ids.
- Tracked?: not found.

### DHT-06 Bootstrap router names are resolved once; a start without DNS never joins
- Severity: important
- Kind: correctness
- Confidence: confirmed
- Where: `crates/dht/src/node/actor.rs:510` and `:656-668` (`resolve`, once, 5 s per name), `:2300-2341` (`start_bootstrap` uses `self.bootstrap`), `:2476-2483` (re-bootstrap passes no saved nodes), `:2344-2360` (one warning)
- What: `run` resolves the router names before the event loop and never again. The re-bootstrap every 30 ticks reuses the resolved list and `Vec::new()` for saved nodes (those are used only by the first bootstrap).
- Failure scenario: the process starts before DNS works (a VPN sidecar such as gluetun still connecting, which architecture §5 names as a common Unraid setup; or a boot race). `self.bootstrap` is empty; saved nodes time out during the first lookup. Every later re-bootstrap has no router and no node to ask, so the table stays empty until the process is restarted. `dht.bootstrap_unreachable` is logged once (`resolved=0`) and never again.
- Suggested fix: re-resolve when a bootstrap round starts with an empty (or wholly unanswered) router list, with backoff; keep the saved nodes for re-bootstraps while the table is empty.
- Tracked?: not found.

### DHT-07 A thin table in one family re-queries every family's bootstrap routers every 30 s, with no backoff
- Severity: important
- Kind: correctness
- Confidence: confirmed
- Where: `crates/dht/src/node/actor.rs:2476-2483` (`thin` is `any` family below 32 nodes), `:2315-2332` (`start_bootstrap` loops over all families and their routers)
- What: when any family's table has fewer than `HEALTHY_NODES`, `start_bootstrap` runs for all families, each asking all of its routers. There is no backoff and no per-family condition.
- Failure scenario: `bind` lists an IPv6 address on a host whose IPv6 doesn't reach the DHT (Docker's default bridge, a VPN without IPv6). The IPv6 table never reaches 32 nodes, so the healthy IPv4 family sends `find_node` to every resolved public router every 30 s for the life of the process, plus a redundant self-lookup. That is the load on shared routers that §2.33 politeness is meant to avoid.
- Suggested fix: re-bootstrap only the thin family, and back off (30 s, 1 min, … capped) while it stays thin.
- Tracked?: not found. §16 records "thin tables keep bootstrapping" for a single table only.

### DHT-08 A stale bucket that cannot change is refreshed again on every tick
- Severity: important
- Kind: resource
- Confidence: likely
- Where: `crates/dht/src/routing.rs:395-402` (`refresh_targets` reads `last_changed`, which only a contact from a node in that bucket updates), `:268-281` (a full last bucket splits before its bad entries are replaced); `crates/dht/src/node/actor.rs:2416-2424`
- What: starting a refresh lookup doesn't stamp the bucket. A bucket with no live node, in a keyspace region that has none, stays stale for good, and `maintain` starts lookups for the first two stale buckets every tick. Such buckets appear over time: the last bucket keeps dead entries (they are only swapped when a verified replacement exists) and splits on the next arrival rather than replacing them, so churn deepens the table into prefix lengths where the DHT has under one node per bucket. DHT-05 lets a /64 force this on the IPv6 table at once.
- Failure scenario: after days of uptime the table has a few deep, dead buckets. Every second two `find_node` lookups for targets beside our own id query the same 8-16 nearest neighbours: roughly 16+ queries a second forever (about a third of the Low budget), all aimed at the same few nodes.
- Suggested fix: record when a bucket was last refreshed and refresh it at most once per `REFRESH_AFTER_SECS`; replace a bad entry before splitting. Add a test that a refreshed bucket isn't refreshed again on the next tick.
- Tracked?: not found.

### DHT-09 BEP 42 is never applied to other nodes' ids, though the module says it is
- Severity: important
- Kind: gap
- Confidence: confirmed
- Where: `crates/dht/src/secure_id.rs:1-4, 87-95`; no caller of `secure_id::is_valid` for a remote node in `crates/dht/src/node/actor.rs` or `routing.rs` (its only uses are our own ids, `actor.rs:593, 637, 1614`)
- What: the module doc says the crawler uses BEP 42 "to judge other nodes' ids". Nothing does: routing-table insertion, bucket replacement and lookup candidates accept any id from any public address. Architecture §5 lists BEP 42 without saying which half.
- Failure scenario: a node with N IPv4 addresses places N ids next to ours (or next to a hash being looked up) at no cost; with IPv6, see DHT-05.
- Suggested fix: decide and record it. The usual soft form is enough: prefer BEP 42-valid nodes when a bucket is full and when choosing lookup candidates, without refusing invalid ones. Otherwise correct the doc comment.
- Tracked?: not found.

### DHT-10 Maintenance lookups have no query cap and no per-IP diversity
- Severity: important
- Kind: resource
- Confidence: likely
- Where: `crates/dht/src/node/actor.rs:2071-2081` (owned requests are capped at 64 queries; others get 3 slots per drive with no total), `:2029-2055` (only the 25 s / 30 s deadline); `crates/dht/src/node/lookup.rs:88-119` (candidates deduplicated by id only)
- What: a bootstrap, refresh, exploration or scrape lookup keeps querying the K closest fresh candidates until none are left or its deadline passes. A responder that keeps returning closer ids at its own endpoints (many ports on one IP are distinct candidates) keeps it running for the full 25 s at whatever rate its replies allow.
- Failure scenario: an attacker endpoint with 20 ms replies drives one exploration lookup at about 150 queries a second for 25 s, most of the Balanced budget, and exploration lookups restart whenever the frontier is low.
- Suggested fix: cap queries per maintenance lookup (as `MAX_PEER_QUERIES` does) and allow one candidate per IP (/64) in a lookup.
- Tracked?: not found (CR-0 records the 25 s deadline as the bound).

### DHT-11 Inbound replies have only a global rate limit; one source can take it all, and replies amplify
- Severity: important
- Kind: security
- Confidence: confirmed (limit), likely (amplification figures are estimates)
- Where: `crates/dht/src/node/actor.rs:133-136` ("so we can't be used as an amplifier"), `:1269-1272`, `:1285-1368`; `crates/dht/src/node/peers.rs:21-22, 58-69`
- What: replies are limited by one bucket (2 × the query rate: 400/s Balanced, 2,000/s Aggressive). There is no per-source limit. A `get_peers` with `scrape=1` and `want=[n4,n6]` for a hash with 50 stored peers is answered with roughly 1.3-1.6 kB (50 values, two 256-byte filters, 8 + 8 nodes) for a query of about 110 bytes, and the same IP may store all 50 peers (ports) for a hash with one token.
- Failure scenario: one sender at 400 pps takes every reply token, so honest queriers get nothing. `on_query` returns at the limit before the passive-harvest step (`:1269-1272` precedes `:1370-1392`), so their `get_peers` and `announce_peer` hashes are lost too: one source can switch off passive harvesting. With a spoofed source, the victim receives about 5 Mbit/s at Balanced and 25 Mbit/s at Aggressive from a tenth of that in queries.
- Suggested fix: add a per-source-IP (/24, /64) reply limit in front of the global one; cap `values` and drop filters when the reply would exceed one MTU.
- Tracked?: not found.

### DHT-12 The passive path and the reply limiter have no test, and the simulator sends no inbound queries
- Severity: important
- Kind: test
- Confidence: confirmed
- Where: `crates/crawler-sim/tests/sim/README.md:180` and the goldens (`queries_in=0`); no assertion on `replies_dropped` in `crates/dht` (only read in `crates/crawler-sim/tests/e2e/net.rs:116`); no test of the query-triggered ping (`actor.rs:1250-1263`) or of oversized `nodes`/`samples`
- What: the code reachable from unsolicited packets (`on_query`: replacement cache, verification ping, token issue and check, reply limit) is covered only by a few happy-path integration tests. Nothing exercises it under load or with hostile input, which is where DHT-01, DHT-04 and DHT-11 sit.
- Failure scenario: a change to `on_query` that removes the reply limit, or pings without bound, passes CI.
- Suggested fix: actor-level tests for the reply limit, a ping budget, and a reply with thousands of nodes; add an inbound-query knob to the simulator.
- Tracked?: partly. The README lists "no inbound queries" as a known limit; no slice found for it.

### DHT-13 `sample_infohashes` replies advertise hashes we only saw, not hashes we store
- Severity: polish
- Kind: gap
- Confidence: confirmed
- Where: `crates/dht/src/node/actor.rs:1305-1308, 1346-1354`, `:913-919`
- What: BEP 51's `samples` are a subset of the info hashes in the node's storage and `num` is their count. We answer with the last 20 newly seen hashes (from other nodes' samples and `get_peers` queries) and `num = 20`, for which our peer store has nothing.
- Failure scenario: crawlers that sample us get hashes for which we return no peers, and we re-publish other nodes' samples under our name.
- Suggested fix: sample from `peer_store` and report its size, or record the deviation.
- Tracked?: not found.

### DHT-14 `dht-state.bin` is written and fsynced on the actor task; a corrupt file is dropped silently
- Severity: polish
- Kind: resource
- Confidence: confirmed
- Where: `crates/dht/src/node/actor.rs:2485-2491`, `:504-507`; `crates/dht/src/state.rs:142-163`
- What: the 10-minute save does write + `sync_all` + rename inline in `maintain`, unlike the restart proof, which uses the blocking pool. `State::load(..).unwrap_or_default()` turns a corrupt or unreadable file into a cold start with no log line. `State::load` reads the whole file before the 1 MiB check, and `save` doesn't fsync the directory.
- Failure scenario: on a spun-down array disk the tick blocks for seconds, during which no packet is read and queries time out. A damaged state file costs the node its ids and warm start with nothing in the log.
- Suggested fix: save through `spawn_blocking`; log a warning with the `StateError`; bound the read.
- Tracked?: not found.

### DHT-15 Smaller items
- Severity: polish
- Kind: maintainability
- Confidence: confirmed
- Where / What:
  - `crates/dht/src/node/actor.rs:589, 622`: `virtual_ids` is clamped to 64; architecture §5 and the `Config` doc say 1-16, and nothing in `crates/lodestar/src/config.rs` validates it.
  - `crates/dht/src/node/actor.rs:1627-1634`: after an id change every non-bad entry is re-inserted as `Contact::Response` at `now`, so questionable nodes become good without having answered.
  - `crates/dht/src/routing.rs:228-260` with `actor.rs:1430-1456`: a query-only (unverified) replacement entry holds its IP; when our ping to it is answered under another id, nothing removes the entry, so the real node at that IP is refused (`IpTaken`) until eight newer entries push it out.
  - `crates/dht/src/node/actor.rs:1217-1233, 1285`: `nodes_for` sorts the whole table for each family on every query, including `ping`, `announce_peer` and unknown methods that send no nodes.
  - `crates/dht/src/node/actor.rs:543-548`: a receive error is ignored with no backoff; a persistently failing socket spins the loop.
  - `crates/dht/src/node/actor.rs:510`: `resolve` (up to 5 s per name) runs before the loop, so `Node::shutdown` can wait up to 20 s during start.
- Tracked?: not found.

## Checked and found sound
- KRPC parsing (`krpc.rs`, `compact.rs`): depth 4 / 4,096 items, transaction id ≤ 32 bytes, whole-entry checks before `chunks_exact`, no reachable `unwrap`/index panic; encoder key order is sorted in every arm.
- Malformed packets get no answer, unknown methods get a short 204 error, and the reflected `t` is at most 32 bytes.
- `announce_peer` tokens (`tokens.rs`): SHA-1 of a 16-byte secret and the source IP, rotated every 5 min, previous secret accepted; checked before storing or harvesting the peer; `implied_port` and port 0 handled.
- Response matching: 8-byte tid, exact source address and family, expected node id for known nodes; errors settle the pending query (but see DHT-02 for predictability).
- Outbound policy: every query path goes through `endpoint_allowed` (`network-policy` public-unicast check plus the live blocklist); the seed exemption applies only to resolved bootstrap addresses; bogons, mapped IPv4, scoped IPv6 and port 0 are refused. Announce-harvest peers are checked again by the fetcher (`peer-wire/src/fetch.rs:187`).
- BEP 51 intervals: clamped to 60 s - 6 h, anything unsupported or over 6 h cools for 6 h, timeouts for 6 h, scale 1-8 only multiplies; no config or env path reaches `sample_start_delay`, `allow_private`, `query_timeout` or `tick` (`config.rs:836-856`); rates are the three presets only.
- Bounded state in the actor: `pending` (outstanding cap), `pending_addrs`, lookups (16 maintenance, 32 owned × families, 16 scrapes), 64 candidates per lookup, scrape queue 1,024, peer store 10,000 × 50 with expiry, hints 20,000 + guard 20,000, sampler 200,000 / 20,000, seen and contacted filters fixed-size and keyed, vote windows 32, unsolicited sources 64, routing table ≤ 160 buckets × (8 + 8).
- Restart proof (`restart.rs`, `proof.rs`): sealed containers with length and SHA-256, strict key sets, sorted-unique entries, bounded reads, every decode failure suppresses; in-place invalidation before the first sample; file work off the actor thread; lock held across jobs.
- Feature switches: each of `coordinated_admission`, `responder_hints`, `streaming_peers`, `target_prefixes`, `safe_restart`, `utp`, `route_backoff` is default off, wired from `config.rs` to the code that reads it, and has an off-path test (`switch_off_allocates_no_hint_state_and_sends_as_today`, `switch_off_draws_each_target_uniformly_as_before`, `restart_without_safe_restart_suppresses_in_full`, `without_utp_a_utp_datagram_is_malformed_krpc_as_before`, the unchanged goldens). `route_backoff` lives in peer-wire, outside this area.
- The `testing` feature (B-tree maps, fixed seen-filter keys, in-memory socket) is a dev-dependency feature of `crawler-sim` only; release builds use `-p lodestar`, and CI also runs `cargo test -p lodestar-dht -p lodestar-sources` with production types (`ci.yml:193`).
- Shutdown: a dropped `Node` ends the loop (watch closed), permits in the command channel and in running requests are returned, state is saved.

## Area-level concerns
- The actor trusts the content of authenticated replies too much. Lists are unbounded per reply, endpoints from `nodes` are treated as independent whatever IP they share, and several limits are keyed by `SocketAddr` or /128. A consistent "per reply" cap and a "per IP / per /64" key across the routing table, sampler, lookups and hint guard would close DHT-04, DHT-05 and DHT-10 together.
- All outbound work shares one token bucket with no classes. Anything an outsider can trigger (verification pings, DHT-01) or prolong (lookups, DHT-10) takes budget from sampling and owned lookups. CR-0 notes priority lanes as later work (CR-2).
- Passive harvesting passes every valid `announce_peer` to the pipeline without dedup (`harvest_hash`, `Source::Announce`). One IP with one token can announce a new random hash per reply token (400/s at Balanced), each with itself as the only peer. Whether the fetch pool's queue copes is outside this area; I did not trace it (suspected).
- `actor.rs` is 2,700 lines of non-test code with about 60 fields on one struct; bootstrap/rejoin/join-refresh state is spread over nine fields and four functions. The recent fixes (X-dht-bep42-rejoin, X-dht-rejoin-nits, X-dht-lookup-converge) all landed there, and DHT-06/07/08 are in the same logic.
- The simulator models IPv4 only, no inbound queries, no churn and no hostile nodes, so none of the findings above can show in its goldens.
