# CR-1a-H: responder hints, behind their own switch
**Model:** Opus: the DHT actor, untrusted routing input, and rate limits. Runner brief, 2026-10-30. Lane C, the crawler program's second candidate (`docs/build-plan.md` §0, "Order"). Its dependency, CR-ADM (#228), has merged.

## Why
H1 says lookup latency × the 32-request cap limits the crawler. A BEP 51 `sample_infohashes` responder just told us about the hash, so it's likely to know peers or close nodes for it. Sending one direct `get_peers` to that responder before ordinary routing may shorten lookups.

This slice builds the candidate so it can be measured alone. It does **not** decide whether hints help; the CR-SIM screen and the live A/B decide that.

## Read first
- `docs/plan/2026-10-27-streaming-and-hints.md`: "Responder hints are a separate capability" and "Prepared regression cases". **That section is the contract.**
- `docs/decisions/2026-10-30-CR-ADM.md`: the switch pattern, the pool/actor admission boundary, and the CR-SIM `Switches` screen. Copy its shape.
- `runner/standards.md`.

## Do
1. **The switch: `dht.responder_hints`, a bool, off by default.**
   - Set it in `config.toml` (`[dht] responder_hints = true`) or with `LODESTAR_DHT_RESPONDER_HINTS=true`, read at start. A bad value is a config error.
   - Add a config test, `responder_hints_is_off_unless_asked_for`.
   - With the switch off, behaviour is byte-identical to today: no hint state is allocated and nothing is sent differently. Prove it with a test, and keep the existing CR-SIM goldens unchanged.
2. **Hint capture.** When a `sample_infohashes` reply yields a hash that becomes a pending key, remember the responder's normalized endpoint and the monotonic receipt time.
   - Keep at most one hint per address family per pending key.
   - Never persist hints.
   - Never refresh a hint on an unrelated rediscovery, and never accept a timestamp from outside.
   - Drop the hint on eviction or terminal removal.
   - Run the public-endpoint checks (existing policy helpers: port 0, private or reserved addresses, unsupported encodings) before inserting the hint, and again before sending.
3. **Hinted dispatch.** A fresh hint (age < TTL; the boundary is exactly as the spec's "Hint expiry" case states) seeds a lookup with one direct `get_peers` to the responder, then ordinary routing.
   - Allow one hinted attempt per family, inside the existing logical lookup deadline.
   - A hint that is expired, invalid, private, an unsupported family or throttled falls back to ordinary routing at once.
   - A send failure doesn't restart the deadline.
   - A hint is never fed to the fetcher, never rewrites XOR distance, and never counts as verified routing knowledge or corroboration.
   - It shares the existing actor admission (including CR-ADM's permits when that switch is on), the outstanding-RPC budget, the family budget and the deadline. It reserves no extra sockets and adds no separate logical attempts.
   - Re-check the consent and private-source policy at dispatch.
4. **The per-IP guard.** It works on the canonical IP: across ports, with IPv4-mapped IPv6 folded to IPv4.
   - Allow one hinted query in flight per IP, and a 60 s not-before interval from each permitted send.
   - Cap the guard at 20,000 IP entries. Never evict an unexpired restriction to make room; when the guard is full, skip the hint.
   - **Ordinary routing checks a retained restriction before sending to that same IP,** so the fallback can't bypass it. Peers with no restriction keep today's policy.
   - Bounded memory, with pruning on expiry, proven by a test.
5. **Defaults.** Use the spec's proposed values: TTL 60 s, a 60 s not-before interval, 20,000 entries, one hinted attempt per family. Make them named constants, not config, and record them in your decisions file under "candidate defaults". The spec says they're reviewed before implementation, so the fresh reviewer checks them and the runner flags them to the owner (§2.19). Don't tune them to win the screen.
6. **Metrics.** Add counters with no infohash or endpoint labels: `dht_hint_queries_total`, `dht_hint_fallbacks_total{reason}` (`expired`, `invalid`, `private`, `family`, `throttled`, `guard_full`, `send_failed`), and whether the hinted query was the one that yielded peers (`dht_hint_useful_total` or your best name). Render them in `crates/lodestar/src/metrics.rs` and document them in `docs/metrics.md`.
7. **Tests.** Implement the spec's hint cases with paused clocks and condition deadlines of at least 10 s, and no fixed sleeps:
   - "Hint expiry", "IP rotation", "Guard saturation" and "Private key";
   - the switch-off identity;
   - fallback when the responder never answers, which must stay inside the deadline;
   - the in-flight-per-IP limit.

   Hints come from the network, so add a property test over arbitrary responder endpoints (mapped forms, port 0, reserved ranges).
8. **The CR-SIM screen.** Add `Switches { responder_hints }`, then an ignored screen like `coordinated_admission_screen`, with the off and on arms over seeds 1..=8. Report steady stored/min, lookup duration and the hint counters against the measured noise floor. The scripted net's sampled responders must answer `get_peers` for the hashes they listed, at the scenario's latency classes. If they can't yet, add that to the net. Put the screen's table in the decisions file. If hints don't beat the noise floor, say so plainly; that's a valid result.
9. **Decisions file:** `docs/decisions/2026-10-30-CR-1a-H.md` (or the date you open the PR), covering the contract as built, the defaults, the screen result and anything you refined.

## Own
`crates/dht/**`, `crates/crawler-sim/**`, the `responder_hints` lines in `crates/lodestar/src/config.rs`, `crates/lodestar/src/metrics.rs`, `docs/metrics.md` and your decisions file. Pool changes in `crates/sources` only if admission truly needs them; say why in the PR. Keep `crates/cluster` and its chaos and transport tests green, with the §19 worker protocol unchanged.

## Size
Aim for ≲ 1,500 changed lines. If it grows past that, split it:
- **part 1:** the switch, capture, the guard, dispatch, metrics and unit and property tests;
- **part 2:** the CR-SIM net support and the screen.

## Done when
- fmt, workspace clippy `-D warnings` and `cargo test --workspace` pass;
- `cargo test -p lodestar --bins` passes (the logging targets and config);
- the production-config clippy and the cargo-tree guard pass;
- the CR-SIM goldens are byte-identical;
- the screen runs deterministically.

Open the PR, follow `.claude/skills/steward/SKILL.md`, then message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head SHA and the screen's one-line result. Never merge. Work frugally: the account is under a usage warning. Push once per round of fixes.
