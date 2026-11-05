# X-log-url-redact-5 decisions (#492)

A security fix in `crates/log/src/url.rs` (architecture §12.1, B12, B-log1).
- **Who decided:** the runner made this slice from its review of #488 (X-log-url-redact-4), and set its rules: monotone through `only_more`, fuzz against `main`, a linear sweep and a security review.
- **The base:** it builds on `main` at ad9e59a, which holds #483 and #488. Every comparison is with that `main`.
- **What it supersedes:** the "Other look-alikes still hide nothing" bullet in `docs/decisions/2026-11-05-X-log-url-redact-4.md`'s gap list. Those look-alikes are covered here.

## What leaked on `main`
None of these hid the value in either stage:
- **A non-ASCII space right after a separator,** before a quoted or space-separated value. The first pass took that space as the whole value, so its output no longer shows the real one, and #488's second reading found nothing:
  - `{"password":\u{a0}"<s>"}`
  - `token=\u{a0}'<s>'`
  - `password:\u{a0} <s>`
  - `x-api-key :\u{3000} <s>`
- **`Authorization: Bearer\u{a0} <s>`,** a no-break space and then an ASCII space.
- **An ideographic space next to `：`,** before it or after it: `password：\u{3000}"<s>"`, `password\u{3000}：<s>`. `skip_space` skips ASCII spaces only.
- **The other look-alikes:**
  - the colons `︓` `﹕` `∶` `꞉` `︰` `։` `׃`;
  - the equals signs `﹦` and `꞊`;
  - the other Unicode spaces after `Bearer` (U+202F, U+2007, U+2002, U+3000, U+1680, U+0085…);
  - the zero-width spaces (U+200B, U+2060, U+FEFF), including one before a `:`.

## The rule
- **One table.** `look_alike(c, all)` maps:
  - every colon look-alike to `:`;
  - both equals look-alikes to `=`;
  - every non-ASCII Unicode space (`char::is_whitespace`), the vertical tab (U+000B, whitespace to Rust but not ASCII whitespace to the credential rules) and the three zero-width spaces to a space.

  With `all` false, it maps only #488's three (`：`, `＝`, U+00A0).
- **Two new readings,** each through an `only_more` of its own, after #488's:
  - the wide output, with every look-alike in ASCII, run only when it holds one that #488's reading doesn't map;
  - the original text, with every look-alike in ASCII, run only when it holds any. This covers a look-alike space after a separator: the first pass's output has lost the value by then.
- **#488's reading stays exactly as it was.** Widening its mapping instead was the first try, and the fuzz showed 326 and 304 secrets per 1.2M that `main` hid.
  - **Why:** a mapped space can end a value that the unmapped reading hid whole. In `token＝r\u{200b}<s>`, the zero-width space read as a space ends the value at `r`.
  - **The lesson:** "monotone by construction" covers only an `only_more` layer that's added. It doesn't cover a change to the argument of a layer that's already there.
- **Monotone by construction:** each new layer is `only_more(previous, …)`, which only replaces text of `previous` with `[redacted]`. So nothing `main` hides can show.
- **None of the look-alikes is a word's character,** so mapping them keeps the words.
- **The length mark `ː` (U+02D0) is left out,** though the runner listed it.
  - **Why:** it's a letter, so mapping it hid every word that held one, on any line, with no credential near: `IPA /kæːt/` showed as `IPA /[redacted]/`.
  - **The trade:** it's seldom a colon in logs, and keeping it out keeps it on the fast path. `passwordːS` still shows (see the gaps).
- **`꞊` (U+A78A),** the equals counterpart of `꞉`, is in, from the self-review.

## A `//` URL's password after a space, in a credential's value (runner's addition)
- **How it was found:** `planted_secrets_never_get_through` failed on #489's Windows CI.
  - The template: `{before} //u:{syntax}{secret}@host.example/x`.
  - The seed: `before = "PW:"`, `syntax = " "`.
  - The runner moved it into this PR.
- **What leaked:** `PW: //u: <s>@host.example/x` showed `<s>@host.example/x`.
- **It's older than #488:** the same output on #488's merge (4e425d4e) and on its parent (6648e69f), on #483's (09c51792), on #460's (40c5c84), and on the scrubber before #460 (0531d024). The proptest planted it only rarely.
- **The cause:**
  - `PW` is a credential's name (`pw` is a credential part), and its value `//u:` ends at the space.
  - #483's guard keeps the separator pass from rewriting a `//` inside a credential's value. That guard is what hides `secret=https%3A%2F%2Fh/<s>` whole.
  - The first pass has already hidden the `//` anyway, so neither stage reads `u: <s>@host` as userinfo.
