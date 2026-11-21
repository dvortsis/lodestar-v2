# X-win-path-ignorables decisions

Follow-up to #1004 (X-win-log-path-display), from its fresh review's optional findings. Windows service logs only; the Event Log's rules are unchanged (`one_line`, the 260-character cut, no quotes).

- **One set of invisible characters.** `acl::invisible` was Unicode's Cf, Zl and Zp alone, so `quoted()` let some characters that draw nothing through raw. It now also covers every Default_Ignorable_Code_Point, by calling the filter crate's own set, `lodestar_filter::normalize::invisible`. That function is now `pub`; nothing else changes and its tests are unchanged. The newly covered characters:
  - variation selectors (U+FE00–FE0F, U+E0100–E01EF);
  - the combining grapheme joiner, U+034F;
  - the Hangul fillers (U+115F, U+1160, U+3164, U+FFA0);
  - the Khmer inherent vowels, U+17B4–17B5;
  - the rest of plane 14.

  It also covers the Mongolian free variation selectors U+180B–U+180F directly. They're default-ignorable, but the filter's set has only U+180E (as a format character), and changing the filter's matching is outside this slice. What `acl::invisible` covered before (every Cf, including the visible format marks, and Zl and Zp) stays.
  - The same set reaches the Event Log's `one_line` and `acl`'s own `plain`, which drop these characters rather than escape them. Their rules are unchanged; they now drop the new characters too.
  - `settings/preferences.rs` has its own `invisible`, which is outside this slice. It could share this set; the PR says so, for the runner to brief.
- **The set is pinned.** A test walks all of `char`. Every character the filter's set names, plus U+2028, U+2029 and U+180B–U+180F, is `invisible` and comes out of `quoted()` as `\u{…}`. Ordinary text comes out as it is: Latin, accented, digits, CJK, emoji, a space, a backslash, and combining marks (U+0301, U+05B0).
- **`quoted()` moves to `acl.rs`,** so `acl::secure_dir`'s own errors use it too: "reading …'s permissions", "creating …" and "… vanished after it was created". They showed the folder with plain `Display`, unquoted and unescaped, and reach the temp log as they are. `acl::refusal`'s text keeps its own rules: `plain()` drops what it can't show, so its `icacls "<folder>"` command stays pasteable.
- **The Linux tests catch every call site.** Three reverts to `{:?}` used to fail only on windows-latest, because the tests' paths held no backslash off Windows. They were the config file in the `data_dir` refusal, the config file in the witness and blocklist refusals, and "is a file". Those tests now name the config file `ser\vice.toml` and the file in the way `in\the <U+202E>way`. A backslash is legal in a Linux file name; on Windows it's a separator, so each test creates the parent folder first.
- **The `"{}: {e}"` detail stays untested, because it can't be triggered portably.** It needs the data folder's own lookup to answer NotFound while a parent's `symlink_metadata` fails another way, such as access denied or a loop.
  - Looking up the folder walks through that same parent, so both fail the same way.
  - A parent that is a link returns early.
  - Running as root ignores permissions.

  Making it testable would mean injecting the metadata call, which is more than this slice's visibility changes allow.
- **Mutations, each checked, and each failing a test on Linux:**

  | Mutation | Fails |
  |---|---|
  | `quoted()` without its `invisible` check | 4 tests |
  | `acl::invisible` without the filter's set | `quoted_escapes_every_default_ignorable` |
  | `acl::invisible` without U+180B–U+180F | `quoted_escapes_every_default_ignorable` |
  | `secure_dir`'s folder back to `{:?}` | `secure_dir_errors_quote_the_folder` |
  | the `data_dir` refusal's file back to `{:?}` | `every_refusal_shows_its_path_as_written` |
  | the witness and blocklist refusals' file back to `{:?}` | `every_refusal_shows_its_path_as_written` |
  | "is a file" back to `{:?}` | `the_file_in_the_way_is_quoted` |
