# X-log-url-redact-3 decisions

The follow-up slice named in `docs/decisions/2026-11-05-X-log-url-redact-2.md` ("Follow-up: X-log-url-redact-3"). It changes `crates/log/src/url.rs` only (architecture §12.1, B12, B-log1). Each item is its own commit, so the runner can drop any one. Every proof compares this branch with current `main` at 40c5c84 (#460's merged code), not with the old floor. The rule is still #460's: never too little, and nothing `main` hides may show.

## Outcome
- **Kept:** the fast path, a simpler separator pass, and `AnyOf` without a `Vec`.
- **Dropped: both narrowings.** Each one showed planted secrets that `main` hides (see below). The over-redaction #460 accepted for them stays.

## 1. Fast path (kept)
- **The problem:** a plain short line cost 4 to 6 times `main`'s old time, because both stages ran on every line.
- **The rule:** `scrub_text` returns stage 1's output alone (`scrub_text_floor`) for any line the fast path accepts (`plain_line`).
  - **Accepted:** a line with none of `:` `/` `@` `%` `=` `"`, and no `bearer` (any case), `lsk_` or `lsf_`.
  - **Safe by construction:** on the fast path the result is exactly `main`'s stage 1, so nothing stage 1 hides can show.
  - **The open question was only whether the fast path ever hides less than both stages would.** The proof below shows it doesn't.
- **Why no wide rule can fire on such a line.** Each trigger, in the order `wide::scrub_text` reaches it:

| Wide rule | What it needs to fire | Excluded by |
|---|---|---|
| `scrub_debug_forms` | `: Some(` or `: "` (it returns early without one) | `:` |
| `scrub_pass`: URL spans | `://` | `:` |
| `scrub_pass`: magnet spans | `magnet:` (first pass, raw only) | `:` |
| `secret_cuts`: Lodestar keys | `lsk_` or `lsf_` at a word's start | the keywords |
| `secret_cuts`: `Bearer` | `bearer` (any case) and whitespace | the keyword |
| `secret_cuts`: `name=value` | a credential name, then `=`, `:`, `%3D` or `%3A` | `=` `:` `%` |
| `plain`: encoded magnet in a cut | a cut (none, from the row above) | – |
| `normalize_separators` | `/`, `%2f` or `%252f` (it returns early without one) | `/` `%` |
| second pass: encoded magnets | `magnet%3a` or `magnet%253a` | `%` |
| `hide_runs` | 32+ hex digits, or 32 base32 characters | not excluded: both stages run the same code |

  - **What each stage then gives.** With no rule firing, `wide::scrub_text` is `hide_runs(text)`. `hide_runs` adds only `[hidden]`, which holds no `/` or `%`, so the separator pass still returns early.
  - Stage 1 reaches the same triggers (`scrub_debug_forms`, `://`, `magnet:`, `redact_secrets`) and gives `hide_runs(text)` too. The two `hide_runs` are the same code.
  - **Why `only_more` changes nothing.** Given two equal outputs, it hides nothing: every word of one is in the other.
  - **`@` and `"` aren't needed for the proof.** They're in the list as margin, as the brief gave it. Taking them out would widen the fast path, and that would need this proof again.
  - **`scrub` (typed URLs) has no fast path:** a typed URL always holds `/` or `:`.
- **The differential test:** `the_fast_path_matches_both_stages` checks that `scrub_text` equals `only_more(scrub_text_floor, wide::scrub_text)` on 2,000,000 accepted token soups.
  - Its tokens are near misses of every trigger (credential names with no separator, `Some(`, `magnet?xt`, `bear`, `lsk`, `LSK_`, hash-like runs, the markers), plus each trigger. A soup holding a trigger must be refused.
  - It takes about 20 s at the test profile's `opt-level = 1`.
- **Speed in a release build** (this container, 2,000,000 calls each):

| Line | `main` | This PR |
|---|---|---|
| `tick` | 0.74 µs | 0.18 µs |
| `listening on port 6881` | 1.67 µs | 0.39 µs |
| `peer connected, 12 pieces left` | 2.09 µs | 0.48–0.61 µs |
| `announce failed, retrying in 30 s` | 2.50 µs | 0.43–0.49 µs |
| `dht: 1024 nodes` (has `:`, so not on the fast path) | 1.73 µs | 1.65–1.85 µs |

- **The fuzz on this commit alone:** 0 differences from `main` in 2 × 1,200,000 soups, on all three entry points.

## 2. Narrowing A, the unmarked userinfo window (dropped)
- **What it tried:** with no sign of userinfo, the look-ahead would end at the later of the first `/` and the first whitespace, instead of at the line break. That's the code #460 withdrew, restored as it was.
- **What the fuzz found:** it shows secrets `main` hides, 2,045 and 2,021 per 1,200,000 soups (seeds 1 and 2). Shrunk examples:
  - `s://x/ AAQ2x@`: `main` gives `s://[redacted]/ [redacted]@`;
  - `F://AAQ0x/ @`, `x://0 yBBQ0z/@` and `s3:/r /AAQ2x@`.
- **Why it can't be saved:** these are the `http://<secret>/ x@h/` class that #460 closed (a raw password holding `/` and then a space). That input has the same shape as the line the narrowing exists to keep, `see https://a.example/p and me@x`, so no rule can tell the two apart.

## 3. Narrowing B, the `dn`/`kt` stop (dropped)
- **What it tried:** after a stop inside a display name or keywords, the magnet would carry on at any later word that carries a magnet parameter, and run to the text's end only if there's none. That's the code #460 withdrew.
- **What the fuzz found:** it shows secrets `main` hides, 502 and 501 per 1,200,000 soups. Shrunk examples:
  - `magnet:?kt &tr= yBBQ2z`: `main` hides `yBBQ2z`;
  - `magnet:&dn &xl="AAQ1x` and `magnet%3A%3Fdn"&xl=\nyBBQ1z`.
- **Why it can't be saved:** the text it shows has the same shape as the text it exists to show (` to queue` in `&dn=Some Name &tr=x to queue`). And a later word can open a parameter of its own (`&kt=?"…`), which ends the scan at a `?` the old rule didn't read as a name. So this narrowing can't be tightened into a safe one either.

## 4. Other cleanups
- **`AnyOf` holds a fixed array of up to three needles (kept).** It allocated a `Vec` on every `scrub_pass` and `normalize_separators` call. It searches the same as before: the fuzz shows 0 differences in 300,000 soups (seed 5).
- **The separator pass's credential-value guard is gone (kept).**
  - **What it was:** `normalize_separators(…, keep_values)` ran `secret_cuts` and skipped any separator inside a credential's value, so the second pass never split such a value into a URL (`secret=https%3A%2F%2Fh/<s>`, the security review in #460).
  - **Why it can go:** it guarded the one-stage design. Stage 1 never rewrites separators and hides that value whole, and `only_more` keeps what stage 1 hid.
  - **What the fuzz found:** 0 secrets shown that `main` hides in 2 × 1,200,000 soups, and 1,519 and 1,503 more hidden than `main`. The value's split parts now hide URL parts too.
  - **Tests:** `a_credential_s_value_is_never_split_into_a_url` still passes.
- **Tried and kept as it was: `in_url`.** It stops the separator pass rewriting inside a word that already holds a raw `://`. Without it, the fuzz showed 1 secret `main` hides in 300,000 soups (`http:/y?,yBBQ0zhttp:/@`), so it does more than guard the old design.
- **Left alone:** the other review-round guards in `secret_cuts` (values read again from their start) and in the Debug rules (`Hidden`). Many wide rules share them, and they also hide what the floor shows. Each would need its own fuzz round, and this slice has none to spare.

## Differential fuzz against `main` (40c5c84)
- **The harness is kept out of the commit.**
  - It compiles this branch's `url.rs` and `main`'s side by side.
  - It strings 2 to 31 tokens: schemes (`http:/`, `//` and `mailto:` among them), `user:pass@`, `@ : / ? & = # ;`, `%40 %3A %2F %3D %26 %3F`, quotes, `\"`, `Some("`, `password:`, `token=`, `apikey=`, `dn=`, `kt=`, `&tr=`, `xt=urn:btih:…`, `magnet:?`, `magnet%3A%3F`, `magnet%253A%253F`, `url::Url` Debug shapes, whitespace and `\r\n`, `[::1]` and `[::1]:6969`, `é 日本 🦀`, `Bearer `, `Authorization: ` and `lsk_`.
  - It plants 1 to 3 marked secrets `AAQ{k}x<sep>yBBQ{k}z` in 15 credential slots, among them `user:S@`, `://S@`, `://u:S@h`, `password: "S"`, `password: Some("S")`, `token=S`, `apikey=S`, `dn=S`, `kt=S`, `Bearer S`, `Authorization: S`, `?passkey=S`, `username: "S"` and `S@`. The 22 separators include `/`, a space, `"`, `@`, `:`, `\n`, `/ `, `\"`, `://` and `%2F`.
  - A secret counts as shown if either half shows.
  - It runs `scrub_text`, `scrub` (absolute) and `scrub` (request), and shrinks each regression by chunk deletion.
- **The head of this PR**, 2 seeds × 1,200,000 soups (about 2,400,000 secrets each):

| Seed | Entry point | PR shows, `main` hides | `main` shows, PR hides | Shown by PR / `main` | Panics |
|---|---|---|---|---|---|
| 1 | `scrub_text` | **0** | 1,519 | 724,666 / 726,185 | 0 |
| 1 | `scrub` absolute | 0 | 0 | 372,569 / 372,569 | 0 |
| 1 | `scrub` request | 0 | 0 | 264,523 / 264,523 | 0 |
| 2 | `scrub_text` | **0** | 1,503 | 724,424 / 725,927 | 0 |
| 2 | `scrub` absolute | 0 | 0 | 371,649 / 371,649 | 0 |
| 2 | `scrub` request | 0 | 0 | 264,023 / 264,023 | 0 |

- **Why the "shown" counts are high on both sides:** most slots put the secret where no rule could know it's one. #460's known gaps say the same.

## Linear time sweep
- **The run:** 3,100 random units of 1 to 6 tokens from the fuzz's list, each repeated to 32 KiB and to 256 KiB, on this PR's `scrub_text` in a release build.
- **The worst time ratio**, normalised to 1 for linear, is 2.34 (`%3Dkey`). #460 reported 2.42.
- **Timed again at 1 and 4 MiB,** the five worst units take 3.0 to 4.2 times as long at 4 MiB, so small sizes cause those ratios, not a quadratic path:

| Unit | 1 MiB | 4 MiB |
|---|---|---|
| `%3Dkey` | 0.054 s | 0.190 s |
| `&xl=1` | 0.151 s | 0.599 s |
| `,magnet:?` | 0.066 s | 0.280 s |
| `s3://%3D%26Bearer ` | 0.171 s | 0.697 s |
| `kt=%26<\r\n {` | 0.139 s | 0.420 s |

## Unchanged
- **#460's over-redaction list and known gaps all still apply.** The two narrowings would have shortened that list, and both are dropped.
- **No public API, log format or crawler change.**
- **The runner decides on any backport,** as for #460.
