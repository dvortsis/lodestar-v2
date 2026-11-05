# X-log-url-redact-2 decisions

A security fix in `crates/log/src/url.rs`, the URL credential scrubbers (architecture §12.1, B12's "For the logging slice", B-log1). It closes the classes that X-log-url-redact (#432) found and left out of scope ("Known gaps for X-log-url-redact-2" in `docs/decisions/2026-11-04-X-log-url-redact.md`). Each one leaks on `main` and on every release so far. The rule is the brief's: never too little. Over-redaction is accepted when it's named here, and nothing `main` hides may show.

## Fix, by class
- **A password holding URL syntax, with the URL standing alone** (class 1). This one needed the userinfo look-ahead to cross the next `://`.
  - **Cause.** The look-ahead stopped at the next `://`, because that bound kept `scrub_text` linear.
  - **Fix.** An index of every `@` (`Ats`) gives the last `@` in a window in constant time. So the window can now run over any number of `://` and stay linear.
  - **When there's a sign of userinfo, the window crosses `://` and line breaks too**, within its 512 bytes. A sign is any of:
    - an `@` before the first hard stop;
    - a `:` with no port number after it;
    - a character no host holds (`%`, `'`, `,`, `=`, …), so `u%3A<secret>` counts;
    - an empty authority before a hard stop, or an empty one in any scheme but `file` (`ftp:///\n<secret>@h`);
    - a `://` with no scheme before it;
    - an authority that runs straight into the next `://` (`u://<secret>@h`, whose `:` is the password's).
  - **Without a sign, the window now crosses `/` and every hard stop except a line break.** That closes `http://<secret>/"@h/`.
  - **These shapes are now hidden:** `udp://u:@<s>"://@h/x`, `ssh://u:://@<s>"@h/`, `ssh://u:\n<s>@h`, `wss://u%3A<s>\n@h` and `http://u%3A<s>/"@h/`.
  - **A host holding `%` outside an IPv6 literal is hidden whole** (`wss://u%3A<s>%40h`), as is a malformed `[…]` host.
- **Whitespace or a line break inside a magnet** (class 2).
  - **A magnet whose display name or keywords (`dn`, `kt`) the hard stop falls in runs to the end of the text.** A name may hold raw spaces and line breaks, and there's no telling where it ends.
  - **After any other stop, a following word that carries a magnet parameter carries the magnet on** (`&tr=`, `&dn=`, or `%26dn%3D` encoded; BEP 9 keys and the common extras).
- **A magnet inside a `://` URL's path, query or fragment** (class 3, B-log1).
  - **The URL's span now ends where the magnet's does,** never at the outer URL's soft end. `GET http://lodestar:8080/api/add?uri=magnet:?xt=…&dn=Some.Movie,2020.1080p` shows as `GET http://lodestar:8080/api/add`.
  - **`Next` (the forward-search cache) is now right for a query in any order.** A search from before its last start searches again. Extending the span asks again from the magnet, which is what broke #432's try (a naive cut).
  - **In `scrub` (typed URLs), a magnet in a query takes the rest of the query** (`?cat=1&uri=[redacted]`), since its own pairs follow it. A magnet in the path goes with the run it's glued to (`/x/magnet:[redacted]`).
  - **A magnet glued to an absolute URL's authority is cut there** (`http://[::1]magnet%3A…`). With an `@` after it, the whole URL goes.
- **A percent-encoded magnet** (class 4). `magnet%3A` and `magnet%253A` are magnets wherever `magnet:` is: in free text, in a query value, and at the start of a URL. All show as `magnet:[redacted]`.
- **An escaped `\"` in a `Debug`-form magnet** (class 5). A `\` and the character after it never end a magnet. A `Debug` value for `path` that holds a magnet is hidden whole, like one that holds an `@`.
- **A malformed IPv6 literal** (class 6). Extra `]` after a literal's own (`[::1]]`) don't end the URL, in a host or in a URL nested in a query.
- **URL forms without `://`, and glued credentials** (class 7).
  - **A pass before the URL rules writes each such form with a plain `://`:**
    - a `:` and two `/` in any mix of raw, percent-encoded once or twice, or JSON-escaped (`http:%2F%2F`, `http%3A%2F%2F`, `http%3A//`, `http:\/\/`, `http:%2F/`);
    - a known scheme, or another one that takes credentials (`ssh`, `git`, `smb`…), with a single `/` (`http:/u:p@h`), but never `file:/`;
    - a `//` at a word's start with an `@` after it, on the same line or after a `:` in its authority (`//u:p@h`).
  - **`holds_url` uses the same pass,** so `?u=http:%2F%2Fu:<s>@h` loses its value.
  - **Credentials glued on with `:` or `%3D`:**
    - a typed URL's scheme, and a query pair with no raw `=`, go through the `name=value` rules (`?passkey%3D<k>`, `?token:<t>`, `token:<t>://h`);
    - a scheme run after `:` or `%3D` stays with the URL before it, as one after `=` did;
    - a `Bearer` token takes `%` and `:`, and the scan reads it again from its start, so a credential's name inside it (`Bearer %3Apassword: "…"`) still has its value hidden. Overlapping cuts merge.
