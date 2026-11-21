# X-safety-toggles decisions (#1019)

Runner brief, 2026-11-21Z; findings I4, I5, U21 and U32 of the 2026-11-13 UI review (`docs/review/2026-11-13-ui-ux/`), placed by `docs/decisions/2026-11-14-runner-ui-review-placement.md`. Labels and styling only. What a rule, a pack or a switch does is unchanged; every change still goes through the same flow; the child-safety core changes only through the weakening flow (owner §2.39, architecture §20.5.3, §20.11).

## Rule and pack names (I5)
- **Every rule and pack shows a plain name, its code beside it as secondary mono text, and one line saying what it checks.** It applies to the core rows, the packs list, the pack panels' rules, and a pending version's rule. The code stays because the docs and logs name it.
- **The words are en and zh messages keyed by code** (`filters_rule_<code>`, `filters_rule_<code>_text`). The API has no description field (`CoreRuleState`, `PackRuleState`, `PackState`).
  - The packs reuse the Filters screen's own descriptions (`filters_pack_info_*`).
  - The pack names copy the wizard's (`wizard_pack_*`) into filters' own keys (`filters_pack_name_*`). Sharing the wizard's message modules made Rolldown split them into a shared chunk, which cost the wizard's chunk 215 B.
- **MW-6 and UC-2 read the same flag, so their names say which concern each answers:** "Cracked software (malware)" (破解软件（恶意软件）) and "Cracked software (copies)" (破解软件（拷贝）), kept short for the names chunk. Turning either off leaves the other on (owner §2.39).
- **A description says what the rule checks, not what it does to a match.** The action is already a badge on the row, and an override can change it, so the text would drift. Examples:
  - CS-4a: "A word for a child near a sexual term in the name or file names."
  - MW-1: "A program disguised as a media file, such as movie.mp4.exe."
  - The texts follow §20.5 and §20.6 and the rule docs in `crates/filter` (`core.rs`, `packs/mod.rs`).
- **A code with no message shows the code alone,** never an empty label. This covers a rule a later build adds, and a texts chunk that fails or is slow (below).
  - Lookups read own keys only, so a code such as `constructor` is never resolved through the prototype.
- **`rule-text.test.ts` reads the built-in ids from `crates/filter` at test time:**
  - `CoreRule::id`, `PackRule::id` and `Pack::as_str`, each checked against its `ALL` count.
  - It fails, naming the id, when a built-in rule or pack has no name or description, in en or zh, or when a name is left for an id the crate no longer has.
  - It doesn't edit `crates/`.
