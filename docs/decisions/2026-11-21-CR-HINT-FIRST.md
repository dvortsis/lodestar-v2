# CR-HINT-FIRST decisions (hint-first lookups)

Build-plan §0.5 "Path to 100", item 4: a hash captured from a BEP 51 reply gets one direct `get_peers` to that responder, through CR-2's T1 tier at the pool, one hash per responder, and the request finishes if that returns values. It targets the two causes CR-1a-H's screen found (`docs/decisions/2026-10-30-CR-1a-H.md`): about 94% of hints expired because keys waited in the pool longer than `HINT_TTL`, and about half of the rest were throttled by the per-IP guard, because one responder lists about 20 hashes while the guard allows one hinted send per IP per minute.

Everything is behind one switch that is off by default. No cap, timeout or rate changes:
- the 32 owned requests and the 3-RPC in-flight budget;
- the 64-query cap per request, `LOOKUP_DEADLINE` (25 s), the query rate and the outstanding cap;
- the admission permits, and pool fairness for keys without a hint;
- CR-1a-H's guard: its 60 s interval and its 20,000-IP capacity.

## The switch
- **`dht.hint_first`, a bool, off by default.** Set it in `config.toml` (`[dht] hint_first = true`) or with `LODESTAR_DHT_HINT_FIRST=true`, read at start. A bad value is a config error (`hint_first_is_off_unless_asked_for`). It isn't in the Settings UI and needs a restart.
- **Off is today's behaviour exactly.**
  - The actor allocates hint state only if `dht.responder_hints` is on, as before. Harvests carry `hint: false`, and nothing is marked or counted (`hint_first_off_marks_nothing`).
  - The pool's T1 queues stay empty, so it serves its backlog and admission queues exactly as before. A mark from any source is ignored and counted (`only_a_trusted_source_with_the_switch_on_buys_t1`).
  - The CR-SIM goldens are byte-identical. The new series read 0, as new zero counters.

## How the switches combine
| `responder_hints` | `hint_first` | Behaviour |
|---|---|---|
| off | off | Today's. No hint state. |
| on | off | CR-1a-H exactly as before: every new sample hash's responder is kept as a hint, and nothing is marked. |
| off | on | The hint table and guard are allocated. Only the one marked hash per reply keeps a hint; its request takes it as CR-1a-H's does. |
| on | on | CR-1a-H keeps every hint as before. The reply's first new hash is also marked (`with_responder_hints_every_hint_is_kept_and_one_marked`). |

Hint-first reuses CR-1a-H's capture, its endpoint checks (at capture and again at send), its guard and its dispatch, and weakens none of them.

## The contract as built
- **Marking at capture** (`crates/dht/src/node/actor.rs`, `hints.rs`).
  - For each `sample_infohashes` reply, only the first hash new to the seen filter is a candidate.
  - It's marked only if all of these hold:
    - the responder passes the public-endpoint and family checks;
    - the per-IP guard doesn't restrict the responder's canonical IP;
    - no other T1 hint for that IP is outstanding, meaning marked but neither taken nor older than `HINT_TTL`;
    - fewer than `T1_RESPONDERS` IPs hold one;
    - the hint table has room.
  - A paused crawler decides nothing.
  - The harvest goes to the pool with `hint: true`, and its hint is kept. The mark ends when the request takes the hint, or at the TTL (`one_marked_hash_per_responder`, `one_t1_hash_per_responder_until_taken_or_expired`, `t1_marks_are_bounded`).
