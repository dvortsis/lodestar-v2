# X-purge-rederive: a purge re-derives what the purged source decided; ENR-G follow-ups

**Runner brief, 2026-11-24T20:40Z.** Core, Opus, content-safety. Target < 600 changed lines.
- **Branch:** `slice/X-purge-rederive` from fresh `origin/main` (ENR-G #1117 merged as 182d49fe).
- **PR title:** `X-purge-rederive: A purge re-derives what the purged source decided`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do. **/security-review before you ask for review**; the runner's fresh Opus review plus a security pass gate the merge.
- Add this slice's own `runner/slices.yaml` entry (kind core, model opus, the owns below, notes "/security-review; from ENR-G review rv-1117/rv-1117b"), and run `cd tools/status-connector && npm test`.

## Why
ENR-G (#1117) found and fixed purge holes, and its review left three items in the same file (reviews/rv-1117.md on runner-state).

## Do
1. **Purge re-derives the adult decision.** In `crates/store/src/unverified.rs`, the purge's Torrents stage (`Fate::Keep`) clears the source's bit without re-deriving. A torrent that only an always-adult source (for example Sukebei) made Adult can stay Adult after that source is purged. Re-derive the classification for kept torrents whose adult (or other) decision came from the purged source, through the existing classify path, and bump `class_gen` so cached answers drop. Test: a torrent made Adult only by an always-adult source is no longer Adult after the purge. A torrent the child-safety core (§20.5) blocks or flags stays blocked or flagged whatever is purged; test that too. Never loosen the locked floor.
2. **Purge Claims marks only on change.** In the Claims stage, set `marked` only when the UPDATE matched rows, so a no-op purge doesn't bump `class_gen`. Test it.
3. **Consent at write, under race.** A test that ingest stores through `put_claims_at_consented`: a hook withdraws consent on another connection between the early check and the write, and nothing is stored.
4. **Plain feed purges (decide and record):** measure what re-deriving costs when a non-enrichment feed is purged (a whole-catalogue reindex?). If it's cheap or can be scoped to the affected torrents, do it. If not, keep today's behaviour and record the numbers and the reason in your decisions file. Either way, say which in the PR; the runner flags it to the owner (§2.19).

If a fix needs files outside your owns, or more than ~150 lines beyond the above, stop and tell the runner first.

## Owns
`crates/store/src/unverified.rs`, `crates/store/tests/unverified.rs`, `crates/pipeline/tests/**`, `docs/decisions/2026-11-24-X-purge-rederive.md`, and this slice's `runner/slices.yaml` entry.

## Done
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. No fixed sleeps. Report `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green. Push once per round of fixes.

Landing lane: ENR-terms-api and X-split-resume-gate (Rust) push next. If either enters the lane, hold Rust pushes until it merges.

No AWS or cloud calls, no new accounts, no spending, no force-push. Held security detail stays off every branch.
