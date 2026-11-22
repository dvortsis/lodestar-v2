# X-pseudo-text-gaps decisions

Runner brief, 2026-11-22Z, from X-ui-checks-3 (#1045, `docs/decisions/2026-11-22-X-ui-checks-3.md`, "Findings"). The nightly pseudo-locale check carried two pieces of hard-coded English as `KNOWN_GAPS`. Both are fixed here, and both entries are removed, so the check now guards them.

- **Who decided:** choices marked "runner" are the runner's calls (§2.26), flagged to the owner (§2.19). The rest are the slice session's.

## "just now"

- **The message:** `common_just_now`, in the shared catalogue `web/src/lib/i18n/messages/{en,zh}.json`: "just now" and 刚刚 (runner). It sits beside `common_dismiss` and `common_no_matches`.
- **The caller:** `RelativeTime.svelte` passes it as `justNow`. It's the only `formatRelative` caller in `src/`.
- **Why "just now" and not Intl's word:** Intl's word is "now", which reads as a different and less natural word for a timestamp (runner).
- **relative.ts keeps no English (runner).**
  - The built-in `{ en: 'just now', zh: '刚刚' }` table is gone.
  - Without `justNow`, the moment is Intl's own word: `relativeFormat(locale).format(0, 'second')`, with `numeric: 'auto'`, which gives "now" in en, 现在 in zh and "maintenant" in fr.
  - So a plain `formatRelative` call, from a test or any future caller, is localized in every locale Intl knows. Only the UI's wording comes from the catalogue.
- **Tests:**
  - `RelativeTime.test.ts` checks that the shown text is `m.common_just_now()` in en and zh.
  - In `format.test.ts`, the under-45 s rows now expect Intl's word, a new case checks that word in another locale, and the existing "takes the translated just_now message" case stays.

## Insights annotations (`Movement.svelte`)

- **Intensity levels show a label, not the raw value.** The server sends the setting's value: `low`, `balanced` or `aggressive` (`crates/lodestar/src/settings/crawler.rs`, `Intensity::name`).
  - **The labels are Insights' own** `insights_overview_level_{low,balanced,aggressive}`, with the same words as Settings: Low, Balanced, Aggressive, and 低, 均衡, 激进 (runner).
  - **Why not reuse Settings' `settings_crawler_intensity_option_*`:** step 0 proposed it and the runner approved it, but the measurement reversed that (runner).
    - Settings' crawler message map imports those message modules too. With Movement importing them as well, Rollup moved them into a new shared chunk.
    - That chunk is counted in "Settings messages (on demand)", which went 4,259 → 4,435 B, past its 4.26 KB limit.
    - Nine Settings first-view screens also grew 13 B each.
    - With Insights' own copies, every Settings entry is unchanged. The cost is a second copy of three short strings per locale.
- **Lookups are own-key only** (`Object.hasOwn`), so a value like `constructor` reads as unknown, not "[object Object]" (runner's review). The server sends only the known names today.
- **A level this build doesn't know** reads `insights_overview_note_intensity_changed`: "Intensity changed" / 强度已更改, with no level.
- **An unknown kind** reads `insights_overview_note_unrecognised`: "An unrecognised event" / 无法识别的事件.
  - **A sentence, not the kind as code:**
    - The raw kind (`import_removed`, say) means nothing to the reader.
    - Shown as code, it would still be untranslated English in the list and its `aria-label`.
    - The CSV export keeps the raw kind for anyone who needs it.
  - `insights_overview_note_unknown` ("Event: {kind}") is removed. It had no other caller, in en or zh.
- **Tests (`Movement.test.ts`):**
  - a known level gives its label, in en and zh;
  - an unknown level gives "Intensity changed" with no raw text;
  - an unknown kind gives the sentence, and neither locale shows any of the kind's or text's words.

## The pseudo-locale check

- **`KNOWN_GAPS` is empty.** The array and its typedef stay, with a comment saying a new gap is listed there with the slice that fixes it (runner).
- The `inMessage` helper is still used by the Intl times allowance.
- `scripts/pseudo-locale.test.ts` passes unchanged.

## Sizes (`npm run size:ci`, gzip; main d4f1446c → branch)

- **Re-measured after merging main bcdd514b** (#1051, #1057): every figure below is the same, with the own-key lookups in.
- **Search route JS (initial): unchanged by this PR.** It's 44,682 B on d4f1446c, and 44,847 B on both main bcdd514b (#1057's change) and this branch. The relative-time chunk isn't in Search's first view.
- **Search results (lazy): 40,822 → 40,841 B.** The limit goes from 40.84 to 40.85 kB, the measured size rounded up to 0.01 kB (runner, §2.26). It's the only limit that moves.
- **The cause:** about 19 B in each chunk that uses RelativeTime. That's the `m` import and the `common_just_now` message module (en and zh), minus the removed 18 B English table.

| Entry                             | Before | After  |
| --------------------------------- | ------ | ------ |
| Search results (lazy)             | 40,822 | 40,841 |
| Search cards (lazy)               | 8,000  | 7,997  |
| Discover route                    | 41,705 | 41,724 |
| Alerts route                      | 24,489 | 24,508 |
| Alert route                       | 45,816 | 45,833 |
| Settings AboutScreen (first view) | 33,340 | 33,353 |
| Detail drawer                     | 28,274 | 28,280 |
| /t/[hash] route                   | 22,057 | 22,064 |
| Insights stage pages              | 40,633 | 40,652 |

Every other entry is unchanged, and every limit passes.