- **The flag's path:** `Harvest.hint` (`crates/dht`), then `Found.hint` (`crates/sources/src/plugin.rs`, false from `Found::key`), mapped in `crates/pipeline/src/dht.rs`.
- **Trust (the runner's addition).**
  - The pool honours `Found.hint` only with `PoolConfig::hint_first` on, and only from a source in `PoolConfig::hint_sources`.
  - The pipeline fills `hint_sources` with the DHT plugin's own source id, matched by its plugin id (`trusted_for_hints` in `crates/pipeline/src/lib.rs`, test `only_the_dht_plugin_is_trusted_for_hints`), only with the switch on.
  - **The runner's call:** no other plugin is trusted, not even a later peer finder. Trusting one is a deliberate edit to `trusted_for_hints`, with its own review.
  - Any other mark is ignored and counted. So is a mark on a private or interactive key: a private key never goes to public peer finding, and an interactive one is T0 already.
- **The T1 tier at the pool** (`crates/sources/src/pool.rs`, `pool/admission.rs`).
  - A hinted key waits for its first lookup in T1, between interactive and background.
    - Under coordinated admission, T1 is a third queue in `Admission`. Its grants take the same permits as any class, counted as `class="hinted"`.
    - Without coordinated admission, T1 is a backlog beside `lookup_backlog`.
  - **Newest first (this slice's refinement, approved by the runner).** A hint is only worth its first minute, so T1 serves its newest key first. When T1 is full (`T1_WAITING`), its oldest key leaves for its ordinary queue, and the newcomer takes T1.
    - The first screen ran first-in, first-out. Marking runs at about 24 hashes a second against about 1.5 admitted lookups a second, so a full FIFO T1 served keys that had waited about 170 s, long past `HINT_TTL`: about 15k of 17k marked hints still expired unused.
  - **One place per key (the runner's addition).** A key waits in T1 or in its ordinary queue, never both. A key that leaves T1, full or aged, moves to its ordinary queue (`a_full_t1_sends_hinted_keys_to_their_ordinary_queue`).
    - **A user's interest ends a key's hint** and gives it its interest place.
      - Under coordinated admission, that's CR-ADM's own rule: the waiting key gets an interactive copy, and whichever copy is granted first makes the other stale (`interest_in_a_hinted_waiter_serves_it_first`).
      - On the plain backlog, its T1 copy goes stale at once and the key moves to the backlog's front, its one place (`interest_in_a_hinted_backlog_key_serves_it_first`). The first review found that this path left the key in no queue at all, so the key a user waited on was never looked up. That's fixed, and the test is the reviewer's probe.
  - **Ageing.** A T1 waiter older than `T1_MAX_WAIT` (60 s, the DHT's `HINT_TTL`) goes to the back of its ordinary queue and its hint ends, since the DHT's hint has expired. This holds in both queues (`an_aged_t1_key_goes_behind_background`, `an_aged_backlog_t1_key_goes_behind_the_backlog`, `an_aged_hinted_key_never_returns_to_t1`).
  - Only a key's first lookup can use T1. Retries and requeues are ordinary.
- **No starvation.**
  - Interactive goes first, but never more than `INTERACTIVE_BURST` (4) in a row while a lower class waits.
  - T1 goes next, never more than `T1_BURST` in a row while background waits.
  - So background keeps at least 1 in 5 of the grants interactive doesn't take, and the lower classes keep at least 1 in 5 of all grants (`t1_goes_between_and_starves_no_one`, `t1_goes_first_with_the_same_permits_and_background_keeps_moving`, `the_plain_backlog_serves_t1_first_and_keeps_background_moving`).
  - **By design,** while interactive is saturated and T1 isn't empty, background's share falls from 1 in 5 grants to 1 in 25: interactive takes 4 of every 5, and background 1 in 5 of the rest. Nothing starves.
- **Dispatch** is CR-1a-H's, unchanged: the request takes its hash's hint and sends one direct `get_peers`, with every check repeated at send, then ordinary routing drives the legs.

## A known limit (logged by the runner)
- One /24 can hold up to 256 outstanding T1 marks, one per IP, about 15% of T1 at the measured marking rate. That buys only queue order, for one hash per IP at a time.
- It's bounded by:
  - the existing per-block sample share (`sample_sources`), which limits how many of a block's hashes reach the pool;
  - `T1_RESPONDERS`, `T1_WAITING` and the burst rule.
- A per-block T1 cap could be a later refinement; this slice leaves it as is.

## Finishing on values (the runner's refinement of item 4, §2.26, flagged under §2.19)
- Item 4 says "the request finishes if it returns values". As built, a hinted reply's values count toward CR-LOOKUP-END's end condition exactly like any reply's: `Actor::has_enough_values`, with X-sec-S1-18's per-source rule.
- Hint-first adds no path where one responder ends a request, and no exception for the hinted responder. If its values alone can't end the request, the request carries on through ordinary routing within its unchanged deadline (`a_hinted_reply_alone_never_ends_a_request`).
- With `dht.lookup_end` off, no request ends on values at all, as before.

## Candidate defaults
These are named constants, not config. The fresh reviewer checks them, and the runner flags them to the owner (§2.19). None was tuned for the screen.

| Constant | Value | Where |
|---|---|---|
| `T1_RESPONDERS` | 20,000 canonical IPs with an outstanding T1 hint (the guard's capacity) | `crates/dht/src/node/hints.rs` |
| `T1_WAITING` | 256 live T1 keys per queue; past it the oldest leaves for its ordinary queue | `crates/sources/src/pool/admission.rs` |
| `T1_BURST` | 4 T1 grants in a row while background waits (as `INTERACTIVE_BURST`) | `crates/sources/src/pool/admission.rs` |
| `T1_MAX_WAIT` | 60 s, the DHT's `HINT_TTL` | `crates/sources/src/pool/admission.rs` |

Memory with the switch on:
- the T1 map: at most 20,000 entries of an IP, a hash and an `Instant`, about 1.5 MB full;
- the T1 queues: at most 2 × 256 small entries;
- one `bool` per pool entry, which fits in the entry's existing padding (`diagnostic_entry_storage_is_bounded` passes unchanged).

## Files outside the brief's list (approved by the runner)
These carry the `hint` flag, or wire and export the switch:
- `crates/dht/src/node/mod.rs`;
- `crates/dht/tests/{harvest,network,scrape}.rs`, one `hint_first: false` or `hint: false` line each;
- `crates/pipeline/src/dht.rs` and `crates/pipeline/src/lib.rs`;
- `crates/sources/src/plugin.rs`;
- `crates/lodestar/src/metrics.rs`, which adds the series, plus its render test;
- `crates/sources/tests/hint_first.rs`, a new test file.

## CR-SIM screen
`cargo test --release -p lodestar-crawler-sim --test sim hint_first_screen -- --ignored --nocapture`.

**Setup.**
- The calibrated `live-lookup` scenario, `SCREEN_SEEDS` 1–8, off against on, with `samplers_hold` on in both arms, as CR-1a-H's screen ran.
- Three variants, because a stage-3 control stacks the earlier winners and none is decided yet:
  - as is;
  - CR-ADM on in both arms;
  - CR-ADM and CR-LOOKUP-END on in both arms.
- **Measures.** "Stored" is the post-warmup rate per minute. "Spread" is the off arm's range across the eight seeds, the noise floor. Per slot-second is stored per second ÷ `L`, and per query is stored ÷ DHT queries.
- **Hint counts** are per-seed means for the on arm.

**Newest-first (as built):**

| Variant | Stored off → on | Change | Off spread | Sign test | Per slot-s off → on (rose) | Per query off → on (rose) | `W` s | `q` | `y` |
|---|---|---|---|---|---|---|---|---|---|
| as is | 13.31 → 13.76 | +3.4% | 25.9% | Inconclusive (5+/3−) | 0.00719 → 0.00744 (5/8) | 0.002862 → 0.002969 (5/8) | 23.17 → 23.42 | 24.5 → 24.7 | 0.167 → 0.174 |
| CR-ADM in both | 16.03 → 16.82 | +4.9% | 20.2% | Inconclusive (7+/1−) | 0.00865 → 0.00909 (7/8) | 0.003417 → 0.003563 (7/8) | 22.74 → 22.72 | 24.2 → 24.5 | 0.197 → 0.206 |
| CR-ADM and lookup_end in both | 24.83 → 26.26 | +5.8% | 21.9% | Inconclusive (5+/2−/1 tie) | 0.01337 → 0.01416 (6/8) | 0.004899 → 0.005162 (5/8) | 14.83 → 14.55 | 18.6 → 18.3 | 0.198 → 0.206 |

**First-in, first-out (the first run, before the refinement):**

| Variant | Stored off → on | Change | Off spread | Sign test | Per slot-s off → on (rose) | Per query off → on (rose) | Hinted queries (on) |
|---|---|---|---|---|---|---|---|
| as is | 13.31 → 13.76 | +3.4% | 25.9% | Inconclusive (5+/3−) | 0.00719 → 0.00744 (5/8) | 0.002862 → 0.002969 (5/8) | 355 |
| CR-ADM in both | 16.03 → 17.07 | +6.5% | 20.2% | Inconclusive (7+/1−) | 0.00865 → 0.00922 (7/8) | 0.003417 → 0.003634 (6/8) | 460 |
| CR-ADM and lookup_end in both | 24.83 → 27.08 | +9.1% | 21.9% | Inconclusive (6+/2−) | 0.01337 → 0.01460 (6/8) | 0.004899 → 0.005329 (6/8) | 1,218 |

The two orders' "as is" rows are identical: without CR-ADM, T1 never holds a key (see below). The off arms are the same runs in both.

**Hint counts for newest-first's on arm:**

| Variant | Marked | Expired unused | Hinted queries | Useful |
|---|---|---|---|---|
| as is | 17,406 | 15,730 | 355 | 248 |
| CR-ADM in both | 17,352 | 15,312 | 764 | 531 |
| CR-ADM and lookup_end in both | 17,549 | 15,110 | 1,210 | 792 |

- **No variant beats the noise floor in either order.** Every change is inside the off arm's spread, and every verdict is the sign test's: Inconclusive.
  - The keep rule stands as briefed: output per slot-second and per query must both rise.
  - On the means it holds in every variant. Per seed it holds most strongly with CR-ADM in both arms.
  - A screen never replaces the live run (stage 3, stacked on the winners so far).
- **Without CR-ADM, T1 does little.**
  - With coordinated admission off, the pool hands each request straight to the actor, so keys wait in the actor's own FIFO queue, not the pool's. T1 orders almost nothing.
  - With CR-ADM, keys wait for a permit in the pool, where T1 acts. Hinted queries roughly double, and with CR-LOOKUP-END too they more than triple.
  - This matters for the live A/B's design: hint-first's arm needs CR-ADM in its control.
- **Why newest-first.** In the FIFO run, about 15k of the 17k hashes marked per seed expired unused. Marking ran at about 24 a second against about 1.5 admitted lookups a second, so a full FIFO T1 served keys that had waited about 170 s, past `HINT_TTL`.
  - With CR-ADM alone, newest-first sends about two-thirds more hinted queries (764 against 460). With CR-LOOKUP-END too, the two orders send about the same number.
  - The two orders' stored rates are within each other's noise. Newest-first is kept as the sounder rule (approved by the runner).
- **Most marks still expire.** Discovery outruns lookup admission by more than 10×, so no ordering serves every mark inside `HINT_TTL`.
- **The sim's limits apply** (CR-SIM README): IPv4 only, approximated routing, and stored means "reached the writer". `samplers_hold` is an optimistic model of a responder. A screen never replaces a live run.

## Tests
- `crates/dht/src/node/hints.rs`: `one_t1_hash_per_responder_until_taken_or_expired`, `taking_another_keys_hint_keeps_the_mark`, `t1_marks_are_bounded`.
- `crates/dht/src/node/hint_tests.rs`, on tokio's paused clock against the real actor:
  - `hint_first_off_marks_nothing`;
  - `one_marked_hash_per_responder`;
  - `with_responder_hints_every_hint_is_kept_and_one_marked`;
  - `rejected_or_paused_responders_mark_nothing`;
  - `a_hinted_reply_alone_never_ends_a_request`;
  - the property test `hostile_hinted_replies_end_nothing_early_and_stay_bounded`. Its replies are empty, huge, duplicate, private or port-0 values, spoofed replies, and address rotation within one /24. None ends a request on values or lets a junk endpoint in, and the hint state stays bounded.
- `crates/sources/src/pool/admission.rs`: `t1_goes_between_and_starves_no_one`, `t1_is_bounded_and_ages_into_background`.
- `crates/sources/tests/hint_first.rs`: eleven pool fixtures, those named above and `private_and_interactive_keys_never_take_t1`.
- **Mutations.** 16 checked. The 12 from the first round all die, as do the first review's three survivors: interest stranding a hinted key, backlog ageing, and the private and interactive exclusion. One mutation survives as equivalent: not clearing the hint when admission ages a key. That key's hint is cleared anyway when its lookup is sent, before which it can't take T1 again. The clear is kept so the flag means what it says.
- `crates/lodestar/src/config.rs`: `hint_first_is_off_unless_asked_for`. `crates/lodestar/src/metrics.rs`: the new families render each label with its own value.
- CR-SIM: `hint_first_marks_and_sends_deterministically` (runs in CI), and the ignored `hint_first_screen`.
