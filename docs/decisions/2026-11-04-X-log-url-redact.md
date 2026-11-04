# X-log-url-redact decisions (#432)

A security fix in `crates/log/src/url.rs`, the URL credential scrubbers (architecture §12.1, B12's "For the logging slice"). SRC-3's session found it when the proptest `planted_secrets_never_get_through` failed once on CI. The shrunk input was `://@http://u:a00aa0@host.example/x`, and `scrub_text` let the password through.

## Cause
- **`scrub_text` took one URL per `://` but searched only the first one's userinfo.** A bare `://` followed by `@` (`://@`) found that `@` as its own userinfo. Its URL then ran on through `http://u:<secret>@host…` to the first character that can't be in a URL. That whole span went to `scrub_with` as a single URL, which never searched the second `://`. The real password ended up in a path segment, where only passkey-like runs are hidden.
- **`scrub` and `scrub_with` had the same cause on their own.** The userinfo runs to the last `@` before the first `/` after the first `@`. So when an earlier `@` ended the authority, a URL in the path kept its userinfo:
  - `http://a@h/r/https://u:p@h2/` showed `p`;
  - `/r/https://u:p@h2/` in request form showed `p`;
  - a URL in a query pair with no `=` (`/x?a&https://u:p@h2/`) showed `p` too.
- **`scrub_debug_forms` had the same pattern.** A stray `path: "` before a field with no quote in between (`path: "a fragment: Some("f")`) took that field into its own value. The field was never read, so its value stayed.
- **Raw URL syntax in a password could cut the userinfo search short,** on `main` too. The fresh self-review and the strengthened proptest found these:
  - a password holding `://` (`http://u:pa]://x@h/`, `ftp://u:pa/ss://@h/`): the search stopped at that `://`;
  - a raw `@` then `/` (`http://u:@/'p@h/`): the text-side search took the first `@`;
  - a URL whose query holds a later one (`://?://@http://u:pa ss@h/`): the later URL's look-ahead was skipped.
- **`scrub_text` could panic on a multi-byte character** 63 bytes after an IPv6 `[` (`see http://[` + 62 × `a` + `é`). That's on `main` and in rc.3 too. Log text can come from outside (tracker and peer URLs, error strings), so an outsider could make a log line panic.
- **The bug is deterministic.** It looked intermittent only because the old generator (`[ -~]{0,40}`) rarely produced that prefix.

## Fix
- **Each `://` in free text gets its own URL, with its own userinfo search, bounded by the next `://`.** A later `://` before this URL's query or fragment ends the URL where that one's scheme starts, so the next pass searches its userinfo, the bounded look-ahead included.
  - A `://` inside a query or fragment stays in the outer URL, which loses both, as before. Splitting there would show what followed the nested URL (`?u=https://h2/&token=t` would keep `token=t` as a path).
  - **With no userinfo found before it, that `://` may be inside a raw password.** That's when only URL characters or a soft end (`)`, `]`, `,`, `'`, `}`) lie between the two. Everything between the two `://` then goes (`http://[redacted]://[redacted]@h/`), and the next URL starts at the `://` itself. A redirect path such as `http://h/r/https://h2/b` becomes `http://[redacted]://h2/b`. That over-redaction is accepted, since the two can't be told apart.
  - **A URL whose query or fragment holds a later `://` runs on over it,** and each later `://` still gets its own bounded userinfo search, so the span ends past its password.
  - **The text-side userinfo is the last `@` before a hard stop,** not the URL rule's first-`@`-then-`/`. That's wider, and `scrub_with` then hides what's ambiguous.
  - The first scan stops at that next `://`. A line of `://@`, `a://` or `magnet:` stays linear. `scrub_text_is_linear` now covers `://@` and `://@http://u:p@h/`. A new test runs 1 MiB of three adversarial units, best of three runs, with a 1 s bound each. Each takes about 0.07 s in a debug build.