- **The rule:** the wide rules also read the original text, after the `Debug` rules, with every URL form rewritten, credential values included (`wide::scrub_text_and_loose`).
  - That reading's output goes through one more `only_more`, as its own layer.
  - It runs only when the text holds such a form.
  - The guarded wide output is unchanged, so what the guard hides stays hidden.
- **Tests:**
  - **The seed** is in `proptest-regressions/url.txt`. It fails without the fix.
  - **`a_network_path_in_a_credential_s_value_keeps_its_password`** has 15 inputs: `PW:`, `PW: `, `token=`, `x-api-key: ` and `password:"`, each with a space, two spaces or a tab.
    - The nine unquoted values after `PW: `, `token=` and `x-api-key: ` showed the secret on `main`.
    - Glued `PW:` (which makes a raw `://`) and the quoted value are guards: `main` hides them too.
- **The host still shows,** as on `main`: `PW: [redacted] [redacted]@host.example/x`.
- **Over-redaction accepted (runner's review):** where a credential's value meets `//`, the reading takes the userinfo to the last `@` on the line, within its window. So text between the two can go too. It only ever hides more.

## The fast path stays exact
- **What `plain_line` refuses:** any line holding a character that `look_alike(c, true)` maps to `:` or `=`. It uses the same table as the readings. The 0xC2 and 0xEF byte arms #488 added are gone.
- **What it accepts:** look-alike spaces, the vertical tab and U+00A0 (runner's review of 8589d4d7).
  - **Why:** refusing every look-alike sent such lines down the slow path. 16 MiB of `x\u{200b}` took 5.5 s against `main`'s 0.16 s, and CJK text with U+3000 took 4.6 s against 0.25 s.
  - **Now:** they take 0.18 s and 0.32 s.
  - **The cost to the fast path:** U+00A0 was refused since #488, as margin. It isn't any more.
- **The proof.** Take a line `plain_line` accepts.
  - It holds no `/` or `%`, so `normalize_separators` returns it unchanged and the `//` reading doesn't run.
  - It holds no credential separator, ASCII or look-alike, and no `bearer`.
  - `ascii_separators` maps only spaces on it, and a space makes no cut without one of those. So every look-alike reading finds no cut, and both stages are `hide_runs` of the text, as the trigger table in `docs/decisions/2026-11-05-X-log-url-redact-3.md` says.
  - **The row this file adds to that table:**

| Rule | What it needs to fire | Excluded by |
|---|---|---|
| The extra credential readings, with every look-alike | a credential name and a look-alike `:` or `=`, or a separator or `bearer` that's already refused, with a look-alike space beside it | the colon and equals look-alikes, refused by `plain_line`; the rest refused already |

- **The test:** `the_fast_path_matches_both_stages`.
  - It plants the colon and equals look-alikes among its triggers.
  - It puts U+00A0, U+3000, U+2007, U+200B, U+FEFF, the vertical tab and `下载完成` among the accepted soups' tokens, so those soups go through the whole slow path (`scrub_text_slow`) as well.
  - It still checks that `©`, `（`, `ｘ` and `；` are accepted, as are `a\u{a0}b`, `共 12\u{3000}个` and `x\u{200b}y\u{b}z`.
- **What the test found:** the original text's reading hid a word glued to a hash-like run (`<hash>日本`). That reading didn't hide runs as the stages do, so `日本` wasn't a word of its own there. It hides them now (`hide_runs`). The fast path is exact again, and that word no longer goes.

## Tests
- **`a_look_alike_space_or_separator_keeps_its_value_hidden`:** 33 inputs, each refused by `plain_line`.
  - **31 showed the secret on `main`:**
    - the shapes above;
    - one per table arm;
    - the vertical tab after `Bearer` and before `:`;
    - three that only the wide output's reading hides: `https://h/pass?x":\u{3000}"<s>"`, `https://h/pass?x"\u{fe55}"<s>"` and, for its vertical-tab clause, `https://h/pass?x":\u{b}"<s>"`. The runner's reviews found the arm and that clause untested: each fails without them.
  - **Two are guards that `main` hides too:** `token＝r\u{200b}<s>` and `password＝a\u{3000}<s>`. Merging the new readings into #488's would show them, so they keep the layers apart.
  - **It also checks five lines are kept:** `共 12\u{3000}个`, `a\u{a0}b ok`, `時刻：12 ok`, `ratio 1∶2 ok` and `IPA /kæːt/ ok`.
- **`the_fast_path_matches_both_stages`** also accepts `a\u{200c}b` and `naːm`: a non-ASCII character that's no look-alike still takes the fast path.
- **Every other test passes unchanged.**

## Differential fuzz against `main` (ad9e59a)
- **The harness:** #483's harness, kept out of the commit, with its `main` side moved to ad9e59a.
  - **New slots (16):** the 7 shapes in "What leaked", plus `password﹕S`, `token﹦S`, `Bearer\u{202f}S`, `Bearer\u{200b}S` and `password\u{200b}:S`, plus four `//` forms: `PW: //u: S@host.example/x`, `token=//u:\tS@h`, `x-api-key: //u:  S@h/` and ` //u: S@host.example`.
  - **New token entries (11):** U+3000, U+202F, U+200B, U+FEFF, U+FE55, U+2236, U+FE66, U+A78A and U+000B, and `ː` twice (now a near miss).
- **The run:** 2 seeds × 1,200,000 token soups, about 2,400,000 secrets per seed.

| Seed | Entry point | PR shows, `main` hides | `main` shows, PR hides | Shown by PR / `main` | Panics |
|---|---|---|---|---|---|
| 1 | `scrub_text` | **0** | 379,510 | 633,662 / 1,013,172 | 0 |
| 1 | `scrub` absolute | 0 | 0 | 615,116 / 615,116 | 0 |
| 1 | `scrub` request | 0 | 0 | 415,433 / 415,433 | 0 |
| 2 | `scrub_text` | **0** | 381,023 | 633,739 / 1,014,762 | 0 |
| 2 | `scrub` absolute | 0 | 0 | 613,581 / 613,581 | 0 |
| 2 | `scrub` request | 0 | 0 | 414,634 / 414,634 | 0 |

- **At 8589d4d7,** before the vertical tab and the fast-path change, the same seeds hid 377,925 and 379,590 more, with 0 regressions.

- **Before the `//` rule,** with 12 new slots, the same seeds hid 330,761 and 330,895 more, with 0 regressions.

- **With `ː` in the table** and 12 new slots, the same seeds hid 337,045 and 337,560 more, with 0 regressions. The difference is the words holding `ː`, which now show as `main` shows them.

## Cost
- **ASCII lines cost the same as on `main`, within noise.** The look-alike readings run only on text holding a look-alike, non-ASCII or the vertical tab. The `//` reading runs only on text holding a URL form without `://`.
- **A line with only look-alike spaces** takes the fast path, as on `main` (0.32 s for 16 MiB of CJK text with U+3000).
- **A line with a look-alike `:` or `=`** gets one or two more credential readings. `下载完成：12 个文件` takes 4.6 µs, against `main`'s 3.4.
- **Linear sweep:** 3,100 units, from 32 KiB to 256 KiB.
  - The worst time ratio, normalised to 1 for linear, is 3.92 (`x://&?`, at 91 ms). The ratios are dominated by noise.
  - The worst units, the `//` form the new reading handles, and the runner's two look-alike-space cases take about 4 times as long at each 4 times in size:

| Unit | 1 MiB | 4 MiB | 16 MiB |
|---|---|---|---|
| `x://&?` | 0.111 s | 0.452 s | 1.872 s |
| `éNameapikey=ok＝;` | 0.174 s | 0.639 s | 2.920 s |
| ` //a@` | 0.197 s | 0.945 s | 5.240 s |
| `x\u{200b}` | 0.014 s | 0.057 s | 0.177 s |
| `共 12\u{3000}个` | 0.016 s | 0.067 s | 0.315 s |

  - `scrub_text_is_linear` passes.

## Gaps left
- **Look-alikes outside the table,** on `main` too. Each would be one more arm in `look_alike`, if logs ever show it:
  - the length mark `ː` (U+02D0), left out above;
  - the equals signs `⁼` (U+207C) and `⩴` (U+2A74);
  - U+180E (Mongolian vowel separator, no longer a space in Unicode);
  - U+200C and U+200D (zero-width non-joiner and joiner; emoji sequences use the joiner);
  - a zero-width space inside a credential's name (`pass\u{200b}word=<s>`);
  - fullwidth letters in a credential's name (`ｐａｓｓｗｏｒｄ`).
- **The `//` rule's near variants (runner's review),** on `main` too:
  - an encoded `//` in a credential's value: `PW: %2F%2Fu: <s>@h`, `token=%2F%2Fu: <s>@h`;
  - a `//` after `%3D`, which isn't a word's start to the separator pass: `apikey%3D//u: <s>@h`;
  - a look-alike space between the name and the `//`: `PW:\u{3000}//u: <s>@h`.
- **A secret that also shows elsewhere on the line,** on `main` too. That's `only_more`'s documented limit: a word the reading shows anywhere is kept everywhere.
  - Take `password\u{fe55}_<s> 0123456789abcdef0123456789abcdef_<s>`. `hide_runs` hides the hash-like run but keeps `_<s>` after it as its own word, so the bare `<s>` after the look-alike stays too.
- **The near variants #488's security review listed are unchanged:** in each, the name before the value isn't a credential's.
