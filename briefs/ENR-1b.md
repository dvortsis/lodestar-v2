# ENR-1b: close ENR-1's review follow-ups before the enrichment runtime is wired in
**Model:** Opus (content-safety core). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-05T12:50Z. The same session as ENR-1. Ships in v0.5.2.

## Why
ENR-1 (#480, merged 77cc0980) built the adult-enrichment runtime. It stays inert until the host wires it in, which happens with SRC-11c and ENR-7. The #480 reviews left items that must be closed **before** that wiring, because each is a content-safety or correctness hole once the runtime is live. The spec is `docs/decisions/2026-11-05-ENR-1.md`.

## Do (branch `slice/ENR-1b` from `origin/main`)
1. **The enrichment gate in the writer:** add a writer `Ctx` field so that only `Kind::Enrichment` sources may attach adult sections. An always-adult source (X-source-adult) that attaches a section must not take the matches path that skips `touched` and `adult`, because that bypasses the always-adult mask. Close it with the gate, or by keeping always-adult sources on the normal path. Add a test that an always-adult source's section can't skip the mask.
2. **The generation race:** at `crates/pipeline/src/enrich.rs:~325`, `apply_matches` reads the generation after the writer snapshot. Pass the generation or `Handle` through, and raise `reenrich` for deferred matches. Add a test that forces the interleaving with a barrier, not a sleep.
3. **Partial-upgrade refetch:** a refetch that has an adult claim is marked for re-enrichment.
4. **Intake counts:** in `pipeline/src/lib.rs`, enrichment matches (strategy 202) are not counted as HARVESTED.
5. **The Display/Name query form** returns `NoQuery` when the torrent name alone is ambiguous or uninformative (`named.title.ambiguous` or `is_uninformative`, e.g. "New folder"). Such a query can never match, costs budget, and tells the service an adult item was viewed. This is the runner's call under §2.33, stricter than Q2; the runner flags it to the owner. Note in the decisions file that a single-file torrent's name is its file name, which is allowed because it is the display name.
6. **Tests:**
   - the backfill after a Pending refusal makes the item eligible again;
   - each item above has a test that fails without its fix.

   No fixed sleeps.
7. **Out of scope:** the host wiring of `AdultEnrichSource`, Switches, StoreState and Interest belongs with the SRC-11c and ENR-7 wiring. So does ENR-7's rate limit on user-interest lookups.

## Own
`crates/pipeline/src/{adult_enrich.rs,enrich.rs,unverified.rs}`, the intake line in `crates/pipeline/src/lib.rs`, `crates/pipeline/tests/adult_enrich.rs`, `crates/sources/src/enrich.rs`, and `docs/decisions/2026-11-05-ENR-1b.md`.

**Shared, keep it minimal:** in `crates/pipeline/src/writer.rs`, only the `Ctx` field and its gate. RP-4 (running) owns `writer.rs` for SP-3. Tell the runner the exact hunk in your PR, and whichever PR merges later brings `main` in.

**Don't touch:** `crates/sources-plugins/src/torrent_cache.rs`, which #491 owns.

## Done when
- `cargo fmt --check`;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- `cargo test --workspace`;
- `/security-review` on the diff.

Then open the PR "ENR-1b: Enrichment gate, generation race and NoQuery before the runtime is wired", label `full-ci`. Add your `runner/slices.yaml` entry and placement. Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Never merge. Push once per round.
