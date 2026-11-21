# X-prefs-invisible-share decisions

Slice session, 2026-11-21. A remapped single-key shortcut (§10.16) must be one character the UI can show as the key. `settings/preferences.rs` used to decide that with its own list of blocks, which missed some default-ignorable ranges and most combining marks. It now uses the crate's one Default_Ignorable table and Unicode's own categories. A saved remap that the new rule refuses no longer costs the user their other remaps at start.

- **Who decided:** choices marked "runner" are the runner's calls. The rest are the slice session's, for the runner to confirm.

## One Default_Ignorable table

- **`crates/lodestar/src/text.rs`** (new, not Windows-only) holds `default_ignorable`, moved unchanged from `windows/acl.rs` (X-win-path-ignorables). It is Unicode 15's Default_Ignorable_Code_Point, written out; Unicode 16's is the same.
- **`acl.rs` uses it from there.** Its `invisible` and `format_or_separator` stay where they are, and its behaviour and its exhaustive equality test (`quoted_escapes_every_default_ignorable`, with its own written-out copy of the table) are unchanged.

## What a shortcut key can't be

A key is refused when it is anything but exactly one character, or when that character is:

| Refused | Why | Table |
|---|---|---|
| a control or whitespace | not a key that shows | `char::is_control`, `char::is_whitespace` |
| default-ignorable | renders as nothing, in any font | `crate::text::default_ignorable` |
| a format character (Cf) | invisible, or reorders the text around it | `lodestar_metainfo::format_char` |
| a combining mark (Mn, Mc, Me), in any script | draws only on the character before it, so alone it's a dotted circle or nothing | `unicode_normalization::char::is_combining_mark` |

- **Newly refused:** U+1BCA0–1BCA3, U+1D173–1D17A, U+E0080–E0FFF, the Cf characters outside the old blocks (U+0600–0605, U+06DD, U+070F, U+0890–0891, U+08E2, U+110BD, U+110CD, U+13430–1343F), and every combining mark outside the old few blocks. For example, the Devanagari vowel signs U+093E (Mc) and U+0941 (Mn) passed before.
- **Kept refused, with the reason now stated: U+FFF9–FFFB,** the interlinear annotation marks, and the other Cf characters some fonts draw (the Arabic number signs U+0600–0605). They mark up the text around them and aren't keys anyone types, so refusing them costs nothing, and one rule ("no Cf") is simpler to state and test than a list of exceptions.
- **No longer refused:** only the unassigned code points at the ends of two of the old blocks: U+1ACF–1AFF and U+20F1–20FF (Unicode 15.1; a later Unicode may assign some of them as marks, which the table then refuses). Every assigned character the old list refused is still refused, because each one is default-ignorable, Cf or a mark. An unassigned code point is treated like any other character: no keyboard types one, and the old list refused these only because it went by whole blocks.

### The dependencies (runner)

- **`unicode-normalization` 0.1.24** in `crates/lodestar`, for `is_combining_mark` (General_Category M). filter and search already use the same version.
- **`unicode-properties` 0.1.4** as a dev-dependency only, as the independent reference in the exhaustive test. metainfo already uses it.
- `Cargo.lock` gains the two names in `lodestar`'s list and no new package.

## Saved shortcuts the new rule refuses (runner)

`shortcut_keys` is one field. Before, a saved map that no longer passed its check was ignored whole at start (`settings.saved_ignored`, reason `invalid`), so one stale key dropped every remap.

- **A load-time hook,** `Section::repair_saved(field, value) -> Option<Value>`, runs on each saved value before it's checked at start. The default is `None`, so every other section loads exactly as before (tested).
- **Preferences repairs `shortcut_keys`** only when the saved map fails `check_shortcuts`:
  - it drops each remap whose key is now refused, so that action has its default key again;
  - a restored default can be a key another remap took, so a remap that now shares a key with a restored default is dropped too, repeated until nothing changes (at most one round per remap);
  - the other remaps are kept;
  - whatever is still wrong (two remaps sharing a key, which no build could save) is left to the check, which ignores the field as before.
- **The log:** the existing `settings.saved_ignored` event, with `reason=invalid_entries`, names the section and field and never the value. `reason` is a free label (`Label`), so this needs no catalog, golden or web change, and there's no new event (runner).
- **Only at start, only in memory.** The store keeps the saved text until the field is next saved, and the repair runs again at each start until then.
- **A patch is unchanged:** a refused key in `PATCH /settings/preferences` is still a `400` naming `shortcut_keys`.

## Tests

- **`preferences::a_shortcut_key_is_one_character_that_shows`** goes over all of `char`. A one-character key is refused exactly when it's a control, whitespace, default-ignorable, Cf or Mn/Mc/Me, where the categories come from `unicode-properties`, not from the tables the rule uses.
  - The cases: U+0941, U+093E, U+20DD, U+1D173, U+E0080, U+200B and U+FFFA are refused; `a`, `ü`, `/`, `?`, `字` and U+0905 are accepted.
- **`preferences::repair_drops_refused_keys_and_what_their_defaults_displace`** covers nothing to repair, a three-step chain, and every remap refused.
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

- **The web editor's own check,** `web/src/lib/features/shortcuts/capture.ts` `BLANK`, mirrors the old block list. A key it accepts and the server now refuses gets the server's message ("a shortcut is one key"). That is correct, but the editor no longer explains it before the request.
  - The fix is one regex, `/[\p{Cf}\p{M}\p{Default_Ignorable_Code_Point}]/u`.
  - web/src/lib/** is X-ui-guards' right now (runner), so it's left for a small follow-up.