- **A Debug field's name inside a raw password** (added to the scope by the runner, from rc.4's delta report). `scrub_debug_forms` read `path: "`, `fragment: Some("` and the others inside a URL's password as a new field, and the value it hid held the userinfo's `@`. What came before it then showed as a host. A hidden value that held an `@` now keeps one after it (`[redacted]@`), so the URL rules still find the userinfo. The fuzz's case 25 plants a field's name in a password four ways; this PR shows none, and `main` shows 17,972 per 300,000.

## Review round (self-review and `/security-review`)
Both found shapes where this PR showed a secret `main` hid. Each is fixed, and each input is a test.
- **The separator pass split a credential's value into a URL** (security review). Examples: `secret=https%3A%2F%2Fh/<s>`, `{"token":"https:\/\/h\/<s>"}`, `token%3A%2F%2F<s>`, `passkey=Zq%3A%2F%2F<s>`.
  - The pass now skips any separator that touches a value `redact_secrets` would hide. `secret_cuts` is that function's cut-finding, split out so both use it.
  - A `//` is rewritten only when no such value starts before its `@`.
  - A mixed-encoding separator needs a scheme character before it.
  - `holds_url` still finds encoded URLs as `main` did.
- **A `Bearer` token, or any value, swallowed the next credential's name** (`Bearer pass:token: <s>`, `pass:token: <s>`, `user:username:"<s>`). Every value is now read again from its start. After `%3D` or `%3A` that's from the `3`, as `main` read it. `stop` remembers its last answer, so this stays linear.
- **A span that ends on a credential's name** (`magnet:?xt=a &tr=Authorization: <s>`, `//@Bearer <s>`, `https://h/x/token: <s>`). The span is still scrubbed whole, since the name may be glued to a passkey. The text after it is then read with that name before it (`carry` and `plain`), so its value goes.
- **A Debug value that ends on a credential's name** (`password: Some("user:username:"<s>`, which `main` shows too). The name's value after the closing quote goes as well.
- **A credential's value in a path or an authority that holds `/`** (`/token:https:\/\/h\/<s>`, `@token:https:\/\/…`). Both show on `main` too.
  - In a path, the value and the rest of the path go (`scrub_path`, used by `finish` and by a Debug `path`).
  - A host holding a character no host can, or a port that's no number, hides the rest of the URL.
- **A magnet glued to an authority hides the whole URL** (`https://<s>,magnet:?xt=a` shows as `https://[redacted]`). Cutting there dropped the `:` that made `main` hide it.

## Over-redaction accepted ("never too little")
- **A URL followed by any `@` on the same line, within 512 bytes and before the next `://`, loses its host and path,** whether it has userinfo or not.
  - Example: `see https://a.example/p and me@x` shows as `see https://[redacted]`. #432 kept that path; the `/` no longer ends the window.
  - A line break still ends the window when nothing marks userinfo: `see https://h.example:8080/x\nme@x` is unchanged.
- **With a sign of userinfo, the window also crosses `://` and line breaks.**
  - `http://user:hunter2://x@h/ ok` shows as `http://[redacted] ok`.
  - `://@http://u:p@h/x` shows as `://[redacted]`. That's the shrunk input from #432.
  - `see http://h/a then://@https://u:p@h2/b` shows as `see http://h/a then://[redacted]`.
- **A magnet whose `dn` or `kt` holds the stop takes the rest of the text.**
  - Example: `add magnet:?xt=…&dn=Some Name to queue` shows as `add magnet:[redacted]`.
  - A magnet whose last parameter is a tracker still ends at the next hard stop (`… to queue` stays).
- **A magnet in a typed URL's query loses every pair after it.**
- **Encoded URLs in free text lose their host** (`GET /login?next=https%3A%2F%2Fh%2Fx` shows as `…next=https://[redacted]`). After the pass, the host holds a `%`.
- **The forms without `://` show with one** (`http:/u:p@h` shows as `http://[redacted]@h`, `//u:p@h` as `://[redacted]@h`). A redacted line needn't keep the spelling.

