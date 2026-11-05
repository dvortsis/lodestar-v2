# X-log-url-redact-5 decisions (PR to be numbered)

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
  - the colons `︓` `﹕` `∶` `꞉` `︰` `ː` `։` `׃`;
  - the equals sign `﹦`;
  - the other Unicode spaces after `Bearer` (U+202F, U+2007, U+2002, U+3000, U+1680, U+0085…);
  - the zero-width spaces (U+200B, U+2060, U+FEFF), including one before a `:`.

## The rule
- **One table.** `look_alike(c, all)` maps:
  - every colon look-alike to `:`;
  - both equals look-alikes to `=`;
  - every non-ASCII Unicode space (`char::is_whitespace`) and the three zero-width spaces to a space.

  With `all` false, it maps only #488's three (`：`, `＝`, U+00A0).
- **Two new readings,** each through an `only_more` of its own, after #488's:
  - the wide output, with every look-alike in ASCII, run only when it holds one that #488's reading doesn't map;
  - the original text, with every look-alike in ASCII, run only when it holds any. This covers a look-alike space after a separator: the first pass's output has lost the value by then.
- **#488's reading stays exactly as it was.** Widening its mapping instead was the first try, and the fuzz showed 326 and 304 secrets per 1.2M that `main` hid.
  - **Why:** a mapped space can end a value that the unmapped reading hid whole. In `token＝r\u{200b}<s>`, the zero-width space read as a space ends the value at `r`.
  - **The lesson:** "monotone by construction" covers only an `only_more` layer that's added. It doesn't cover a change to the argument of a layer that's already there.
- **Monotone by construction:** each new layer is `only_more(previous, …)`, which only replaces text of `previous` with `[redacted]`. So nothing `main` hides can show.
- **Two modifier letters,** `ː` (U+02D0) and `꞉` (U+A789), are word characters. A word holding one isn't in the mapped text, so it goes whole (`passwordːS` shows as `[redacted]`). That hides only more. Every other look-alike is no word's character.

## The fast path stays exact
- **What `plain_line` refuses:** any line holding a character `look_alike(c, true)` maps. It uses the same function, so the fast path refuses exactly what the readings map. The 0xC2 and 0xEF byte arms #488 added are gone.
- **The proof:** on a line `plain_line` accepts, `ascii_separators` changes nothing, so every reading is `redact_secrets` of text with no credential separator, and finds no cut. So the trigger table in `docs/decisions/2026-11-05-X-log-url-redact-3.md` holds. This file adds this row to it:

| Rule | What it needs to fire | Excluded by |
|---|---|---|
| The extra credential readings, with every look-alike | a credential name and a look-alike separator or space | every character `look_alike(c, true)` maps, refused by `plain_line` |

- **The test:** `the_fast_path_matches_both_stages` plants eight more look-alikes among its triggers. It still checks that `©`, `（`, `ｘ` and `；` are accepted.

## Tests
- **`a_look_alike_space_or_separator_keeps_its_value_hidden`:** 16 inputs, each of which showed the secret on `main`, and each refused by `plain_line`. It also checks that `共 12\u{3000}个`, `a\u{a0}b ok` and `時刻：12 ok` are kept.
- **Every other test passes unchanged.**

## Differential fuzz against `main` (ad9e59a)
- **The harness:** #483's harness, kept out of the commit, with its `main` side moved to ad9e59a.
  - **New slots:** the 12 shapes above.
  - **New tokens:** 9 more look-alikes.
- **The run:** 2 seeds × 1,200,000 token soups, about 2,400,000 secrets per seed.

| Seed | Entry point | PR shows, `main` hides | `main` shows, PR hides | Shown by PR / `main` | Panics |
|---|---|---|---|---|---|
| 1 | `scrub_text` | **0** | 337,045 | 715,117 / 1,052,162 | 0 |
| 1 | `scrub` absolute | 0 | 0 | 623,129 / 623,129 | 0 |
| 1 | `scrub` request | 0 | 0 | 487,449 / 487,449 | 0 |
| 2 | `scrub_text` | **0** | 337,560 | 714,987 / 1,052,547 | 0 |
| 2 | `scrub` absolute | 0 | 0 | 621,108 / 621,108 | 0 |
| 2 | `scrub` request | 0 | 0 | 486,907 / 486,907 | 0 |

## Cost
- **ASCII lines cost the same as on `main`:** the new readings run only on non-ASCII text holding a look-alike.
- **A line with a look-alike** gets one or two more credential readings. `下载完成：12 个文件` takes 4.6 µs, against `main`'s 3.4.
- **Linear sweep:** 3,100 units, from 32 KiB to 256 KiB.
  - The worst time ratio, normalised to 1 for linear, is 4.33 (`,`), a fast-path unit at 12 ms. The ratios are dominated by noise.
  - The slowest unit off the fast path, `https://\u{3000}#\u{200b}tr=:8080`, takes 0.39, 1.49 and 6.2 s at 1, 4 and 16 MiB: about 4 times as long at each 4 times in size.
  - `scrub_text_is_linear` passes.

## Gaps left
- **Look-alikes outside the table:** for example U+180E (Mongolian vowel separator, no longer a space in Unicode), U+200C and U+200D (zero-width non-joiner and joiner; emoji sequences use the joiner), and fullwidth letters in a credential's name (`ｐａｓｓｗｏｒｄ`). Each would be one more arm in `look_alike`, if logs ever show it.
- **The near variants #488's security review listed are unchanged:** in each, the name before the value isn't a credential's.
