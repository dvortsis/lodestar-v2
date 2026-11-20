# runner-records-r5 decisions

The runner's records PR after #970 (path to 100, r5 and r5.1). It records the owner's decisions from 2026-11-14 to 2026-11-20 in `docs/owner-decisions.md` §2.72 to §2.81, and the runner's own calls below. Each call is flagged to the owner, who can veto it (§2.19).

## Switch names (runner, 2026-11-20)
- **The four new crawler switches have fixed names,** so their briefs, CR-COL-5's switch sets and the A/B records all use the same ones:

  | Slice | Config key | Environment variable |
  |---|---|---|
  | CR-LOOKUP-END | `dht.lookup_end` | `LODESTAR_DHT_LOOKUP_END` |
  | CR-1c | `dht.preset_slots` | `LODESTAR_DHT_PRESET_SLOTS` |
  | CR-HINT-FIRST | `dht.hint_first` | `LODESTAR_DHT_HINT_FIRST` |
  | X-sampler-cooldown-split | `dht.sampler_cooldown_split` | `LODESTAR_DHT_SAMPLER_COOLDOWN_SPLIT` |

  Each is off by default (`docs/decisions/2026-11-20-path-to-100.md`, "Not changed"). `runner/slices.yaml` carries them in the slices' titles or notes.

## The API lane's reviews (runner; flagged to the owner)
- **The runner accepted the API lane's independent per-PR reviews as the merge rule's fresh review** for #940 (ENR-mb), #966 (X-worker-service-log) and the five PRs combined in #971 (#947 ENR-tvdb, #955 ENR-discogs, #960 ENR-srrdb, #965 ENR-predb, #967 ENR-anidb).
  - **The conditions, checked for each:** the review is a fresh read-only session posted on the PR itself with a MERGE verdict; the head it reviewed is the head that merged; CI is green.
  - **Why:** the owner approved the same kind of paid independent review under §2.67, and the lane's reviews were bought from the same credit (§2.75). Reviewing them again in a runner session would have spent review bandwidth on work already reviewed to the same bar.
  - **Not covered:** a CHANGES verdict, an unreviewed PR or a head that moved after the review goes through the runner's normal review.

## Follow-ups from #970's review
- **The lookup shortlist weakness is named by id only.** `docs/reviews/2026-11-16-dht-additional-methods.md` method 7 gave a live low-severity weakness step by step. It's now finding S1-16 (low; surface 1, the network and the crawler, `docs/security/v1/README.md` §1), for a hardening slice, with its detail held (§5).
- **Finding 14 of the crawler survey is the proposal's, not Lodestar's.** `docs/reviews/2026-11-16-dht-crawler-survey.md` now says that main's network policy (`public_ip` in `crates/network-policy/src/lib.rs`) already rejects those blocks.
- **The screens wait for the calibrated scenario.** CR-LOOKUP-END, CR-HINT-FIRST, CR-1c, X-sampler-cooldown-split and CR-AB-LC depend on X-crawler-sim-calibrate. Their screens use its scenario, and it removes their `owns` overlap on `crates/crawler-sim`.
- **X-sampler-cooldown-split owns the files that exist.** Its `owns` named two paths that don't exist. It now owns the sampler, the restart proof, `crates/dht/src/restart.rs` and `restart/**`, `crates/dht/src/node/mod.rs` (where the DHT config lives), `node/actor.rs` and `crates/lodestar/src/config.rs`. It depends on CR-LEDGER, which also edits `actor.rs`.
- **CR-AB-LC's notes match r5.1:** the image is built during stage 1 under A3 (not A4), and the third mechanism check, "the query rate stays within the preset", is listed.
- **CR-1c's role for CR-AB-ADM is clarified.** CR-AB-LC decides CR-1c together with CR-LOOKUP-END (A1); CR-AB-ADM's result only informs CR-1c's default slot count. The change is in `docs/decisions/2026-11-20-path-to-100.md`, `docs/build-plan.md` §0.5 and CR-1c's notes. No decision changes.
- **The calendar's caveats are stated.** `docs/build-plan.md` now says that a possible CR-5 arm (about 3 days) isn't in the 16.5–28.5 host-day total, and that 28.5 host-days with the 30 % retry budget is about 5.3 weeks. "The r5 figures" became "The r5.1 figures".
- **CR-SPEC rule 6.1 points to its amendment** (`docs/decisions/2026-11-20-path-to-100-cr-spec.md`).
- **The survey evidence's lock file is renamed `Cargo.lock.txt`,** so the repository doesn't carry a second Cargo lock file outside the workspace. `verify-proposal.ps1` copies it into its temporary crate as `Cargo.lock`.
- **The outside scheduler code goes to the owner.** `docs/reviews/dht-survey-evidence/original-scheduler.rs.txt` is outside code with no recorded source. Whether it can stay in the repository is an owner-queue item.
