# X-log-url-redact-4 decisions (PR to be numbered)

A security fix in `crates/log/src/url.rs` (architecture §12.1, B12, B-log1).
- **Where it came from:** #483's `/security-review` (X-log-url-redact-3) found the leaks and recorded them in `docs/decisions/2026-11-05-X-log-url-redact-3.md`.
- **Who decided:** the runner made this slice and chose the rules. The fix must be monotone: prefer a wide rule that only adds redactions. Prove it with the same differential fuzz, with 0 cases that this PR shows and `main` hides.
- **The base:** it builds on #483's head, and compares with `main` at 40c5c84. #483's output equals that `main`'s on every input.

## The leak
- **The cause:** once the first pass drops a URL's query, a credential's name can end up at the end of the URL's path, with a value after its quote.
  - Take `https://h/pass?x": "<s>"`. It reads `https://h/pass": "<s>"` after the first pass.
  - Stage 1 never reads that value as a credential's, and neither does the wide rules' first pass.
  - Only the second pass would, and it runs only when the line holds a form without `://` or an encoded magnet.
- **What showed:** `main` showed the secret in full in all three of these:
  - `https://h/pass?x": "x%3A%2F%2Fh/<s>"`
  - `https://h/key?a': x%3A%2F%2Fh/<s>`
  - `https://h/apikey?a=1'=x%3A%2F%2Fh/<s>`

## The rule
- **What runs:** `scrub_text` reads the wide output again with the credential rules (`wide::redact_secrets`). It applies the result through a second `only_more`: `only_more(only_more(floor, wide), redact_secrets(wide))`.
- **Why it's monotone by construction:** `only_more` replaces only text of its first argument, with `[redacted]`. So the outer call hides all that the inner one hides, which is #483's output, and possibly more. Nothing #483 or `main` hides can show.
- **Why it's a separate `only_more`:** `redact_secrets(wide)` isn't used as the wide output itself. A cut after `%3D` can leave a new word fragment, `3D` in `key%3D[redacted]`. The inner `only_more` would then keep that word wherever stage 1 shows it. The outer call only ever adds `[redacted]`.
- **The fast path is unchanged:** a line it accepts holds no credential separator, so `redact_secrets` would find nothing there either.

## Look-alike separators (runner's review of #483)
- **The leak:** a fullwidth `：` (U+FF1A) or `＝` (U+FF1D) after a credential's name, or a no-break space (U+00A0) after `Bearer`, hid nothing in either stage, on `main` too.
  - Examples: `password：<s>`, `token＝<s>` and `Bearer\u{a0}<s>`.
  - Third-party log text may hold them.
- **The rule:** the credential rules' second reading (above) sees the three as `:`, `=` and a space (`ascii_separators`).
  - None of them is a word's character, so the words stay the same.
  - That reading feeds only the outer `only_more`, so it still hides only more.
  - The first stage and the wide rules are unchanged.
- **The fast path stays exact:** `plain_line` now refuses a line that holds any of the three. Its trigger list in `docs/decisions/2026-11-05-X-log-url-redact-3.md` gains one row:

| Rule | What it needs to fire | Excluded by |
|---|---|---|
| The second credential reading, with look-alikes | a credential name and `：` or `＝`, or `bearer` and U+00A0 | the three characters, refused by `plain_line` |

  - On a line `plain_line` accepts, `ascii_separators` changes nothing. So the reading is `redact_secrets` of `hide_runs(text)`, which finds no cut: the fast path's proof still holds.
  - `the_fast_path_matches_both_stages` now plants the three characters among its triggers, too.
- **Test:** `a_look_alike_separator_still_marks_a_credential` has six inputs, each of which showed the secret on `main`. It also checks that `時刻：12 ok` is kept.

## `AnyOf::new` asserts its size (runner's review of #483)
- **What changed:** it asserts it gets 1 to 3 needles, in release builds too.
- **Why:** an empty slice underflowed `needles.len() - 1`. All three callers pass 1 to 3, so this is a guard, not a fix.

## Over-redaction accepted
- **The rule hides more than the three shapes.** A value after any credential's name that the wide rules' output holds now goes, even when the original line didn't read as one. That costs only lines whose URL path ends in a credential's name (`/token`, `/key`, `/session`, `/auth`…) followed by a quote, a `:` or an `=`.
- **Ordinary lines are unchanged.** These give the same output as on `main`:
  - `GET https://api.example/v1/auth?x=1: 401 unauthorized`
  - `POST https://h/session?id=1 status=200 took=12ms`
  - `fetch https://h/oauth/token?grant=x failed: connection reset`
  - `tracker http://t.example/announce: timeout after 30 s`