## Known gaps left
Each one leaks on `main` too.
- **A username-only userinfo (no `:` or other sign) holding a raw line break** (`ssh://<secret>\n@h`). It can't be told from a URL at a line's end with an address on the next line, and hiding every such URL loses too much. About 200 of the fuzz's 300,000 inputs per seed hit this.
- **A single-slash form behind an unknown scheme** (`x:/u:p@h`). Taking any word before `:/` would turn `C:/Users` into a URL.
- **A `Debug` value holding a raw, unescaped `"`.** `Debug` never writes one; the fuzz's cases 7 and 8 plant them.
- **Forms escaped or encoded more than the pass knows** (class 7): a double JSON escape (`http:\\\/\\\/u:<s>@h`), an encoded backslash (`http:%5C/%5C/u:<s>@h`), and a magnet encoded three times (`magnet%25253A…`, class 4). Each extra form is one more spelling in `separator_len` or `magnets` if logs ever show it.
- **`scrub` (typed URLs) given free text with a credential's value before `://`** (`secret=https%3A%2F%2Fh/<s>` as an absolute URL). Typed URLs come from the code, never from such text; `scrub_text` hides it.

## Cost
- **`Ats` takes 4 bytes per byte of text** (4 MiB for a 1 MiB line), for the time a line is scrubbed. It's built only when the text holds an `@`.

## Tests
- **Deterministic tests, one per class, with every input leaking on `main`:**
  - `a_raw_password_with_url_syntax_goes_whole`
  - `a_magnet_runs_on_past_a_space_in_its_name`
  - `a_magnet_inside_a_url_is_never_shown`
  - `a_percent_encoded_magnet_is_a_magnet`
  - `a_malformed_ipv6_host_never_ends_the_url_early`
  - `url_forms_without_a_raw_scheme_separator_lose_their_secrets`
  - `a_credential_glued_on_with_a_colon_or_an_encoded_equals_goes`
  - `a_debug_field_name_in_a_password_keeps_its_at` (the runner's case)
  - `a_credential_s_value_is_never_split_into_a_url` and `a_name_glued_on_never_escapes_with_its_value` (the review round)
- **Existing tests whose expected output changed:** four exact outputs, named in the over-redaction section above, and the magnet test's `dn`-last case. Every other assertion is unchanged.
- **Properties:**
  - `planted_secrets_never_get_through` adds line breaks and `://` on both sides of a hard stop, `u%3A`, unmarked userinfo holding `/` and hard stops, every form without `://`, and the `//` and single-slash forms.
  - A new `magnets_keep_no_secret` covers whitespace in `dn`, `kt` and `tr`, a magnet in a URL after a soft end, encoded and `Debug`-escaped magnets, and a malformed IPv6 literal before a passkey.
  - A new `a_credential_s_value_keeps_its_secret` puts a credential's value after `Bearer`, a URL or a magnet. The value may start with an encoded or escaped `//`, a scheme and `:/`, a `Bearer` chain or a field's name.
  - 15 extra runs of 1,024 cases each after the review round, all passing. The seeds they saved during the round are kept in `proptest-regressions/url.txt`.
- **Speed:**
  - `scrub_text_is_linear` gains 18 units and the 1 MiB test 3: `://u:x`, a magnet with a raw space, ` //a@`, `[::1]]`, `\"`, `token: "`, `Bearer a:`, `token=token=`, `x/Authorization: ` and others.
  - In a release build, every unit takes about 4× as long at 4 MiB as at 1 MiB. The slowest, `:/`, takes about 0.13 s per MiB.
- **No panics:** none in the fuzz on either side. The new magnet cut panicked on an empty URL during development; its seed is kept in `proptest-regressions/url.txt`.

## Differential fuzz against `main`
The harness is #432's labelled one (`reviews/432/round4-harness-labelled.rs` on `runner-state`), with nine cases added for this slice's classes (16 to 24). It compares this PR's `scrub_text`, `scrub` (absolute) and `scrub` (request) with `origin/main` at e62a8b3. A regression is a planted secret this PR shows and `main` hides.

| Run | Seed | Inputs | Regressions | Shown by PR | Shown by `main` | Panics (PR / `main`) |
|---|---|---|---|---|---|---|
| Original cases 0–15 | 2 | 1,000,000 | 0 | 6,536 | 50,650 | 0 / 0 |
| | 5 | 1,000,000 | 0 | 6,709 | 50,464 | 0 / 0 |
| | 9 | 1,000,000 | 0 | 6,522 | 50,085 | 0 / 0 |
| | 13 | 1,000,000 | 0 | 6,666 | 50,984 | 0 / 0 |
| New cases 16–24 | NEW_ROWS |
| `review-diff-harness.rs` | 7 | 1,000,000 | 0 | 6,603 | 50,197 | 0 / 0 |

What this PR still shows is in the three gaps above: cases 7 and 8 (raw `"` in a `Debug` value), case 3 (an unmarked username with a raw line break) and case 23 (`x:/` and other unknown schemes).

## Released builds
- **Every class leaks on v0.5.1-rc.1 to rc.4,** since they share `main`'s scrubber.
- **The change is this one file, with no crawler change.** The runner decides whether it's backported to `release/v0.5.1`.
