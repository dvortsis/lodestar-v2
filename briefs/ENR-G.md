# ENR-G: Adult-enrichment gate: never looked up, never sent, never stored; consent; visibility; purge

**Runner brief, 2026-11-24 (UTC).** Core gate slice, Opus. Tests that prove the adult-enrichment path is safe end to end; fixes only where a test finds a real hole. Target < 1,200 changed lines.
- **Branch:** `slice/ENR-G` from fresh `origin/main` (or the branch the harness fixes).
- **PR title:** `ENR-G: Adult-enrichment gate: never looked up, never sent, never stored; consent; visibility; purge`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do. **/security-review before you ask for review**, and the runner's fresh Opus review plus a security pass gate the merge.

## Why
v0.6 Sources is done except this gate (build-plan §0 table; SRC-G passed, `docs/decisions/2026-11-07-SRC-G.md`). ENR-4 (ThePornDB, #503), ENR-5 (StashDB, #506), ENR-8 (detail drawer section, #508) and SRC-15b (consent dialogs and purge, #552) are merged. ENR-G proves, as a gate, that adult enrichment can't leak or run without consent.

## Read first
- `docs/owner-decisions.md` (anything on adult content, consent and §2.38), `docs/architecture.md` §18.7, §18.9, §20.5 (the child-safety core and its locked floor) and §20.9 (media categories, Adult).
- The ENR decisions files (`docs/decisions/2026-11-0*-ENR-*.md`), SRC-G's and SRC-15b's, and how SRC-G laid out its gate tests.
- The adult plugins in `crates/sources-plugins/`, the host's `Switches` and kill switch, the consent gate and purge in the sources host and API, and the ENR-8 drawer section in `web/`.
- `runner/standards.md`.

## What to prove (each a test; name the spec line it covers)
1. **Never looked up:** with Show adult off, or the plugin's consent not given, or the plugin off, no adult-enrichment request is built or sent for any item, including ones whose category is Adult. Use a fake transport that fails the test on any request.
2. **Never sent:** no item that the child-safety core (§20.5) blocks or flags, and no `private_source` item, is ever sent to an adult-enrichment service, whatever the settings; hashes and titles of such items never leave.
3. **Never stored:** enrichment results for a blocked item are dropped before storage; nothing from an adult service is stored while consent is off; nothing is written to logs beyond the §12 allowances.
4. **Consent:** enabling needs the Account-class consent (§18.7) and can't be bypassed through the API (direct PUTs, imports, settings restore). Withdrawing consent stops lookups at once.
5. **Visibility:** adult enrichment never shows while Show adult is off: API responses, `/suggest`, `/search/fields`, Torznab/Stremio, exports and the drawer (e2e `web/e2e/adult-enrichment.spec.ts`).
6. **Purge:** purging the source removes its stored enrichment and claims and leaves the rollups reconciled.
7. **Runner security pre-audit (2026-11-05):** check that the host `Switches::kill_switch_tripped` treats an Unchecked or permanently-down route as tripped, so an adult plugin with a broken or unchecked egress sends nothing. Test it.

If a test finds a hole, fix it in the smallest place and say so in the PR; if the fix is outside your owns or bigger than ~150 lines, stop and tell the runner first. Never weaken a safety rule to make a test pass; anything that would loosen a content-safety decision goes to the owner through the runner.

## Owns
`crates/sources-plugins/tests/adult_enrichment_safety.rs`, `web/e2e/adult-enrichment.spec.ts`, a decisions file `docs/decisions/<date>-ENR-G.md`, and this slice's `runner/slices.yaml` `gates` entry if one names ENR-G. Fixes elsewhere only with the runner's say-so.

## Done
Rust: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. Web: `npm run check`, `npx vitest run`, `npm run build`, `npm run size:ci`, `npx playwright test e2e/adult-enrichment.spec.ts` with `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium` (never `playwright install`). No fixed sleeps. Report `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green.

Landing lane: #1112 (X-escalate-full-table, Rust) is bringing main in; if it enters the lane, hold Rust pushes until it merges.

No AWS or cloud calls, no new accounts, no spending, no force-push. Held security detail stays off every branch.
