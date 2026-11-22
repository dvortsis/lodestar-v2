# X-prefs-invisible-share decisions

Slice session, 2026-11-21. A remapped single-key shortcut (§10.16) must be one character the UI can show as the key. `settings/preferences.rs` used to decide that with its own list of blocks, which missed some default-ignorable ranges and most combining marks. It now uses the crate's one Default_Ignorable table and Unicode's own categories. A saved remap that the new rule refuses no longer costs the user their other remaps at start.

- **Who decided:** choices marked "runner" are the runner's calls. The rest are the slice session's, for the runner to confirm.

## One Default_Ignorable table

- **`crates/lodestar/src/text.rs`** (new, not Windows-only) holds `default_ignorable`, moved unchanged from `windows/acl.rs` (X-win-path-ignorables). It is Unicode 15's Default_Ignorable_Code_Point, written out; Unicode 16's is the same.
- **`acl.rs` uses it from there.** Its `invisible` and `format_or_separator` stay where they are, and its behaviour and its exhaustive equality test (`quoted_escapes_every_default_ignorable`, with its own written-out copy of the table) are unchanged.

## What a shortcut key can't be

A key is refused when it is anything but exactly one character, or when that character is:

| Refused                                      | Why                                                                             | Table                                            |
| -------------------------------------------- | ------------------------------------------------------------------------------- | ------------------------------------------------ |
| a control or whitespace                      | not a key that shows                                                            | `char::is_control`, `char::is_whitespace`        |
| default-ignorable                            | renders as nothing, in any font                                                 | `crate::text::default_ignorable`                 |
| a format character (Cf)                      | invisible, or reorders the text around it                                       | `lodestar_metainfo::format_char`                 |
| a combining mark (Mn, Mc, Me), in any script | draws only on the character before it, so alone it's a dotted circle or nothing | `unicode_normalization::char::is_combining_mark` |

- **Newly refused:** U+1BCA0–1BCA3, U+1D173–1D17A, U+E0080–E0FFF, the Cf characters outside the old blocks (U+0600–0605, U+06DD, U+070F, U+0890–0891, U+08E2, U+110BD, U+110CD, U+13430–1343F), and every combining mark outside the old few blocks. For example, the Devanagari vowel signs U+093E (Mc) and U+0941 (Mn) passed before.
- **Kept refused, with the reason now stated: U+FFF9–FFFB,** the interlinear annotation marks, and the other Cf characters some fonts draw (the Arabic number signs U+0600–0605). They mark up the text around them and aren't keys anyone types, so refusing them costs nothing, and one rule ("no Cf") is simpler to state and test than a list of exceptions.
- **No longer refused:** only unassigned code points at the ends of two of the old blocks, U+1AF1–1AFF and U+20F1–20FF (under the locked Unicode 18 tables; a later Unicode may assign some of them as marks, which the table then refuses). Every assigned character the old list refused is still refused, because each one is default-ignorable, Cf or a mark. An unassigned code point is treated like any other character: no keyboard types one, and the old list refused these only because it went by whole blocks.
- **Mixed Unicode versions:** the rule combines the Unicode 15 Default_Ignorable table (the same in 16) and metainfo's Unicode 17 Cf table with Unicode 18 marks. So a Cf character new in Unicode 18 would be accepted until metainfo's table moves on. That is negligible (Unicode 18 adds no Cf a keyboard types), but it's on record.

### The dependencies (runner)

