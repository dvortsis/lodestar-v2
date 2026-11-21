# X-prefs-invisible-share

Runner brief, 2026-11-21Z. Slice id `X-prefs-invisible-share`, branch `slice/X-prefs-invisible-share`, PR title `X-prefs-invisible-share: <title>`. The plan entry is in `runner/slices.yaml` (added by #1012); widen its `owns` in your PR to match the files below.

## Why
`crates/lodestar/src/settings/preferences.rs` `invisible` decides whether a remapped single-key shortcut (§10.16) is "one visible character". It is a hand-written list of blocks that differs from the written-out Default_Ignorable table now in `crates/lodestar/src/windows/acl.rs` (`default_ignorable`, from X-win-path-ignorables).
- It misses U+1BCA0–1BCA3, U+1D173–1D17A and U+E0080–E0FFF, all default-ignorable.
- It covers combining marks only in a few blocks. A lone combining mark from other scripts (for example a Devanagari vowel sign) passes as "visible".
- It also includes U+FFF9–FFFB (interlinear annotation marks), which are visible format characters. That is fine for a shortcut key, but the reason should be stated.

A user who remaps a shortcut to such a character gets a key the UI can't show. This is a low-severity UX and correctness issue, not a security finding.

## What
1. **One Default_Ignorable table.** Move it to a module that isn't Windows-only (for example `crates/lodestar/src/text/ignorable.rs`, or wherever fits the crate). acl.rs uses it from there, with its behaviour and its exhaustive equality test unchanged. Keep acl's other sets (`invisible`, `format_or_separator`) where they are, built on the shared table.
2. **Preferences' rule.** A shortcut key is refused if it is a control or whitespace character, default-ignorable, a format character (`lodestar_metainfo::format_char`, if the crate already depends on it), or a combining mark (any Mn/Me/Mc, using a table the workspace already depends on, such as `unicode-normalization`'s `is_combining_mark`).
   - Don't add a new dependency without sending me its name and why first.
   - State in a doc comment what is refused and why.
3. **Tests:** an exhaustive test over the code space that preferences' rule equals its stated definition, plus cases: a Devanagari vowel sign, U+1D173, U+E0080, U+200B and an ordinary letter.
   - Settings that are already saved must still load. If an existing saved shortcut would now be refused, say how it's handled: the default rule for a rejected remap, never a crash, and never silently dropping the other shortcuts. Test it.
4. A decisions file `docs/decisions/2026-11-21-X-prefs-invisible-share.md`.

## Owns
- `crates/lodestar/src/settings/preferences.rs`
- `crates/lodestar/src/windows/acl.rs` (moving the table out only)
- the new shared module and its `mod` line
- tests in those files, or a new test file
- the decisions file
- this slice's `slices.yaml` entry

`crates/lodestar/src/config.rs` and `worker/runtime.rs` are CR-1c's: don't touch them. Send me the list before you edit anything else.

## Bars
`runner/standards.md`. Run the Rust gates from CLAUDE.md before pushing, plus `cargo clippy --target x86_64-pc-windows-gnu -p lodestar` if that target is installed (acl.rs is used on Windows). Otherwise say that CI's Windows job covers it. The review includes /security-review, because settings parsing is untrusted input. Push once per round of fixes, and send "ready <pr> <sha>" when CI is green.
