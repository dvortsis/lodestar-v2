# Lodestar v2

A from-scratch DHT crawler and torrent-metadata search engine: one Rust binary with an embedded Svelte 5 UI, running on Docker, Unraid or Windows. Tagline: "Every node is a new discovery".

## Order of authority
1. **`docs/owner-decisions.md`: the owner's own instructions and decisions.** These are the primary rules.
2. **`docs/architecture.md`:** the approved plan built from them. It covers architecture, scope, milestones (§14), budgets (§3, §10.1) and every decision so far (logged in §16).
3. **The contract docs:** `docs/design-tokens.md`, `docs/metrics.md`, `docs/ui-glossary.md`.
4. **`ui-design-skills`:** only for what 1–3 leave open (see UI rules).
5. **The task's brief.**

Read the relevant sections before starting any task. If work changes a decision, update `docs/architecture.md` (§16) in the same PR. If it touches one of the owner's decisions, stop and ask the owner. Builds run through `docs/build-plan.md` and `runner/`; **`runner/standards.md` is the bar for every slice and review.**

## Working conventions
- **Branches:** one feature branch per milestone or coherent slice (`m0-foundations`, `m2-search-core`, …), merged into `main` by pull request. Nothing goes directly to `main` except trivial docs fixes. Keep PRs reviewable (≲ 1,500 changed lines where practical); split large milestones into several PRs. A larger PR needs a stated reason in its description.
- **Before pushing:**
  - Rust: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
  - In `web/`: `npm run check` (svelte-check, eslint, prettier, the hex and arbitrary-value bans), `npx vitest run`, `npm run build`, `npx size-limit`, and `npx playwright test` for anything with an e2e spec. Chromium is at `/opt/pw-browsers`; never run `playwright install`.
  - Merge `origin/main` (don't rebase shared branches). Don't push red.
- **Merging:** PRs merge with green CI and no blocking findings from a fresh Opus reviewer. The build runner may merge them, falling back to GitHub auto-merge (owner, 2026-10-22).
- **Commits:** imperative subject ≤ 72 chars, a body that explains why, and the Co-Authored-By / Claude-Session trailers the harness supplies. Never put model identifiers in commits, PRs or code.
- **Tests:** every crate ships unit tests. Parsers (bencode, KRPC, metainfo, query language, release-name parser) also get property tests or `cargo fuzz` targets, plus golden-corpus tests where the plan calls for them. Benchmarks gate budgets once `bench/` exists.

## Model and delegation policy (approved by the owner)
- **Main session (Opus 5.5, high effort):** core and correctness-critical work, which means bencode/KRPC/metainfo, storage format and schema, the DHT crawler, the peer protocol (BEP 3/9/10, MSE), search (tokenizer, query language, ranking, counts, pagination), concurrency and anything that parses untrusted input. Also all reviews.
- **Sonnet 5.5 helper agents:** well-specified, pattern-following work. Svelte components and screens per §10, theme JSON and the theme compiler, packaging (Docker, Unraid, WiX), CI workflows, docs, and test scaffolding that follows an existing pattern.
- **Haiku 5.5 helper agents:** mechanical edits only, such as generating theme files from the spec, renames and formatting.
- **Never delegate to Sonnet or Haiku:** security-sensitive code, untrusted-input parsers, on-disk formats, or concurrency-heavy code.
- **Opus sub-agents and sessions may build core slices** (owner-approved exception, `docs/owner-decisions.md` §1b.3), with every result reviewed by a fresh Opus reviewer before merge.
- **Every helper result is reviewed and tested in the main session before merge.** Weak output is redone on Opus. Helpers that edit the same files never run at the same time.

## UI rules
- **The design decided with the owner is primary; `ui-design-skills` fills the gaps** (owner, 2026-10-22).
  - The owner's choices and the §10 UI plan come first. The §10 plan already applies the skills, as the owner asked in the design session.
  - `dvortsis/skills/ui-design-skills` (`ui-design-skills/skills/*/SKILL.md`, in its own precedence order) decides only what they leave open, and never overrides them.
  - It's applied **by concept and spirit** for Svelte where a skill assumes React/Tremor: tiny in-house SVG charts plus lazily loaded uPlot replace Tremor.
- **The owner's explicit choices:**
  - Svelte 5 with the smallest, fastest UI as top priority;
  - `minimalist-visual-style`;
  - fonts: **Atkinson Hyperlegible Next** (UI) and **Atkinson Hyperlegible Mono**;
  - the **Riverstone** palette as the default and official palette, with plum `#9381B6` replacing seafoam;
  - a large theme collection: **24 families × light/dark, Rosewood the 24th**;
  - brand assets per §10.19.
- **Spacing** uses Tailwind's default scale (`p-4` = 16 px), limited to the steps on the §10.2 ramp (`docs/design-tokens.md` §5).
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