- **`unicode-normalization` 0.1.26** in `crates/lodestar`, the version the lock already holds for filter and search (Unicode 18; the exhaustive test relies on it, so it's pinned at that minimum), for `is_combining_mark` (General_Category M). filter and search already use it.
- **`unicode-properties` 0.1.4** as a dev-dependency only, as the independent reference in the exhaustive test. metainfo already uses it.
- `Cargo.lock` gains the two names in `lodestar`'s list and no new package.

## Saved shortcuts the new rule refuses (runner)

`shortcut_keys` is one field. Before, a saved map that no longer passed its check was ignored whole at start (`settings.saved_ignored`, reason `invalid`), so one stale key dropped every remap.

- **A load-time hook,** `Section::repair_saved(field, value) -> Option<Value>`, runs on each saved value before it's checked at start. The default is `None`, so every other section loads exactly as before (tested).
- **Preferences repairs `shortcut_keys`** whenever the saved map fails `check_shortcuts`, for whatever reason, and keeps the repair only if the result passes it (`repair_shortcuts`):
  - every remap whose key isn't a shortcut key is dropped, so its action has its default key again. That covers a character the new rule refuses, and also a malformed key such as `""` or `"ab"`;
  - then every remap whose key is the default of an action without a remap is dropped, whether that default was just restored or was in effect all along. For example, `{"detail.send": "j", "search.focus": "f"}` drops `detail.send`, because `results.next` still has `j`, and keeps `search.focus`. Before, the whole field was ignored;
  - this repeats until nothing changes. A round only follows one that restored a default, so there are at most nine rounds, each linear in the map;
  - every other remap is kept;
  - what the repair can't fix leaves the field ignored whole, as before, logged once as `invalid`. That means an action id that isn't one, more than 64 remaps, or two remaps sharing a key.
- **Wider than the brief asked, runner-approved (§2.26, UX; a refinement under §2.19).** The repair keeps the still-valid remaps of any saved map that fails the check, not only one the new rule broke. Keeping them is better for users than ignoring them all, and the result always passes `check_shortcuts`.
- **The log:** the existing `settings.saved_ignored` event, with `reason=invalid_entries`, names the section and field and never the value. `reason` is a free label (`Label`), so this needs no catalog, golden or web change, and there's no new event (runner).
- **Only at start, only in memory.** The store keeps the saved text until the field is next changed (a patch equal to the repaired map changes nothing, so writes nothing), and the repair and its log line repeat at each start until then.
- **A patch is unchanged:** a refused key in `PATCH /settings/preferences` is still a `400` naming `shortcut_keys`.

## Tests

- **`preferences::a_shortcut_key_is_one_character_that_shows`** goes over all of `char`. A one-character key is refused exactly when it's a control, whitespace, default-ignorable, Cf or Mn/Mc/Me, where the categories come from `unicode-properties`, not from the tables the rule uses.
  - **The versions differ.** The rule's mark table is Unicode 18 (`unicode-normalization` 0.1.26, the version the lock holds), and the reference is Unicode 17 (`unicode-properties` 0.1.4). So a code point the reference leaves unassigned, and that isn't default-ignorable, is counted, never compared: it may be one of Unicode 18's new marks, such as U+05C8. Fewer than 100 may be refused that way: Unicode 18 has 37 new marks, and a wrong table would refuse whole blocks.
  - The cases: U+0941, U+093E, U+20DD, U+1D173, U+E0080, U+200B and U+FFFA are refused; `a`, `ü`, `/`, `?`, `字` and U+0905 are accepted.
- **`preferences::repair_drops_refused_keys_and_what_their_defaults_displace`** covers nothing to repair, a three-step chain, every remap refused, a remap colliding with a default in effect all along, and malformed keys.
- **`settings::repair_tests`** goes through the registry:
  - a stale key loads with the rest kept and the log names only the field;
  - the store isn't rewritten;
  - the chain;
  - remaps that hold load untouched with no log line;
  - what repair can't fix is ignored as before;
  - a section without a repair (crawler) loads as before;
  - a patch with each newly refused key is refused.
- **`text::edges_are_in_and_neighbours_out`** checks the table's edges. Its exact equality stays `acl`'s test.

## Follow-ups

- **Importing settings** (`settings::portable`) goes through the ordinary save, not the repair, so importing an older build's export that holds a now-refused shortcut fails the whole import, as any invalid value does. That is existing behaviour, left as it is.

- **The web editor's own check,** `web/src/lib/features/shortcuts/capture.ts` `BLANK`, mirrors the old block list. A key it accepts and the server now refuses gets the server's message ("a shortcut is one key"). That is correct, but the editor no longer explains it before the request.
  - The fix is one regex, `/[\p{Cf}\p{M}\p{Default_Ignorable_Code_Point}]/u`.
  - web/src/lib/** is X-ui-guards' right now (runner), so it's left for a small follow-up.
