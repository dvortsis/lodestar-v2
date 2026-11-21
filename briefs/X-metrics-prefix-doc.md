# X-metrics-prefix-doc

Runner brief, 2026-11-21Z. Slice id `X-metrics-prefix-doc`, branch `slice/X-metrics-prefix-doc`, PR title `X-metrics-prefix-doc: <title>`. The plan entry is in `runner/slices.yaml` (from `docs/decisions/2026-11-21-runner-records-r6.md`). A small docs slice; an Opus reviewer checks it.

## Why
The cloud tester read `docs/metrics.md`'s tables literally and looked for `dht_sample_sent_total`, but the `/metrics` endpoint exports it as `lodestar_dht_sample_sent_total`: `crates/lodestar/src/metrics.rs:48-50` writes every series as `lodestar_{name}`. The doc never says so, and some rows (for example lines 415–431) quote the prefixed name while most tables use the bare one.

## What
1. **One plain sentence** near the top of `docs/metrics.md`, where the exported series are introduced: every series on `/metrics` carries the `lodestar_` prefix, so a table's `dht_sample_sent_total` is scraped as `lodestar_dht_sample_sent_total`. Confirm the wording against `metrics.rs` (including how labels are written) before you write it.
2. **Check the tables.** Every series name in the metric tables should be the unprefixed one, with the one-sentence rule covering the prefix. Where a row quotes the prefixed name in prose as the scraped name (as lines 415–431 do, mapping API fields to series), leave it if it's clearly the scraped name; otherwise make it consistent. List every row you changed in the PR description.
3. **A small test, if cheap:** if there's already a test that checks `docs/metrics.md` against the exported names (search `crates/lodestar` and `tools/` for one), extend it so a prefixed name in a table fails. If none exists, don't build one; say so in the PR.
4. No decisions file is needed unless you make a call beyond this; if you do, add `docs/decisions/2026-11-21-X-metrics-prefix-doc.md`.

## Owns
`docs/metrics.md`, this slice's `runner/slices.yaml` entry only, and the test file in item 3 if one exists (tell me which first). No other running slice edits `docs/metrics.md` now.

## Bars
`runner/standards.md`; developer docs are exempt from the user-docs skill. UTC only, no model names. If you touch Rust, run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and the touched crate's tests. Push once, and send "ready <pr> <sha>" when CI is green. Follow `.claude/skills/steward/SKILL.md`; the runner merges.
