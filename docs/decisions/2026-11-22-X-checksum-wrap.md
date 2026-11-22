# X-checksum-wrap decisions

Runner brief, 2026-11-22Z, from #1034's review. On the install dialog (`web/src/lib/features/settings/companions/InstallDialog.svelte`), the 64-character SHA-256 was a `break-all` mono caption. That predates #1034. It wrapped wherever the column ended:
- 63+1 at the large layout, leaving one stray character on a line of its own;
- 46+18 at 390 px.

## The markup (runner, §2.26)
- **Two text nodes with a `<wbr>` between them:** `{shaHead}<wbr />{shaTail}`, the first 32 characters and the rest, with no spans. A missing value renders nothing (`offer.sha256 ?? ''`), as before.
- **`break-all` is replaced by `wrap-anywhere` (`overflow-wrap: anywhere`).**
- **Why it wraps 32+32:**
  - With `word-break: normal`, the default, a run of hex has no break opportunity, so the `<wbr>` is the only one.
  - Where 64 characters fit, the value stays on one line.
  - Where they don't, the line breaks at the `<wbr>`.
- **Why `overflow-wrap: anywhere` still breaks at the `<wbr>` first:** it applies only when no ordinary break opportunity lets the line fit. The `<wbr>` is an ordinary opportunity, so it's taken first. `anywhere` breaks inside a half only if that half can't fit on its own, and then the text wraps rather than overflowing sideways.
  - `anywhere` also keeps the grid cell's min-content width small, as `break-all` did, so the checksum can't widen the dialog. `break-word` wouldn't do that.
- **Copy and screen readers:**
  - `<wbr>` adds no character, so a copy gives the 64 characters, with no space or newline.
  - Screen readers get one text value, with no `aria-label`.
- **No hex values, arbitrary values or new spacing.** `wrap-anywhere` is already in the shipped CSS (`PAGE_H1`), so the CSS entry didn't move.

## The check (`web/e2e/companions.routes.test.ts`)
- **What it runs:** "the checksum wraps evenly and copies whole at 1366 px" and the same test at 390 px, in the `core` project, the only one that runs this spec.
- **How it reads the lines:** it measures each character's line from a one-character `Range`'s box, groups the characters by top, and requires `[64]` or `[32,32]`.
  - So no line under 32 can pass, and a half broken inside itself fails too.
  - It also checks that neither the box nor the dialog scrolls sideways.
- **Layouts measured:**
  - **1366×900 (large):** 32+32. The 478 px column fits 63 characters, not 64.
  - **390×844:** 32+32. A half fits with room to spare, so neither half breaks inside itself.
  - No viewport tested gives one line, which the check would also accept.
- **Mutation-checked:** with the old `break-all` markup put back, both tests fail, with `[63,1]` at 1366 px and `[46,18]` at 390 px.
- **Copying:**
  - **Selection read (required):** the test selects the `<dd>`'s contents, and `getSelection().toString()` must equal the 64-character SHA.
  - **Clipboard read (optional; kept):** after the selection, Ctrl/Cmd+C, then `navigator.clipboard.readText()`, which must equal the SHA too.
    - **It clears the clipboard first** (`navigator.clipboard.writeText('')`), before the key press. Headless Chromium shares one clipboard across a worker's contexts, so without that an earlier test's copy could pass the read (runner's review, checked with a probe). The selection read is unaffected.
    - It needs only `context.grantPermissions(['clipboard-read', 'clipboard-write'])`, which Chromium supports. Every project that runs the spec is Chromium, so there's no browser-specific skip.
    - **Repeated:** the whole spec passed 20 of 20 runs (`--repeat-each 5`).
    - If it ever flakes, drop the clipboard read and keep the selection read (runner).
- **Waits:** conditions only (`expect.poll`, the spec's 10 s deadline), no fixed sleeps.

## Sizes (`npm run size:ci`, gzip; main ff250d30 → branch)
| Entry | Before | After | Limit |
|---|---|---|---|
| Settings companions parts (on demand) | 28,846 B | 28,900 B | 29 KB |
| Sources companion chip and setup (on demand) | 26,897 B | 26,951 B | 27 KB |

- Both entries count the install dialog, so both grew by the same 54 B: the two derived halves and the `<wbr>`.
- Both stay within their limits, and no limit changes. The Sources entry has 49 B left.
- No other entry moved.
