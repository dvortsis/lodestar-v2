# X-win-path-ignorables decisions

Follow-up to #1004 (X-win-log-path-display), from its fresh review's optional findings. Only the temp log changes. The Event Log line (`one_line`, the 260-character cut, no quotes) and `acl`'s `icacls` command are as they were.

- **One set of invisible characters.** `acl::invisible` was Unicode's Cf, Zl and Zp alone, so `quoted()` let some characters that draw nothing through raw. It now also covers every Default_Ignorable_Code_Point, from `acl::default_ignorable`, the property as DerivedCoreProperties.txt (Unicode 15) publishes it. The newly covered characters:
  - variation selectors (U+FE00–FE0F, U+E0100–E01EF);
  - the combining grapheme joiner, U+034F;
  - the Hangul fillers (U+115F, U+1160, U+3164, U+FFA0);
  - the Khmer inherent vowels, U+17B4–17B5;
  - U+2065 and U+FFF0–FFF8;
  - the rest of plane 14 (U+E0000–E0FFF).

  - the Mongolian free variation selectors, U+180B–U+180F.

  What `acl::invisible` covered before (every Cf, including the visible format marks, and Zl and Zp) stays.

- **The filter crate isn't touched; `acl` carries its own copy** (the runner's call on #1007).
  - **Why:** reusing `lodestar_filter::normalize::invisible` meant making it `pub`. Any change to `crates/filter` runs the §20.8 filter-budget job, and that job is red on `main`: it fails locally on e8e16420 too, with the mixed 10,000-rule cases over 50 µs. A separate filter-perf slice the runner schedules deals with that, and this slice doesn't wait for it.
  - **The filter's own copy:** its private `normalize::invisible` lists U+180E but not U+180B–180D or U+180F. The runner's review of #1007 found that's a doc and test gap, not a matching bug: those marks are dropped earlier, before `ignored()`. The runner has queued it as a filter slice, and it's unchanged here.
  - **How the copy is pinned:** against a table written into the test from the Default_Ignorable_Code_Point section of DerivedCoreProperties.txt (Unicode 15; Unicode 16's is the same), not against the filter's behaviour. The filter's public entry points (`tokens`, `paths`) read invisibles through its tokenizer, where visible format marks read both ways and other rules apply, so they don't expose the set on its own.
  - **Only the temp log uses the wider set.** The old list is now `acl::format_or_separator` (Cf, Zl, Zp), and `plain()` and the Event Log's `one_line` keep using it. So both drop exactly what they did before (tested).
    - `plain()` builds the pasteable `icacls "<folder>"` command in `acl::refusal`. Dropping a variation selector there would have named a folder that doesn't exist, such as `D:\Music ❤` for `D:\Music ❤️`. The self-review caught this.
    - Keeping `one_line` unchanged also keeps the brief's "no change to the Event Log line". So for a folder holding one of the new characters, the two logs differ: the temp log escapes it, the Event Log shows it as it is.
  - `settings/preferences.rs` has its own `invisible`, which is outside this slice. It could share this set; the PR says so, for the runner to brief.
- **The set is pinned exactly.** A test walks all of `char` and checks that `invisible` is true for exactly every Cf character (`lodestar_metainfo::format_char`, visible format marks included), U+2028, U+2029, and the written-out Default_Ignorable_Code_Point table. Each of those comes out of `quoted()` as `\u{…}`. Ordinary text comes out as it is: Latin, accented, digits, CJK, emoji, a space, a backslash, and combining marks (U+0301, U+05B0).
- **`quoted()` moves to `acl.rs`,** with its comment reworded so `acl.rs` names no Event Log writer: `tests/legal_facts.rs` scans for those words, and only the helper and the service may write there (Legal Notice section 5). The move lets `acl::secure_dir`'s own errors use it too: "reading …'s permissions", "creating …" and "… vanished after it was created". They showed the folder with plain `Display`, unquoted and unescaped, and reach the temp log as they are. The system's own error in them (on Windows, PowerShell's stderr, which echoes paths and entry names over several lines) goes through `acl::escaped`, the same escaping without the quotes. It comes out on one line, with CR, LF and any invisible character as `\u{…}`, so a name someone planted in the folder can't forge a line or reorder the text (the runner's review of #1007). `acl::refusal`'s text keeps its own rules: `plain()` drops only Cf, Zl and Zp, so its `icacls "<folder>"` command still names the folder as it is.
- **The Linux tests catch every call site.** Three reverts to `{:?}` used to fail only on windows-latest, because the tests' paths held no backslash off Windows. They were the config file in the `data_dir` refusal, the config file in the witness and blocklist refusals, and "is a file". Those tests now name the config file `ser\vice.toml` and the file in the way `in\the <U+202E>way`. A backslash is legal in a Linux file name; on Windows it's a separator, so each test creates the parent folder first.
- **The `"{}: {e}"` detail stays untested, because it can't be triggered portably.** It needs the data folder's own lookup to answer NotFound while a parent's `symlink_metadata` fails another way, such as access denied or a loop.
  - Looking up the folder walks through that same parent, so both fail the same way.
  - A parent that is a link returns early.
  - Running as root ignores permissions.

  Making it testable would mean injecting the metadata call, which is more than this slice's visibility changes allow.

- **Mutations, each checked, and each failing a test on Linux:**

  | Mutation                                                        | Fails                                           |
  | --------------------------------------------------------------- | ----------------------------------------------- |
  | `quoted()` without its `invisible` check                        | 4 tests                                         |
  | `default_ignorable` without the variation selectors U+FE00–FE0F | `quoted_escapes_every_default_ignorable`        |
  | `default_ignorable` without U+180B–U+180F                       | `quoted_escapes_every_default_ignorable`        |
  | `secure_dir`'s folder back to `{:?}`                            | `secure_dir_errors_quote_the_folder`            |
  | `secure_dir`'s inspect error back to raw `{e}`                  | `secure_dir_errors_escape_the_systems_own_text` |
  | `secure_dir`'s create error back to raw `{e}`                   | `secure_dir_errors_quote_the_folder`            |
  | the `data_dir` refusal's file back to `{:?}`                    | `every_refusal_shows_its_path_as_written`       |
  | the witness and blocklist refusals' file back to `{:?}`         | `every_refusal_shows_its_path_as_written`       |
  | "is a file" back to `{:?}`                                      | `the_file_in_the_way_is_quoted`                 |
  | a visible Cf mark (U+0600–0605) dropped from the old list       | `quoted_escapes_every_default_ignorable`        |
  | `plain()` on the wider set                                      | `plain_keeps_what_only_the_temp_log_escapes`    |
  | `one_line` on the wider set                                     | `the_event_log_line_keeps_its_own_set`          |
