# R-docs: Runner, contract and packaging doc cleanups (carry-forwards)
**Model:** Sonnet (docs and config only). **Branch:** `slice/R-docs`.

**Spec:**
- `runner/standards.md`, `runner/README.md`, `runner/slices.yaml`
- `docs/architecture.md` §16 (the decisions log; append only)
- `docs/metrics.md`, `docs/design-tokens.md`, `docs/themes.md`
- `crates/integrations/README.md`
- `packaging/unraid/lodestar.xml`

## Own
Only these files, and only the changes listed:
- `runner/README.md`
- `runner/slices.yaml`
- `docs/architecture.md` (new §16 entries only)
- `docs/metrics.md`
- `docs/design-tokens.md`
- `docs/themes.md`
- `crates/integrations/README.md`
- `packaging/unraid/lodestar.xml`

**No code changes.**

## Changes
1. **`runner/README.md`:** add the rule "When a PR conflicts with `main` only in `docs/architecture.md` §16, the runner (or the slice) resolves it by merging `main` and keeping both blocks, main's first. A semantic conflict, such as a new required struct field, is fixed in the same merge commit and the checks re-run."
2. **`runner/slices.yaml`.** Read each slice's merged PR, as named in the PR titles on GitHub, to confirm the paths before changing them.
   - **W1b:** the title says "47 JSONs"; it's 23 families / 46 themes. Widen `owns` to match what W1b actually changed.
   - **X-email:** add the slice (opus, deps X-notify), owning `crates/integrations/src/notify/{email,text}.rs`, `src/net.rs` (the guarded TCP connect), `tests/email.rs` and `tests/fixtures/email/**`.
   - **X-notify:** narrow `owns` to `crates/integrations/src/notify/{ntfy,discord,rss,webhook,mod}.rs` plus its tests and fixtures.
   - **X-clients:** `owns` includes `crates/integrations/tests/{transmission,deluge,rtorrent,aria2}.rs` and `tests/fixtures/<client>/**`.
   - **W6:** `owns` is `web/e2e/**` (not `web/tests`), `web/playwright.config.ts`, `web/lighthouserc*.json` and `.github/workflows/e2e.yml`.
   - **B17:** keep it as is. **R-docs:** add this slice (sonnet, docs).
   - Validate the YAML parses: `python3 -c "import yaml,sys;yaml.safe_load(open('runner/slices.yaml'))"`.
3. **`docs/architecture.md` §16:** append one block, "Runner-recorded decisions (2026-10-22)", with:
   - **X-notify:**
     - Discord sends without following redirects;
     - ntfy requires JSON with an `id` on 2xx;
     - ntfy credentials go over plain http only to private hosts.
   - **D-docker:**
     - the image's STRIP/DEBUG build overrides;
     - the §4 data layout as built (read `packaging/docker/` and `crates/lodestar/src/config.rs` and describe the actual layout; don't change §4's text, note the difference).
   - **The W6 snapshot policy:**
     - key screens only;
     - the shared 46-theme set at breakpoint 1366;
     - Compact density only on density-aware routes;
     - baselines generated in the pinned Playwright container, updated via the `.update-snapshots` marker or workflow_dispatch, and imported by the owner when the artifact host is blocked.
4. **`docs/metrics.md` §6.7:** add `notifications_delivered` and `notifications_failed`. Read `crates/integrations/src/metrics.rs` (or wherever `IntegrationMetrics` lives) for the exact names, labels and units, and keep the table's format.
5. **`docs/design-tokens.md`:** the CJK fallback must use installed family names ("Noto Sans CJK SC" etc.). Align with §10.2's wording without changing any token names.
6. **`docs/themes.md`:** the derived-token gates table still says 9 pending (W1c); all 48 themes pass now. Verify by running `cd web && npm run themes:check` (or whatever script the theme compiler exposes; read `web/package.json`), then update the table.
7. **`crates/integrations/README.md` rule 1:** mention `net::connect_tcp` / `resolve_tcp` for non-HTTP protocols (SMTP). **Only if X-email (#63) has merged when you start;** otherwise skip this item and say so.
8. **`packaging/unraid/lodestar.xml`:** add `--user 99:100` to ExtraParams. The image runs as nonroot until the binary drops privileges for PUID/PGID, so the template's PUID=99/PGID=100 has no effect yet. Add an XML comment saying so, and check the XML parses.

## Done when
- `cd web && npm run check` passes, if anything under `web/` was read by scripts.
- `cargo fmt --check` is untouched (no Rust changes).
- The YAML and XML parse.
- `git fetch origin main` merged (no rebase).
- A PR titled `R-docs: Runner, contract and packaging doc cleanups`, listing each item done or skipped and why.
