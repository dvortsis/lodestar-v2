# X-cmp-egress-ui: Companions screen: egress editor, restart warning, live SOCKS5 warnings

**Runner brief, 2026-11-24 (UTC).** Helper (Sonnet-class UI work), `/security-review` before merge (it edits the route that keeps a companion's traffic on the VPN). About 600 lines with tests.
- **Branch:** `slice/X-cmp-egress-ui` from fresh `origin/main` (or the branch the harness fixes; the PR title is what the dashboard reads).
- **PR title:** `X-cmp-egress-ui: Companions screen: egress editor, restart warning, live SOCKS5 warnings`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Why
X-cmp-egress-api (#805) added `Companion.egress` (`{from, setting}` or null), `Companion.runs` (`lodestar`, `own_container`, `yours`, `not_here`) and `PUT /companions/{id}/egress`. Its decisions file says the editor and the switch of the screen's "where it runs" sentence to these fields are this follow-up. CMP-9a (#681) and the #672 review ask for the SOCKS5 warnings.

## Read first
- `docs/decisions/2026-11-12-X-cmp-egress-api.md` (the whole file: the PUT's codes, `restarting` vs `not_restarted`, the admin password, `cant_follow_route`).
- `docs/decisions/2026-11-11-CMP-9a.md` and `2026-11-11-X-cmp-egress-prelaunch.md` lines 38–43 (the SOCKS5 residuals).
- The existing screen under `web/src/lib/features/settings/companions/**`, and how the Sources screen edits a source's route (reuse its route control and the admin-password confirm pattern; don't fork them).
- `docs/design-tokens.md`, `docs/ui-glossary.md`, architecture §10.10.13 and §18.10, `runner/standards.md`.

## What to build
1. **Route editor** on a companion's row, shown only where `egress` isn't null (so only `runs: lodestar` and a companion that takes a route).
   - Shows the current setting and whether it is the companion's own (`from: saved`) or copied from its source (`from: source`), in plain words.
   - Offers only the kinds the API accepts. `interface` and `bind_address` are not offered.
   - Saving asks for the admin password (`428 confirm_required` / `401 bad_password`), the same dialog as the Sources route change.
2. **Restart warning:** before saving, the screen says that saving restarts the companion. After saving:
   - `restarting: true`: say it's restarting on the new route and keep polling `health` the way the restart button does. Never say "applied".
   - `restarting: false`: say the route is saved and is used at the next start, with the sentence for the `not_restarted` code.
   - Each refusal (`unknown_companion`, `takes_no_route`, `not_installed`, `busy`, `cant_follow_route`, `route_unavailable`, `invalid_egress`, `bad_body`) gets its own message; the field is named, never the value.
3. **"Where it runs"** sentence comes from `runs` (and `refusal` for `not_here`), replacing the fixed sentence.
4. **Live SOCKS5 warnings** while Prowlarr's route is SOCKS5: email notifications connect directly (MailKit), a FlareSolverr pairing leaves direct, and per-indexer proxies bypass the route. Warn only; the server decides what's refused.
5. Messages in the feature's `en.json` and `zh.json`. Tokens only, no hex, Tailwind steps from the §10.2 ramp.

## Owns
`web/src/lib/features/settings/companions/**`. Ask the runner before touching anything else (a shared route control under another feature counts). No API change: if the API lacks something, stop and say so.

## Tests
- Vitest for the editor states: hidden when `egress` is null; `saved` vs `source`; each response (`restarting` true and false, each refusal); the SOCKS5 warnings on and off.
- Extend the companions e2e only if it's already owned or the runner agrees (`web/e2e/companions.spec.ts` is CMP-9a's).
- No fixed sleeps; waits are conditions with ≥ 10 s deadlines.

## Done
In `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npm run size:ci`, and Playwright for any e2e spec touched (`PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`; never `playwright install`). Name your refinements in the PR description and a `docs/decisions/2026-11-24-X-cmp-egress-ui.md` if you make any decision. Report `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green; a fresh Opus `/security-review` follows.

No AWS or cloud calls, no new accounts, no spending, no force-push.