- **A scheme goes through the passkey rules.** In free text, what ends up as a scheme may be the tail of the text before it (`/dl/<passkey>.torrent://`). A real scheme is never passkey-like. The strengthened proptest found this one once URLs were split at a nested `://`.
- **An `@` in the path, once the authority is off, hides the whole path and its query** (`http://[redacted]@h/[redacted]`). That includes the request form: `/api/@me/x?token=1` shows as `/[redacted]`. It's intended over-redaction, not a bug. It's the rule the authority already applied to an `@` after a `/` (`https://registry.npmjs.org/@scope/pkg` shows as `https://[redacted]`), carried past an earlier `@`. Over-redaction is accepted ("never too little"). A path without an `@` is unchanged.
- **A query pair with no `=` that holds a URL is redacted,** as a value holding one already was.
- **The IPv6 look-ahead's bound steps back to a char boundary,** as the userinfo look-ahead's already did.
- **A Debug-form value that holds another field's name at a word boundary is scrubbed whole by its own rule,** and scanning starts again at that field, so its value is scrubbed by its own rule too. A Debug path holding an `@` is hidden, as `finish` hides one.
- **The security review and a differential fuzz against `main` found more places where splitting at a nested `://` showed what `main` hid.** Each is fixed:
  - **A host goes through the passkey and `name=value` rules.** After a split, a path segment can sit in host position (`/https://<passkey>/announce`, `@passkey=<k>`). A real host label is never passkey-like or `name=value`.
  - **Path segments go through the `name=value` rules too** (`/dl/passkey=<k>`).
  - **A passkey-like run is any run of letters, digits, `-` and `_` between other characters,** not only between `.`, `:`, `,`, `=`, `+` and `~`, so `[<passkey>&` loses it.
  - **The scheme back-up includes `_`,** so a passkey glued to a scheme (`/dl/<pass_key>http://`) is seen whole.
  - **The text before a URL and its scheme are scrubbed together as plain text,** so a credential's value glued to `://` (`passkey=<k>://`, `token=<t>://h/`) isn't taken for a scheme. That's on `main` too.
  - **A scheme run right after `=` stays with the URL before it** (`@passkey=<k>://`), and the next URL has no scheme.
- **Every existing test passes unchanged.**

