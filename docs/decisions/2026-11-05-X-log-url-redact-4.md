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
- **The harness:** #483's harness, kept out of the commit. It has five more credential slots for this class:
  - `https://h/pass?x": "S"`
  - `https://h/key?a': S`
  - `https://h/apikey?a=1'=S`
  - `/token?q"=S`
  - `http://h/x/auth?a=b': "S"`
- **The run:** 2 seeds × 1,200,000 token soups, about 2,400,000 secrets per seed.

| Seed | Entry point | PR shows, `main` hides | `main` shows, PR hides | Shown by PR / `main` | Panics |
|---|---|---|---|---|---|
| 1 | `scrub_text` | **0** | 163,779 | 739,000 / 902,779 | 0 |
| 1 | `scrub` absolute | 0 | 0 | 396,523 / 396,523 | 0 |
| 1 | `scrub` request | 0 | 0 | 274,981 / 274,981 | 0 |
| 2 | `scrub_text` | **0** | 163,357 | 739,581 / 902,938 | 0 |
| 2 | `scrub` absolute | 0 | 0 | 394,883 / 394,883 | 0 |
| 2 | `scrub` request | 0 | 0 | 274,021 / 274,021 | 0 |

- **What the counts show:** most of the extra hidden are in the new slots, which leak on `main`. Typed `scrub` is unchanged.

## Cost
- **The fast path doesn't change.**
- **The extra cost on other lines:** one more credential pass over the wide output, plus one more `only_more`. That's about 0.6 to 1.7 µs on a short line in a release build, measured with the fuzz running alongside.
- **Linear sweep:** 3,100 units, from 32 KiB to 256 KiB.
  - The worst time ratio, normalised to 1 for linear, is 2.60 (`ssh://" @`).
  - The three worst units take about 4 times as long at each 4 times in size, from 1 to 16 MiB:

| Unit | 1 MiB | 4 MiB | 16 MiB |
|---|---|---|---|
| `ssh://" @` | 0.105 s | 0.516 s | 1.700 s |
| `"), https://` | 0.251 s | 1.107 s | 4.642 s |
| `magnet%3A%3F` | 0.153 s | 0.927 s | 2.641 s |
