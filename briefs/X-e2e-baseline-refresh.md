# X-e2e-baseline-refresh (Sonnet, helper): stale Insights e2e baselines and a freezeClock note

Assigned by the runner on 2026-11-11 to the session that built CMP-9c (#665, merged as 61459e4d). The slices.yaml entry is X-e2e-baseline-refresh.

## Check first
After #541, some Insights state baselines (`web/e2e/states.test.ts-snapshots/**`) still showed old text. Later PRs (#674 and others) may already have refreshed some or all of them.
- Find which baselines are still stale. Compare the rendered strings in each image against the current `messages/en.json`, or run `npx playwright test e2e/states.test.ts` against the current build and look at the diffs.
- If none are stale, say so. The PR is then only the README line.

## Do
1. Regenerate the stale baselines through e2e.yml's own mechanism only:
   - commit `web/e2e/.update-snapshots`, push, let CI regenerate the baselines and commit them back (see the e2e.yml comments around :82-:120 and :232-:256);
   - then remove the marker and push again so CI compares.

   Don't hand-generate baselines locally: local renders differ from CI's.
2. Add a line to `web/e2e/README.md`: call `freezeClock(page)` before `go()` in any snapshot spec. Say why: relative times otherwise change the pixels.
3. Regenerate only Insights state baselines. If the marker regenerates others, only commit the ones that changed for a stated reason, and list them.
   - CMP-9a (#681) is open and touches the settings snapshots. Don't regenerate the `settings-*` or `theme-snapshots/settings--*` images.
   - If the mechanism makes that impossible, stop and tell the runner.

## Owns
- `web/e2e/README.md`
- `web/e2e/states.test.ts-snapshots/**`
- `web/e2e/.update-snapshots` (temporary)
- `docs/decisions/2026-11-11-X-e2e-baseline-refresh.md`

## Bar
- npm run check; the e2e job green on the final, comparing push.
- Branch `slice/X-e2e-baseline-refresh`, PR `X-e2e-baseline-refresh: <title>`, steward skill.
- Reply "ready" with the head when ci-ok, web-ok and e2e-ok are green.

After it merges, this session takes CMP-9b, once CMP-9a (#681) has merged, since its Set up opens CMP-9a's install dialog.
