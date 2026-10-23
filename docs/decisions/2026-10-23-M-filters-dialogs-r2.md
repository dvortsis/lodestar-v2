# M-filters-dialogs decisions, part 2: CSX-5, Block this, reveal (2026-10-23)

Follows `2026-10-23-M-filters-dialogs.md` (part 1). None changes an owner decision. The first two are §2.19 refinements the runner accepted, flagged to the owner.

1. **"Block this…" defaults to the `hide` action** (§2.19 refinement). `hide` applies to stored torrents and is reversible at once; `drop` only stops future fetches and leaves stored ones as they are; `quarantine` holds and seals the name for 30 days. Each choice carries one line saying so. The rule is still previewed, and the server decides whether it needs the password.
2. **A revealed sealed name has no Copy button** (§2.19 refinement). The name is shown in Mono inside the dialog only, as text. It lives in the flow's `done` state and goes when the dialog closes, and it never reaches the URL, the title, a toast, storage or the console. S-filters may raise Copy with the owner.
3. **Block this, tier order and availability:**
   - This torrent: `hash:<v1>`, or `hash:<v1> | hash:<v2 truncated>`. The query language's `hash:` takes 40 hex digits, and the index keeps a v2 hash truncated to 20 bytes.
   - This payload signature and This uploader or group: unavailable, with their reasons. Neither field is on a search hit or the detail yet.
   - A phrase from the name: never preselected. The narrowest available tier is chosen when the dialog opens.
4. **The phrase splitter** separates on whitespace and `. _ - [ ] ( )`. Picked words form one run of consecutive words. Picking next to the run extends it, picking an end word shrinks it, and picking elsewhere starts a new run. `phraseQuery` removes every `"`, collapses whitespace, and refuses an empty phrase or one over 200 UTF-8 bytes.
5. **"Matches now"** is `/search?q=<query>&hidden=true&exact=true&limit=1`'s `total`, `≈` when `total_approx`. It's shown on the form step and again beside Apply, because `Preview` has no matches-now count for a strengthening rule. One request at a time; a stale answer is dropped. A torrent whose hash isn't valid gets its own reason on the disabled torrent tier.
6. **CSX-5 editor: the shape is checked on the client and the rest is left to the server.** Hashes must be 40 hex digits each (separated by spaces, commas or new lines), a payload signature 16 hex digits, other values non-empty, and days whole numbers from 1 to 3,650. The server caps expiry and shadow at 3,650 days, and 0 would expire at once. Strength is offered for keyword entries only.
7. **The two strengths side by side.**
   - An Override preview carries the Neutralize numbers beside it (`assessment.neutralize`). A Neutralize preview carries only its own numbers, so the Override column keeps the last Override numbers the server sent.
   - After the preview, Neutralize stays on offer only when the server previewed it beside Override (it refuses it for some keywords, such as more than one word), or when it's already the strength chosen. The column that will be applied is marked "(chosen)".
   - The strength on screen is the source of truth. Once a preview settles, a different strength re-previews (a switch made mid-preview waits for it), and the typed password is dropped. So the warning and the password are always for the strength chosen. The variant is built from the change that was previewed, so a "Shadow for N days" choice made in the dialog is kept. The switch is disabled while applying.
8. **Initial focus:** the reveal dialog previews as soon as it opens, so focus starts on the title, then moves to the password field. The two editors start on their first control (Bits' default; `ChangeDialog` returns no initial focus while a form step shows). Focus goes back to the invoker only when it's still in the closed dialog or nowhere, so a dialog opened in between keeps it.
9. **Back to the form.** The form-first dialogs (CSX-5, Block this) offer **Back** in review, confirm and failed. It cancels the flow (dropping any typed password) and returns to the form with what was typed. A refused entry (a 400, such as "Neutralize term takes one word") can then be corrected instead of retyped.

## Backend follow-ups

As in part 1 (breakdowns and sealed names in `Assessment`, the CSX-5 contradiction flag, "matches now" in `Preview`, payload signature and release group on results, structured warnings, `SettingsPatch` types).