- **Dialog titles name the rule, not its code.**
  - "Change Child terms near sexual terms" for a core rule and "Change pack: Malware" for a pack. A pack rule gets "Change <name>" from its own message (`filters_packs_rule_title`); it used to share the pack's "Change pack {id}".
  - The rule's description shows under the title, as the dialog's `aria-describedby` text (`ChangeDialog`'s new `description`, passed to `Dialog`).
  - The messages that took `{id}` now take `{name}`, and get the plain name, or the code when there's none.
- **The packs list's "What each pack hides" door is gone** (a UX call, approved by the runner, §2.26). `PackInfo.svelte` and its size entry are removed. Nothing it showed is lost:
  - it listed each pack's id and its `filters_pack_info_*` text;
  - each pack row now shows the same text, with the id beside the name as its code;
  - that includes the malware pack's "(MW-6)" reference and the unauthorised-copy pack's "can't know who holds the rights … Always allow" (owner §2.39).
  - The rule ids that the pack panels list stay there, under each pack's disclosure.

## Loading the texts
- **The texts are one on-demand module, `filters/screens/rule-text.ts`,** and a small holder (`names.svelte.ts`) imports it once. Neither the screen's first view nor the change dialogs carry them (sizes below).
- **The screen starts the import beside `GET /filters`, and shows once both are in,** so a row never changes from its code to its name under the reader.
- **It waits at most 1.5 s for the texts** (runner, on this slice; the 1.5 s is a runner call, §2.26). After a failure, or 1.5 s without them, it shows with the codes, and a late chunk then names the rows.
  - There's no new spinner: the screen keeps its loading skeleton for that time.
  - Tests:
    - `names-failed.test.ts`: a chunk that fails;
    - `names-slow.test.ts`: one that never arrives (fake timers);
    - `names-late.test.ts`: one that arrives after the wait, which names the rows then;
    - `names.test.ts`: the holder.

## Danger style for loosening child safety
- **The rule (runner, on this slice):** a change that loosens child safety uses the danger style; everything else uses the ordinary style, strengthening included. The changes that loosen child safety are:
  - turning a CS rule off, or weakening one;
  - removing a pending floor removal's entries from the floor;
  - Release and False positive on a review-queue row held by a CS rule (its code starts `CS-`, CS-0a included), which §20.5 treats as weakening (runner's review). Confirm, which adds to CS-1, stays ordinary, and so do the same buttons on a pack's hold.
- **Where it shows:**
  - the CS row's "Turn off …" button;
  - a pending floor removal's "Remove from the floor";
  - the queue's Release and False positive on a CS hold;
  - the dialog's Apply for those changes (`Pending.danger`, `ChangeDialog`'s `danger`).
  - All of them use `Button`'s `danger` variant, which is design tokens with no hex. Turning a CS rule back on, pack switches and your own rules keep the ordinary style.
- **Style only.**
  - The change, the preview, the effect warning and the password are exactly as before.
  - Whether a change weakens is still the server's call (`confirm`). The danger flag is the screen's, from what the button does, and never decides whether the password is asked.
  - Tests check each case, and that the password is still asked first.
- **Undo is left ordinary (follow-up).** The screen can't tell what Undo reverses before it runs: it sends `{op: "undo"}`, and the server says what it restores only in the preview. Styling it would mean changing how Undo works, or styling the dialog from the preview's numbers (`core_caught_30d`, `removed_from_floor`), which would apply to every change's dialog. Both are outside this slice, so it's a follow-up for the runner.
- **A UX call to flag (§2.26):** with several CS rules on, the list shows several solid danger buttons, one per rule. `Button` has no outline danger variant, and the brief asks for the danger style on the row button, so it's used as it is.

## Packs collapsed (U32)
- **Each pack's panel (versions, changelog, corpus status, its rules) is collapsed by default.** It sits behind a real `<button aria-expanded aria-controls>`, "Rules and versions of <pack>", with the same caret as the Sources rows.
  - The body stays in the page (`hidden`), so an unsaved override edit survives closing it.
  - A closed pack with a version waiting shows "A newer version is waiting", so collapsing never hides that something needs a decision. The badge describes the closed disclosure button (`aria-describedby`), so a screen reader hears it on the button.
- **The "Version" row, which lists the rule ids, reads "Rule versions"** ("规则版本").
- **Page height at 390 px.** Measured with Playwright at 390 × 844, mock data with all 33 built-in rules in the 10-rule core and 6 packs, every pack on, nothing pending, and the empty queue:

| | Height |
|---|---|
| Before (main ea6ff8c1) | 4,599 px |
| After | 3,473 px (−1,126 px, −24%) |

  The descriptions add height to the core and pack rows; collapsing the six panels takes more away.

## Switches say what on means (I4, U21)
`docs/ui-glossary.md` has no entry for either switch, so it's unchanged (runner).

| Switch | Before | After (en) | After (zh) | Checked when |
|---|---|---|---|---|
| Alert (`PauseToggle`) | "Paused", checked while paused | "Active", aria "Active: <name>" | 启用中, aria "启用中：<name>" | the alert isn't paused |
| Source (`SourceRow`) | "On", aria "<name>: on" | "Enabled", aria "<name>: enabled" | 已启用, aria "<name>：已启用" | the source is enabled (unchanged) |

- **The alert switch's polarity flips** (runner, on this slice), so on means it records new alerts.
  - It still sends the same `PATCH {paused}`: switching it off sends `{paused: true}`, on sends `{paused: false}`.
  - It's still a toggle with no confirmation, with pending, revert and toast as before.
  - The unit and e2e tests check both bodies, and that checked follows `!paused`.
- **zh alert label 启用中, not 已启用:** it reads as "in effect" and pairs with the paused state, 已暂停 (runner's preference, this session's call).
- **The source switch keeps its behaviour and its polarity;** only its text changes. "Enabled" also stops reading as a state ("On") shown unchecked next to the "Off" status.

## Sizes
gzip, main ea6ff8c1 → this branch. The new limits are the runner's calls (§2.26), each the measured size rounded up to 0.01 kB, after everything that can load on demand does.

| Entry | Before | After | Limit before | Limit after |
|---|---|---|---|---|
| Settings FiltersScreen (first view) | 30,879 B | 31,401 B | 31.14 kB | 31.41 kB |
| Filters rule names (on demand) | — | 3,999 B | — | 4.0 kB |
| Filters pack descriptions (on demand) | 1,427 B | removed | 1.6 kB | removed |
| Filters packs list (on demand) | 1,797 B | 1,557 B | 1.85 kB | 1.56 kB |
| Filters pack panels (on demand) | 8,765 B | 7,878 B | 9 kB | 7.88 kB |
| Filters dialogs (on demand) | 35,880 B | 35,898 B | 35.9 kB | 35.9 kB |
| Detail drawer (own JS + CSS) | 28,139 B | 28,141 B | 28.14 kB | 28.15 kB |

- **The first view's +522 B** is the names holder, `RuleLabel` (which the core rows need on first render), the rows' markup and the 1.5 s wait.
- **The Detail drawer's +2 B is chunk-hash and gzip drift that this PR doesn't cause in code.**
  - The drawer's code is unchanged.
  - About 30 other entries moved ±1–10 B the same way.
  - Main had 1 B of headroom there.
- **The packs list and the pack panels fall to their measured sizes,** so the bytes they freed can't be quietly spent.

## Specs
- **The e2e specs that reach into a pack panel open it first:** `filters-packs.keyboard` by keyboard, and `packs.routes` by click. No other spec uses the panels' test ids.

## Left alone
- **The review queue and the exceptions list still show a rule's code** (`Badge {rule}`): the brief covers rule rows, packs and the dialogs that name a rule. Naming them there too is a follow-up for the runner.
