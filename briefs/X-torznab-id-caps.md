# X-torznab-id-caps: Torznab caps advertise ID parameters only when they can be answered

**Runner brief, 2026-11-27T07:40Z.** Core, Opus (an API contract the *arr apps act on). Target < 600 changed lines. `/security-review` before you report ready.
- **Branch:** `slice/X-torznab-id-caps` from fresh `origin/main` (with #1130, runner records r18, merged as 68799e1b).
- **PR title:** `X-torznab-id-caps: Torznab caps advertise ID parameters only when they can be answered`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do. A fresh Opus review gates the merge.
- The `runner/slices.yaml` entry already exists (r18). Don't add another; edit nothing else in that file or `runner/dashboard.yaml`.

## Why
The runner's audit of #1110 (ENR-11) found that `caps()` in `crates/api/src/torznab.rs` (about lines 196-199) advertises `tvdbid`, `imdbid` and `tmdbid` unconditionally. While ID coverage is partial (before the identify sweep has run, or for releases no source identified), Sonarr and Radarr send an ID search, get only the identified subset, and never see the unidentified releases. ENR-11's decisions file assumed the *arrs fall back to a title search; for partial coverage they don't, because a non-empty ID answer is taken as complete.

## Read first
`docs/decisions/2026-11-24-ENR-11.md`, `docs/decisions/2026-11-11-TZ-0.md`, architecture §18.1, `crates/api/src/torznab.rs` and `crates/api/src/torznab/**`, `crates/api/tests/torznab.rs`, and `runner/standards.md`.

## Do
1. Decide, and record in `docs/decisions/2026-11-27-X-torznab-id-caps.md`, what an ID search does under partial coverage. Options to weigh: advertise the ID params only once coverage is known complete enough (and say what "enough" means and where that state comes from); or keep advertising them but answer an ID search with the ID matches plus a title search over unidentified releases, so nothing is hidden. Pick the one that never hides a release Sonarr/Radarr would otherwise find, and keeps the caps truthful. If the choice changes a number or loosens an owner decision, stop and tell the runner.
2. Implement it inside your owns. If the coverage state you need lives outside them (store or search crates), tell the runner before touching anything else; don't widen silently.
3. Tests in `crates/api/tests/torznab.rs`: caps with no coverage, with partial coverage and with full coverage; an ID search where only some releases are identified returns the unidentified ones too (or the caps don't advertise the param, per your decision). Keep the existing golden caps test honest: update its expectation, don't delete it.
4. Out of scope, noted for the runner: the user-facing notice that ID fields are partial until the sweep finishes needs `crates/api/src/search.rs` (SRC-3b owns it). Mention in the PR what that notice should say; don't build it.

## Owns
`crates/api/src/torznab.rs`, `crates/api/src/torznab/**`, `crates/api/tests/torznab.rs`, `docs/decisions/*-X-torznab-id-caps.md`. Not `crates/api/src/search.rs`, not `crates/sources/**` or `crates/integrations/**` (X-sec-S8-25 holds them), not `crates/dht/**`.

## Done
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. No fixed sleeps. Report `ready <n> <full sha>` to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) once ci-ok, web-ok and e2e-ok are green. Push once per round of fixes.

No AWS or cloud calls, no new accounts, no spending, no force-push. Held security detail stays off every branch.
