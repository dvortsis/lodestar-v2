# OUT-2 (Opus, /security-review): Settings, Outputs: Torznab URL and key, bitmagnet export

Runner brief, 2026-11-12 (UTC). Slice entry: `runner/slices.yaml` OUT-2. Deps: OUT-0 (merged as #716, dadcb069) and **TZ-0 (#715, not merged yet)**. Same session as OUT-0. Use the steward skill; Auto mode.

**Start condition:** read and plan now, but branch `slice/OUT-2` from `origin/main` only after the runner tells you TZ-0 has merged. TZ-0 adds the Torznab key and endpoint this screen shows.

## Read first
- `docs/plan/2026-11-05-v07-m7.md` (Outputs section) and `docs/decisions/2026-11-05-P-v07.md`.
- `docs/architecture.md` §10.10 and §10.10.7 (the Settings screens and the password dialog for secret reveal); `docs/design-tokens.md`, `docs/ui-glossary.md`; §10.1 budgets (route JS ≤ 45 KB first load).
- TZ-0's decisions file and API (the Torznab key routes) once it merges; your own `docs/decisions/2026-11-11-OUT-0.md` and the export API.
- An existing Settings screen with a revealed secret and a dialog, as the pattern to follow.

## Do
- A Settings → Outputs screen in `web/src/lib/features/settings/outputs/**`, registered with the Settings registry the usual way:
  - **Torznab:** the endpoint URL to paste into Prowlarr, Sonarr or Radarr, with a copy button; the key hidden by default. **Revealing or copying the key goes through the password dialog** (§10.10.7), never shown in plain text otherwise; regenerating it asks for confirmation and says what breaks.
  - **bitmagnet export:** start an export, show progress and the download, with the `export_adult` setting and a plain sentence on what is always left out (private, hidden, unverified, blocklisted).
- en and zh strings, glossary terms, tokens only (no hex, no arbitrary values), keyboard and screen-reader paths, loading, empty and error states.
- Tests: vitest for the logic, and a Playwright e2e for reveal-through-dialog and export start, with condition waits (no fixed sleeps). Size-limit entries if the screen is on demand.
- **Fold in OUT-0's nit:** add "(#716)" to the heading of `docs/decisions/2026-11-11-OUT-0.md` (shared one-line edit, approved).
- Decisions file `docs/decisions/2026-11-12-OUT-2.md`.

## Owns
`web/src/lib/features/settings/outputs/**`, its e2e spec, the decisions file, message and glossary rows, the Settings registry line, a size-limit entry, the OUT-0 heading. Anything else: ask the runner.

## Bar
- runner/standards.md; `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, `npx playwright test` for the new spec (`PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`).
- Branch `slice/OUT-2`, PR `OUT-2: Settings: Outputs, Torznab URL and key, bitmagnet export`. Aim for under 1,500 lines; say why if larger.
- Self-review including /security-review; push once per round; reply "ready" to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with PR number and full head SHA when ci-ok, web-ok and e2e-ok are green.
