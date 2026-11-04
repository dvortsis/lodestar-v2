# X-log-url-redact decisions

A security fix in `crates/log/src/url.rs`, the URL credential scrubbers (architecture §12.1, B12's "For the logging slice"). SRC-3's session found it when the proptest `planted_secrets_never_get_through` failed once on CI. The shrunk input was `://@http://u:a00aa0@host.example/x`, and `scrub_text` let the password through.

## Cause
- **`scrub_text` took one URL per `://` but searched only the first one's userinfo.** A bare `://` followed by `@` (`://@`) found that `@` as its own userinfo. Its URL then ran on through `http://u:<secret>@host…` to the first character that can't be in a URL. That whole span went to `scrub_with` as a single URL, which never searched the second `://`. The real password ended up in a path segment, where only passkey-like runs are hidden.
- **`scrub` and `scrub_with` had the same cause on their own.** The userinfo runs to the last `@` before the first `/` after the first `@`. So when an earlier `@` ended the authority, a URL in the path kept its userinfo:
  - `http://a@h/r/https://u:p@h2/` showed `p`;
  - `/r/https://u:p@h2/` in request form showed `p`;
  - a URL in a query pair with no `=` (`/x?a&https://u:p@h2/`) showed `p` too.
- **`scrub_debug_forms` had the same pattern.** A stray `path: "` before a field with no quote in between (`path: "a fragment: Some("f")`) took that field into its own value. The field was never read, so its value stayed.
- **The bug is deterministic.** It looked intermittent only because the old generator (`[ -~]{0,40}`) rarely produced that prefix.

## Fix
- **Each `://` in free text gets its own URL, with its own userinfo search, bounded by the next `://`.** A later `://` before this URL's query or fragment ends the URL where that one's scheme starts, so the next pass searches its userinfo, the bounded look-ahead included.
  - A `://` inside a query or fragment stays in the outer URL, which loses both, as before. Splitting there would show what followed the nested URL (`?u=https://h2/&token=t` would keep `token=t` as a path).
  - The first scan stops at that next `://`. A line of `://@`, `a://` or `magnet:` stays linear. `scrub_text_is_linear` now covers `://@` and `://@http://u:p@h/`. A new test runs 1 MiB of three adversarial units, best of three runs, with a 1 s bound each. Each takes about 0.07 s in a debug build.
- **A scheme goes through the passkey rules.** In free text, what ends up as a scheme may be the tail of the text before it (`/dl/<passkey>.torrent://`). A real scheme is never passkey-like. The strengthened proptest found this one once URLs were split at a nested `://`.
- **An `@` in the path, once the authority is off, hides the whole path and its query** (`http://[redacted]@h/[redacted]`). It's the rule the authority already applied to an `@` after a `/` (`https://registry.npmjs.org/@scope/pkg` shows as `https://[redacted]`), carried past an earlier `@`. Over-redaction is accepted ("never too little"). A path without an `@` is unchanged.
- **A query pair with no `=` that holds a URL is redacted,** as a value holding one already was.
- **A Debug-form path that holds another field's name at a word boundary goes from there,** and scanning starts again at that field, so its value is scrubbed by its own rule.
- **Every existing test passes unchanged.**

## Tests
- Deterministic tests, each failing on `main` before the fix:
  - `a_stray_scheme_separator_never_hides_a_later_url`, with the exact shrunk input first;
  - `an_at_in_a_path_never_keeps_a_url_s_userinfo`;
  - `a_stray_debug_field_never_keeps_a_later_one`.
- **`planted_secrets_never_get_through` is strengthened:**
  - prefixes come from printable ASCII, from an alphabet weighted 3:1 towards `:`, `/`, `@`, `?`, `#`, `[` and `]`, or from that junk followed by `://@`;
  - secrets are planted in userinfo, in query credentials, and in passkey path segments (`/ann/<pk>/announce`, `/dl/<pk>.torrent`);
  - it runs 1,024 cases.
- **A new property, `a_url_after_junk_in_a_path_keeps_no_secret`,** covers `scrub` in both forms, with junk before a nested URL.
- **`crates/log/proptest-regressions/url.txt` is new** (other crates already use these files). Its seed fails both properties on the unfixed code, and `planted_secrets_never_get_through` shrinks it to `before = "://@"`.
- **300,000 cases of each property pass locally** in a release build.

## Released builds
- **v0.5.1-rc.1, rc.2 and rc.3 all carry the bug.** `crates/log/src/url.rs` is byte-identical at all three tags, on `release/v0.5.1` and on `main` before this fix.
- **It's a log-redaction leak, class B, with no crawler change.** It shows a credential in a log line only when the line holds a stray `://` and `@` before a credentialed URL, or a URL behind an earlier `@`. The runner decides whether it's backported to `release/v0.5.1`. The change is this one file.