- **The runner's review (round 2) found two more leaks this change introduced, and one on `main` too.** All three are fixed:
  - **A Debug-form value never takes a `://` with it.** When a value holds one (`username: "x username: "http://u:"S3cr3t@h/x`, a raw `"` in a URL's password), the field's rule stops at that URL's scheme. `scheme://` stays and the rest of the value goes, so the URL rules still find the `@` after the quote.
  - **What was read as userinfo or host is never read again as a scheme.** A nested `://` with no `/` between the userinfo and it is in this URL's authority (`ws://u:?@S3cr3t9://@h:80`), so everything up to it goes.
  - **Splitting trusts only known schemes:** `http`, `https`, `ws`, `wss`, `udp`, `tcp`, `ftp`, `ftps`, `sftp`, `socks4`, `socks4a`, `socks5`, `socks5h` and `file`. Any other run before a nested `://` may be a password's tail (`u:@/<secret>://@h/`), so it's hidden with what comes before it.
  - **An `@` anywhere after the authority hides the host and everything after it** (`http://u:@S3cr3t/@h/` shows as `http://[redacted]`). That's on `main` too. It extends the path rule above: `http://a@h/r/https://u:p@h2/x` now shows as `http://[redacted]`, and `http://u:p@h/x?email=a@b` loses its host as well.
  - **A magnet link once ended before a URL glued to it. Round 4 reverses that** (below): it showed tracker hosts, against B-log1.
- **Round 3 (runner): nothing `main` hides may show, even where `main` hid it by luck.** The fix is in the wide look-ahead:
  - **When there's userinfo, the wide look-ahead runs past a `/`,** to the line's end within its 512 bytes. Userinfo means an `@` before the first hard stop, or a `:` in the authority with no port number after it (`u:/p"@h/`, `u:@/p"@h/`).
  - So a password holding a raw `/` and a hard stop is read whole, and the rule that an `@` after the authority hides the rest covers what follows.
  - Without such a sign the window still stops at `/`, so `see https://a.example/p and me@x` keeps its path.
  - **The cost is over-redaction.** A credentialed URL followed by any `@` within 512 bytes loses its host, its path and the text between. Since round 4, that includes an `@` on a later line. Example: `warn: tracker udp://u:p@t.example:6969/announce failed: timed out (peer@1.2.3.4 said no)` shows as `warn: tracker udp://[redacted] said no)`. That's accepted ("never too little"). A URL without userinfo is unaffected (`see https://a.example/p and me@x` keeps its path), and so are a `host:port` with a numeric port (`http://localhost:8191/`) and an IPv6 literal.
  - **A Debug value's URL is written as `scheme://[redacted]@`,** which marks it as userinfo for the same rule.
- **Round 4 (runner's fresh review):**
  - **Magnet links are never split** (B-log1, `docs/decisions/2026-10-23-B-log1.md`: "Magnet links are never shown"). Round 2's split showed the tracker host, and sometimes the torrent's name, of a raw `tr=` URL (`magnet:?xt=…&dn=Some.Movie.2020.1080p&tr=http://t.example/announce`). That loosened a security decision.
    - Now a URL glued to a magnet stays inside it. Each `://` the magnet runs over gets its userinfo searched, so the magnet ends past any raw password, and the whole span shows as `magnet:[redacted]`.
  - **A Debug value's backed-over scheme is written only if it's a known scheme,** and as `[redacted]` otherwise (`query: Some("token=<secret>://h/")`).
  - **A `:` with no port number, with no `@` found, is a password** (`http://u:<secret>/x` shows as `http://[redacted]`). An IPv6 literal's `:` doesn't count.
  - **With a userinfo `@` found before the first hard stop, the wide look-ahead also crosses line breaks** (`u:&@<secret>` and a newline before the real `@`). It's still bounded by 512 bytes and the next `://`. The over-redaction is in the round 3 entry above.
- **Over-redaction in a common log shape:** a comma-separated tracker list loses all but its last tracker. `udp://a.example:80/announce,udp://b.example:6969/announce` shows as `udp://[redacted]://b.example:6969/announce`. Each `://` with nothing but URL characters or a soft end before the next one is read as possibly inside a password. That's accepted ("never too little").

## Tests
- Deterministic tests, each failing on `main` before the fix:
  - `a_stray_scheme_separator_never_hides_a_later_url`, with the exact shrunk input first;
  - `an_at_in_a_path_never_keeps_a_url_s_userinfo`;
  - `a_stray_debug_field_never_keeps_a_later_one`.
- **`planted_secrets_never_get_through` is strengthened:**
  - prefixes come from printable ASCII, from an alphabet weighted 3:1 towards `:`, `/`, `@`, `?`, `#`, `[` and `]`, or from that junk followed by `://@`;
  - secrets are planted in userinfo, in query credentials, and in passkey path segments (`/ann/<pk>/announce`, `/dl/<pk>.torrent`);
  - passwords also carry URL syntax before or after the secret: runs of `:/@?#,')[]`, a `://` with junk around it, or hard stops (space, `"`, `<`, `>`, a backtick);
  - it runs 1,024 cases.
- **A new property, `a_url_after_junk_in_a_path_keeps_no_secret`,** covers `scrub` in both forms, with junk before a nested URL.
- **A new property, `scrubbing_never_panics_on_url_tokens`,** builds input from URL and Debug-form tokens, which `\PC` almost never produces.
- **`scrubbing_never_panics_on_odd_input` gains the IPv6 boundary case,** which panics on `main`.
- **`crates/log/proptest-regressions/url.txt` is new** (other crates already use these files). Its seed fails both properties on the unfixed code, and `planted_secrets_never_get_through` shrinks it to `before = "://@"`.
- **The runner's inputs are deterministic tests in `a_stray_scheme_separator_never_hides_a_later_url`,** along with the shapes the differential fuzz found (a Debug value holding a URL, an `@` in the query, a URL glued to `magnet:`). The proptest's passwords now carry URL syntax on both sides of the secret.
- **Round 4 tests:**
  - `a_magnet_link_is_never_shown_in_part` covers raw `tr=` URLs: http, udp, two trackers, and a glued `dn`.
  - `a_debug_value_glued_to_a_scheme_keeps_no_secret` covers the reviewer's three Debug inputs.
  - `a_port_that_is_no_number_is_a_password` covers fix B.
  - The property `debug_values_and_magnets_keep_no_secret` builds Debug `query`/`fragment` values shaped `{name}={secret}{glue}{scheme}://…`, and magnets shaped `magnet:?xt=…&dn={secret}&tr={scheme}://{host}/announce`. It asserts the magnet shows only as `magnet:[redacted]`.
  - Each of these fails on round 3's code.
- **1,000,000 cases of each property pass locally** in a release build, and 300,000 on the final code.
- **Differential fuzz against `main`, run locally (not committed).** Two generators, both re-run on round 4's code:
  - **Mine** puts a credentialed URL (`http://u:{a}<secret>{b}@h.example/x`), a `passkey=` query, a passkey path, or a Debug `Url` among random URL and Debug-form tokens. `{a}` and `{b}` come from `: / @ ? # :// " ' ) ] @/ ?@ :/@`.
    - It leaves out: percent-encoded userinfo, raw newlines and tabs in passwords, IPv6 and odd hosts, non-`http` schemes, magnets with raw `tr=` URLs, and Debug `query`/`fragment` values.
    - **Round 4, seed 11, 3,000,000 inputs:** 0 shown by this PR but hidden by `main`. This PR shows 3,593 in all; `main` shows 91,523.
  - **The reviewer's** (`reviews/432/review-diff-harness.rs` on `runner-state`) covers what mine leaves out: 11 schemes, `%3A`/`%40`/`%2F`, newlines and tabs, IPv6 and `host:abc` hosts, `tr=` magnets, Debug query and fragment values, and two URLs per line.
    - **Round 4, seed 2:** 14 shown by this PR but hidden by `main` at 300,000 inputs (this PR shows 15,581 in all, `main` 52,106). At 1,000,000 inputs it's 46 (this PR 52,143, `main` 174,059).
    - At round 3 it was 372 at 300,000, and about 1,235 at 1,000,000.
    - No panics in this PR; `main` panics on 48 of the 1,000,000 inputs (the IPv6 case).
  - **The 46 left all hold a `://` inside the password, after the secret** (`udp://h/xudp://u:@<secret>"://@h/x`), so the userinfo search, bounded by the next `://`, can't reach the real `@`. 41 also hold a raw hard stop, and 21 a percent-encoded userinfo (`%3A`). `main` hid them only because an earlier span swallowed them. Closing them means letting the userinfo search cross the next `://`, the bound that keeps the scan linear. That's reported to the runner, which decides whether they go to X-log-url-redact-2.
- **Earlier runs of my generator, for the record** (3,000,000 inputs each). Each input put a well-formed credentialed URL, a passkey path or a Debug-form `Url` among random URL and Debug-form tokens. After the security-review round, `scrub_text` showed the planted secret in none of them. `main` panicked on some of them (the IPv6 case).
  - **After the runner's round, a wider run** put URL syntax on both sides of the password and added Debug `username` fields. `scrub_text` showed the planted secret in 13,890 of the 3,000,000 inputs; `main` showed it in more of them.
  - **418 of those were inputs where `main` happened to hide the secret.** Every one was a password holding both a raw `/` and a hard stop (`http://u:@/<secret>"@h/`). `main` hid them only because a stray Debug field or a URL ahead of them swallowed the span. Round 3 fixes them.
  - **After round 3, the same run** (seed 11, 3,000,000 inputs) has 0 secrets shown by this PR but hidden by `main`. This PR shows 3,170 in all; `main` shows 91,523.
  - **Seeds 12 and 13 (3,000,000 each) and seed 99 (10,000,000) also have 0.** This PR shows 3,147, 3,121 and 10,532; `main` shows 91,108, 90,883 and 305,788.

## Known gaps, for X-log-url-redact-2 (the runner is briefing it)
All of these leak on rc.3 as well.
- **A raw hard stop with a `/` in a password when nothing marks the userinfo** (`http://u/<secret>"@h/`, no `:` or `@` before it), and one with a later `://` (`http://u:pa ss://@h/x`). The wide look-ahead stops at that `/` and at the next `://`, which keeps the scan linear. - **Accepted by the runner (2026-11-04, §2.33; flagged to the owner under §2.19):** the 46 per 1,000,000 the reviewer's harness still finds at seed 2 on round 4 (Tests, above) go to X-log-url-redact-2.
  - **The shape:** each holds a password that itself contains `://` after the secret (`udp://h/xudp://u:@<secret>"://@h/x`), and most also hold a raw hard stop or a percent-encoded userinfo.
  - **Why:** `main` hides them only by luck, and closing them would break the bound that keeps the scan linear (the userinfo search stops at the next `://`). Against that, this PR shows about 3.3× fewer secrets overall on the same harness and removes `main`'s panics (48 per 1,000,000 inputs).
- **A mixed encoding such as `http:%2F%2Fu:p@h`,** which `holds_url` misses. In `scrub` it's a kept query value; in `scrub_text`, URL forms without `://` aren't found at all.
- **IPv6 hosts in URLs nested in a query or fragment, or after `magnet:`.**
- **Credential values glued on with `:` or `%3D`.**
- **URL forms without `://`.**

## Released builds
- **v0.5.1-rc.1, rc.2 and rc.3 all carry the bug.** `crates/log/src/url.rs` is byte-identical at all three tags, on `release/v0.5.1` and on `main` before this fix.
- **The IPv6 char-boundary panic is in rc.3 as well.**
- **It's a log-redaction leak, class B, with no crawler change.** It shows a credential in a log line only when the line holds a stray `://` and `@` before a credentialed URL, or a URL behind an earlier `@`. The runner decides whether it's backported to `release/v0.5.1`. The change is this one file.