## Tests
- **`a_name_left_at_a_url_s_end_keeps_its_value_hidden` is no longer ignored,** and it passes. #483 added it, ignored, holding the three inputs above.
- **Every other test passes unchanged.**

## Differential fuzz against `main` (40c5c84)
- **The harness:** #483's harness, kept out of the commit.
  - **It has nine more credential slots:**
    - `https://h/pass?x": "S"`, `https://h/key?a': S`, `https://h/apikey?a=1'=S` and `http://h/x/auth?a=b': "S"`;
    - `/token?q"=S`, which shows on both sides: a path with no scheme keeps its query, so the name read is `q`. Listed under the gaps below;
    - `password：S`, `token＝S`, `Bearer\u{a0}S` and `"apikey"："S"`.
  - **It has three more tokens:** `：`, `＝` and U+00A0.
- **The run:** 2 seeds × 1,200,000 token soups, about 2,400,000 secrets per seed.

| Seed | Entry point | PR shows, `main` hides | `main` shows, PR hides | Shown by PR / `main` | Panics |
|---|---|---|---|---|---|
| 1 | `scrub_text` | **0** | 275,669 | 745,038 / 1,020,707 | 0 |
| 1 | `scrub` absolute | 0 | 0 | 475,107 / 475,107 | 0 |
| 1 | `scrub` request | 0 | 0 | 347,198 / 347,198 | 0 |
| 2 | `scrub_text` | **0** | 275,193 | 746,453 / 1,021,646 | 0 |
| 2 | `scrub` absolute | 0 | 0 | 474,868 / 474,868 | 0 |
| 2 | `scrub` request | 0 | 0 | 346,461 / 346,461 | 0 |

- **Before the look-alikes,** the same seeds gave 0 regressions and 163,779 and 163,357 more hidden, with five new slots.

- **What the counts show:** most of the extra hidden are in the new slots, which leak on `main`. Typed `scrub` is unchanged.

## Cost
- **The fast path doesn't change.**
- **The extra cost on other lines:** one more credential pass over the wide output, plus one more `only_more`. That's about 0.6 to 1.7 µs on a short line in a release build, measured with the fuzz running alongside.
- **Linear sweep:** 3,100 units, from 32 KiB to 256 KiB, on the final code.
  - The worst time ratio, normalised to 1 for linear, is 4.27 (`}`). It's a fast-path unit at 10 ms, where small timings are noisy.
  - The next are 3.38 (`日本u:`) and 3.24 (`x"%40[`).
  - The worst units take 3.4 to 5 times as long at each 4 times in size, from 1 to 16 MiB:

| Unit | 1 MiB | 4 MiB | 16 MiB |
|---|---|---|---|
| `}` | 0.018 s | 0.072 s | 0.281 s |
| `日本u:` | 0.151 s | 0.757 s | 2.595 s |
| `x"%40[` | 0.127 s | 0.550 s | 2.556 s |

- **Before the look-alikes,** the worst ratio was 2.60 (`ssh://" @`: 0.105, 0.516 and 1.700 s at 1, 4 and 16 MiB).

## Security review
- **What it checked:** the second `only_more` rule, read for monotonicity, panics and near variants. The review ran before the look-alike commit, which was then fuzzed above.
- **What it found:** nothing above its bar.
  - **Monotone by construction:** the outer call's words are a subset of the inner call's, plus `redacted`.
  - **Its own fuzz:** 3 seeds × 300,000, with 0 new words, 0 secrets shown and 0 panics.
- **Near variants still shown, on `main` too.** In each, the name left before the value isn't a credential's:
  - `/token?q"=<s>`: a path with no scheme keeps its query, so the name is `q`;
  - `https://h/pass/?x": "<s>"`: no name is left before the quote;
  - `https://h/pass.json?x": "<s>"`: the name is `json`;
  - `magnet:?…&tr=https://h/pass?x": "<s>"`: the magnet's span takes the name;
  - `x=https%3A%2F%2Fh%2Fpass%3Fx": "<s>"`: encoded, so no URL rule reads it.
- **Cosmetic:** `https://h/pass?x"=[<s>]` shows as `https://h/pass"=[redacted]]`, with one extra `]`.
