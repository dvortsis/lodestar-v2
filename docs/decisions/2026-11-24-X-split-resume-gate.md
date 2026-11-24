# X-split-resume-gate decisions

With `dht.sampler_cooldown_split` on, a clean restart after the sampler's table had filled sampled no one for up to about 6 h. Owner §2.77 rules that out: "No sampling wait after any start, and no blanket gate carried across a restart." This slice removes that hold by keeping the spill filter itself in the restart snapshot. It supersedes the "Restart (CR-0d)" section of `docs/decisions/2026-11-20-X-sampler-cooldown-split.md`.

## The cause
- **The filter couldn't be listed, so its cooldowns became a blanket gate.** On commit, `Proof::commit` took `Sampler::spill_end` (the latest end the filter might hold, capped at now + H) and folded it into the snapshot's gate. The next run then held every sample until that gate passed. In steady state the filter holds cooldowns ending across the next 6 h, so the gate sat near its 6 h cap.

## The fix
- **The snapshot keeps the filter, slot by slot, and the next run rebuilds it.** No gate is added for it.
  - **Commit:** `Spill::export` writes every live slot with its block, its tag and the whole minutes left until its end, rounded up and capped at H, as the listed entries are (WK-10e). The filter's two SipHash keys go with it.
  - **Start:** seeding maps each slot's end onto this run's clock as it maps a listed entry's (`restart::seed`, `mono0` read after `wall0`), dropping ended ones. `Spill::restore` puts each slot back in its block with its tag, ending at the first whole minute at or after its seeded end. Same keys, same blocks, same tags: every restored cooldown is found where the next lookup looks, and its shares count as before.
  - **Whatever the switch says in the next run.** With it off, the restored filter only refuses its restored cooldowns and holds their shares until they end, and nothing new moves in. Turning the switch off across a restart can't free a cooled node early.
- **Never shorter:** each step only rounds an end up (export from the capture's monotonic reading, `at` read after it; seeding as for entries; restore to the next minute). The cap at H is the one the old gate and the listed entries already had. Tested by `a_spilled_cooldown_never_ends_early_and_ends_within_a_minute`, `seeding_a_spill_section_never_moves_an_end_earlier`, `capture_then_seed_never_early_for_any_lags` (now with a spilled slot), `the_filter_survives_a_restart_whole`, and the proptest `no_node_is_sampled_in_cooldown`, whose restart step now goes through capture, encode, decode, seed and restore and asserts that no hold is needed.
- **A node whose record was lost is still never free.** If the section can't be rebuilt (a filter already in use; a checked snapshot can't make a block overflow), `restore_spill` returns the latest restored end and the proof gates every sample until it (`a_filter_that_cant_be_rebuilt_holds_until_its_last_end`): the old behaviour, never a shorter one. A snapshot that fails any check is corrupt and the run takes the spec's full suppression, as before. A marker or snapshot that is missing or stale is handled exactly as CR-0d specifies.

## The bound
- **A restart from a spilled state samples at once:** no hold is carried for the filter, so the first sample goes out within the same bound as with the split off. The simulator states it as 60 s (`restart_from_a_spilled_state_samples_at_once`: leg 1 spills into a 200-place table, leg 2 resumes, samples within 60 s, and samples no node before its restored cooldown ends, listed or spilled).
- **No cooldown is shortened,** as above.
- **One sim check is relaxed with the split on, and only then:** with it off, every node whose restored cooldown ends during leg 2 must be sampled again in leg 2. A spilled node has left the table, so once its cooldown ends it's sampled when a lookup offers it again, as within a run; the check asks for 99 % (927 of 929 in the run, the two missed due about 4 minutes before its end). The split-off check and its golden are unchanged.

## The format (for the runner's review)
- **The snapshot container goes to version 2 only when something is spilled.** With nothing spilled it's written byte for byte as CR-0d wrote it: version 1, its nine keys (`version_1_is_unchanged_and_still_resumes_with_its_gate`). The split-off path never spills, so its files, goldens and tests are unchanged. The marker and the witness stay version 1, and a version-2 marker or witness is rejected.
- **Version 2 adds two keys to the strict bencode body:**
  - `spill_key`: 16 bytes, the filter's two SipHash keys, little endian.
  - `spill`: for each of the 2^17 blocks in order, a count byte (0–64), then that many 4-byte big-endian words, each the slot's 20-bit tag (high bits, top byte never 0) and its minutes left (1 to ceil(horizon / 60 s)).
  - Decoding is strict, as for the rest of the body: both keys in version 2 and neither in version 1, exact length, no empty section, no block over 64, no bad tag or minutes (`spill_rules`). Capture refuses a section decode would reject (`CaptureError::Spill`).
- **Migration:** a version-1 snapshot written by an older binary still decodes and resumes, and its gate, which then held any spilled cooldowns, is honoured as before. An older binary reading a version-2 snapshot rejects it as an unknown version and takes the full suppression, never a shorter wait.
- **The hash is pinned.** The filter used `RandomState`, whose algorithm may change between releases; it now hashes with SipHash-2-4 (std's `SipHasher`, deprecated only in favour of `DefaultHasher`) over a fixed byte encoding of each value, checked against the reference vectors (`siphash_is_the_reference_function`). Keys are still random per process (fixed under the `testing` feature, as the seen filter's are) and carried across a restart in the snapshot.
- **Size:** 128 KiB plus 4 bytes a live slot, so at most about 32.1 MiB (`MAX_SNAPSHOT_BODY` raised to match; `the_largest_spill_section_decodes`).

## §2.77 and the split
- **This clears §2.77's "no blanket gate carried across a restart" for the split:** a clean restart from a spilled state no longer carries one. The fallback gate above runs only when the snapshot's filter can't be rebuilt, which a valid snapshot can't cause.
- **The start suppression itself is CR-0w's, not this slice's.** The code still suppresses a first start or an unusable restart for 6 h, and `dht.safe_restart` is still off by default; both wait on CR-0w.

## §2.19 refinements, for the runner to flag to the owner
- **The filter's hash keys are written to the data directory** in the snapshot. They only spread addresses across blocks (a defence against crafted collisions); anyone who can read the data directory can already read the crawler's state.
- **Restart memory and disk:** with the filter full, the snapshot is up to about 32 MiB on disk, and a start briefly holds the decoded section, its seeded copy and the rebuilt 32 MiB filter. Commit scans the 8M slots once. With the split off, none of this happens.
- **The cap at H for spilled ends** is kept, as for listed entries (WK-10e).

## Follow-ups (not owned here)
- **`docs/metrics.md`, `dht_sample_full_seconds_total`:** its line "with the switch on, a clean restart after the table filled is gated for up to about 6 h" is no longer true and should be dropped.
- **`fuzz/fuzz_targets/dht_restart.rs`** seals with `restart::VERSION`, so it only reaches version-1 snapshots. It should also seal at `SNAPSHOT_SPILL_VERSION`.
