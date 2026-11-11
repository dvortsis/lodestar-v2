# PK-ui: wizard filesystem warning and six-hour wait; Insights copy for the new exceptions
**Model:** Sonnet, a UI helper. A fresh Opus reviewer follows. Runner brief, 2026-11-11T16:15Z. Epic M6 (ws-m6), on the path PK-ui → PK-guide-2 → PK-guide-3 → G-m6. The dep PK-exc merged as #687 (71530a30).

## Read first
- `CLAUDE.md` (the UI rules) and `runner/standards.md`.
- `docs/plan/2026-11-06-m6-gap-audit.md`: the PK-ui rows (~40, ~41, ~95, ~112), the PK-ui paragraph (~189), and **Q12** (~221), which gives the exact wording.
- `docs/decisions/2026-11-11-PK-exc.md`, especially **"For PK-ui"** (~61) and the 9p note (~59).
- `crates/api/src/dto/insights.rs` (~400–520): `InsightsException`, `DataFilesystem` and the `ephemeral_ports` detail. `crates/api/src/digest.rs` has the model sentences for both.
- `docs/architecture.md` §10.10.10 (the wizard), §10.9.2 (Insights exceptions), §10.14 and §11. Also `docs/design-tokens.md` and `docs/ui-glossary.md`.

## Do
1. **Storage step** (`web/src/lib/features/wizard/StorageStep.svelte`).
   - Read `/system`'s `filesystem_warning`, which already exists; check its shape in the OpenAPI schema.
   - When it's set, show a warning with the safe alternative:
     - `unraid_user_share`: move the data folder to a pool path such as `/mnt/cache/appdata/lodestar`;
     - `fuse` and `network`: keep it on a local disk. Call `network` "a network or shared-folder filesystem", which covers 9p mounts from WSL2 and Docker Desktop.
   - It's a warning, not a block: the user can continue.
2. **How step and Done step** (Q12). Keep the shipped v0.5.1 sentence ("After every start, including restarts and updates, sampling waits 6 hours before it resumes"), and add: "Full speed starts after those 6 hours. With safe restart on, later clean restarts skip the wait."
   - Replace `wizard_how_cold`'s days-and-weeks wording at `wizard/messages/en.json:~56`.
   - Use the same words in both steps, so PK-guide-1's copy matches.
3. **Insights exceptions** (`web/src/lib/features/insights/overview/Exceptions.svelte`). Render the two new kinds with an icon, a text and a fix (§10.9.2), replacing today's generic "Needs attention: data_filesystem":
   - `data_filesystem`: the sentence follows `filesystem.kind`, as in step 1. `fs_type` is a detail only, not part of the sentence.
   - `ephemeral_ports`: give the `netsh` command built from `ports.family`, `fix_start` and `fix_count`, to run in an administrator Command Prompt. When the fix fields are null, say "lower the crawler's intensity in Settings, then restart Lodestar" instead. The sentence counts the rest of the PC: "Lodestar and the rest of this PC need about …".
   - Map `fix.kind: "guide"` to the guide pages PK-exc names.
4. **Messages:** every new key in `en` and `zh`. The zh wording must match the existing glossary terms. Don't hard-code text.
5. **Tests:**
   - vitest in `WizardPage.test.ts` and `insights/overview/components.test.ts`, with fixtures in `fixtures.ts`;
   - both exception kinds, with and without fix fields, and each `filesystem.kind`;
   - the storage warning shown and absent;
   - the How and Done wording.

   No fixed sleeps. `waitFor` deadlines are at least 10 s.

## Rules
- Tokens only: no hex and no arbitrary values. Spacing follows the §10.2 ramp. Svelte 5 runes.
- Keep the wizard and Insights first-view sizes under their `size-limit` entries. Raising a limit needs the runner's OK first.
- If the API lacks a field you need, stop and ask the runner. Don't edit `crates/**`.

## Owns
- `web/src/lib/features/wizard/StorageStep.svelte`, `HowStep.svelte`, `DoneStep.svelte`, `messages/**` and `WizardPage.test.ts`;
- `web/src/lib/features/insights/overview/Exceptions.svelte`, `components.test.ts` and `fixtures.ts`;
- `web/src/lib/features/insights/messages/**`;
- your decisions file `docs/decisions/2026-11-11-PK-ui.md`, if there's anything to record.
- Ask the runner first for anything else.

## Done when
- **Checks** in `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, and `npx playwright test` for any e2e spec covering the wizard or Insights, with `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium` and `--trace off`. Never run `playwright install`.
- **Process:** follow `.claude/skills/steward/SKILL.md`, push once per round, and never merge.
  - Branch `slice/PK-ui`. PR title `PK-ui: Wizard: filesystem warning on the storage step, the six-hour sampling wait; Insights copy for the new exceptions`.
  - Estimated ~350 lines.
  - Once CI is green, message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) "ready" with the PR number and full head SHA, using the Claude Code Remote MCP `send_message`.
