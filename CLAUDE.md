# Lodestar v2

A from-scratch DHT crawler and torrent-metadata search engine: one Rust binary with an embedded Svelte 5 UI, running on Docker, Unraid or Windows. Tagline: "Every node is a new discovery".

**`docs/architecture.md` is the source of truth** for architecture, scope, milestones (§14), budgets (§3, §10.1) and every decision so far (logged in §16). Read the relevant sections before starting any task. If work changes a decision, update that document in the same PR.

## Working conventions
- **Branches:** one feature branch per milestone or coherent slice (`m0-foundations`, `m2-search-core`, …), merged into `main` by pull request. Nothing goes directly to `main` except trivial docs fixes. Keep PRs reviewable (≲ 1,500 changed lines where practical); split large milestones into several PRs.
- **Before pushing:** run `cargo fmt --check`, `cargo clippy --workspace -- -D warnings` and `cargo test --workspace`. Once `web/` exists, also run `npm run check`, `npx vitest run` and `npx size-limit` there. Don't push red.
- **Commits:** imperative subject ≤ 72 chars, a body that explains why, and the Co-Authored-By / Claude-Session trailers the harness supplies. Never put model identifiers in commits, PRs or code.
- **Tests:** every crate ships unit tests. Parsers (bencode, KRPC, metainfo, query language, release-name parser) also get property tests or `cargo fuzz` targets, plus golden-corpus tests where the plan calls for them. Benchmarks gate budgets once `bench/` exists.

## Model and delegation policy (approved by the owner)
- **Main session (Opus 5.5, high effort):** core and correctness-critical work, which means bencode/KRPC/metainfo, storage format and schema, the DHT crawler, the peer protocol (BEP 3/9/10, MSE), search (tokenizer, query language, ranking, counts, pagination), concurrency and anything that parses untrusted input. Also all reviews.
- **Sonnet 5.5 helper agents:** well-specified, pattern-following work. Svelte components and screens per §10, theme JSON and the theme compiler, packaging (Docker, Unraid, WiX), CI workflows, docs, and test scaffolding that follows an existing pattern.
- **Haiku 5.5 helper agents:** mechanical edits only, such as generating theme files from the spec, renames and formatting.
- **Never delegate:** security-sensitive code, untrusted-input parsers, on-disk formats, or concurrency-heavy code.
- **Every helper result is reviewed and tested in the main session before merge.** Weak output is redone on Opus. Helpers that edit the same files never run at the same time.

## UI rules
- **`dvortsis/skills/ui-design-skills`** (`ui-design-skills/skills/*/SKILL.md`) is the authority for UI decisions, with its precedence order. It's applied **by concept and spirit** for Svelte where a skill assumes React/Tremor: tiny in-house SVG charts plus lazily loaded uPlot replace Tremor.
- **The owner's explicit choices override the skill defaults:**
  - Svelte 5 with the smallest, fastest UI as top priority;
  - `minimalist-visual-style`;
  - fonts: **Atkinson Hyperlegible Next** (UI) and **Atkinson Hyperlegible Mono**;
  - the **Riverstone** palette with plum `#9381B6` replacing seafoam;
  - 24 theme families × light/dark;
  - brand assets per §10.19.
- Colour roles, derived ink/data tokens, contrast gates and the theme compiler are specified in §10.18. Never hard-code hex values in components; use tokens.

## Environment notes (cloud sessions)
- **Git LFS uploads are blocked by the proxy.** Brand assets in `brand/source/` are plain files, verified with `SHA256SUMS.txt`. Don't reintroduce LFS.
- **Claude can't create GitHub repositories on this account;** the owner creates them.
- **Clones are shallow.** Run `git fetch origin main` before pushing a branch.

## Repository layout
See §13 of `docs/architecture.md`. In short:
- `crates/*`: Rust workspace
- `web/`: SvelteKit SPA
- `packaging/`
- `bench/`
- `brand/source/`
- `docs/`
