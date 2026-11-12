# Review: PR #755 (X-rx0-nits), head ce3bde42

Reviewer: a fresh, independent reviewer. Head confirmed as `ce3bde4236053c1ac05250cb7967884f4df67dbf`, reviewed against the three-dot diff from `origin/main`. Release builds ran on the shared 4-core container. Load average swung between 0.9 and 6.4 during the session, so every time below is either the best of 9–15 runs or a ratio against `\p{Any}\p{Any}` (accepted before and after) timed in the same loop.

**Verdict: MERGE.** There are no Blocking or Important findings.

The invariant holds. Every fold the translator does is charged at or above its real work, and nothing newly accepted runs slower than the existing baseline. I checked this three ways:
- I read regex-syntax 0.8.11's `translate.rs`.
- I ran a differential over about 123,000 random candidates against an independent reference that mirrors the translator's folds. It found no undercharges.
- The same differential catches every mutation I made to the folded flags. Two of those mutations are not caught by the committed tests (Nit 1).

## Summary of the change

| Nit | What changed |
|---|---|
| N1 | The ASCII item's own fold is now charged: `fold_cost` of the un-negated ASCII set. |
| N3 | `class_set`/`class_item` return `(set, any_part_folded)`, and `fold_cost_of(set, folded)` widens by `cased()` only when some part was folded already. |
| N4 | `Cased.lo = 0`. |
| N2, N5 | Wording and timing claims. |
| Test, seed | A new unit test checks that an unfolded bracket's model set equals regex-syntax's case-sensitive translation, plus three new cost assertions. Four boundary rows moved, and fuzz `seed17` was added. |
| YAML | `runner/slices.yaml` gains only the X-rx0-nits entry, and `runner/dashboard.yaml` gains only its placement line. |

## Checks run

| Check | Result |
|---|---|
| `cargo fmt --all --check` (stable; 1.99.0 has no rustfmt here) | pass |
| `cargo +1.99.0 clippy -p lodestar-search --all-targets -- -D warnings` | pass |
| `cargo +1.99.0 test -p lodestar-search` | pass: all 15 binaries, including the lib's 250 tests and `regex_plan`'s 6 (timing table, proptests) |
| `cd tools/status-connector && npm test` | pass: "579 slices, 27 roadmap steps, 24 gates, 63 conditions" |
| `cargo +nightly fuzz run regex_plan <corpus> fuzz/seeds/regex_plan -- -max_total_time=300` | pass: 31,522 runs, cov 4820, no crash. All 17 seeds loaded, `seed17` included. cargo-fuzz was built offline into my target dir. |
| Hygiene | No 12-digit numbers, no local times or zones, no model identifiers. The only ones are the existing `model: opus` schema field and the harness-supplied commit trailer. |

## N3 soundness: is the "exact set" exact?

I read regex-syntax 0.8.11's `hir/translate.rs`. These are the calls on a bracket's frame:
- **Literal and Range** (`visit_class_set_item_post`) are `cls.push(range)`, with no fold, whatever the `i` flag.
- **Perl** (`hir_perl_unicode_class`) builds the table class and negates it, with no fold ("already closed under Unicode simple case folding").
- **ASCII** (`hir_ascii_unicode_class`) runs `unicode_fold_and_negate`, so it folds before negating. The model marks it folded (`true`) and charges the un-negated set (N1).
- **Unicode** (`hir_unicode_class`) folds before negating. The model marks it `true`.
- **Nested bracket**: a new frame is pushed at pre and folded and negated at post, then unioned into the parent. The model marks it `true`.
- **Binary op**: two new frames, both folded at post, then the operation, unioned into the parent frame. Each operand is charged on its own flag, and the result is marked `true`.
- **Union**: items push into the same frame. The model ORs its items' flags.

So a set reaches a fold without any earlier fold only if it's built from Literal, Range, Perl and Empty items. In that case the translator's frame holds exactly the case-sensitive union, which is what the model computes.

The flag scopes can't reach the exact path with a set that was folded earlier:
- `(?i)`, `(?i:…)` and `(?-i)` never change what's pushed. They only decide whether a fold happens, and the model charges every fold whatever the flag.
- Flags can't appear inside a bracket.
- Every flag other than `i` is refused, including `(?-u)`/`(?u)`, so byte classes never arise.
- Classes are translated once per AST node; repetition does not re-translate.

The IntervalSet `folded` flag (an empty class starts folded, and a fold is skipped when the flag is set) can only make the real work cheaper.

**The differential.** I wrote a scratch reference, kept outside the repo. It walks the AST exactly as `translate.rs` does, with every frame, `push`/`union` and `case_fold_simple`. Before each fold call it adds `fold_cost` of the frame's real contents, always as if `(?i)` were on, and it ignores the `folded` shortcut, so it's an upper bound on the real work.

I generated about 123,000 candidates by greedy fill, about 86,000 of them accepted. They mixed:
- brackets nested 3 deep, with `^`, `--`/`&&`/`~~`, boundary ranges (U+0000, U+0040/41, U+1E943/44, surrogate edges, U+10FFFF), cased edge literals (`ſ K K ß Σ ς İ ı ǅ`), Perl, ASCII and Unicode items;
- `(?i:…)`/`(?-i:…)` wrappers, repetitions and `|`.

Results:
- **Undercharges (model < reference): 0.**
- In every exact-path bracket I spot-checked, model equals reference to the unit: `[\W_]`, `[^/]`, `[\w.-]`, `[\S\W\D]`, `[\x{1E943}-\x{10FFFF}]`, `[\x{0}-\x{40}\x{1E944}-\x{10FFFF}]`, `[ſK\x{212A}]`, `(?-i:[\x{80}-\x{FFFF}])`.
- The largest over-charge ratio among real costs over 100k units was 10.7×. It comes from widening a folded set.

## Adversarial patterns and boundary moves

`ratio` is parse time over the baseline `\p{Any}\p{Any}` (about 9.8–10.4 ms when quiet), timed in the same loop.

| Pattern × copies | Old → new limit | Next refused for | Parse | `regex` compile (for information) |
|---|---|---|---|---|
| `[[^a]b]` ×2 | 1 → 2 | class | 10.2 ms, ratio 1.01 | 0.97× base |
| `[b[^a]]` ×2 | 1 → 2 | class | 10.6 ms, ratio 1.07 | 0.99× |
| `[\w--\d]` ×26 | 18 → 26 | class | 5.2 ms, ratio 0.54 | 1.10× |
| `[\W_]` ×62 | 40 → 62 | compiled size | 2.7 ms, ratio 0.28 | 1.78× (see Nit 3) |
| `[^/]` ×127 | 60 → 127 | nodes | 0.3 ms | 0.07× |
| `[^a]` ×127 | 60 → 127 | nodes | 0.2 ms | 0.07× |
| `[a-z0-9]` ×63 | 60 → 63 | nodes | 0.2 ms | 0.03× |
| `[\W\d]` ×57 | 39 → 57 | compiled size | 3.3 ms | |
| `[\x{0}-\x{40}\x{1E944}-\x{10FFFF}]` ×15 | 4 → 15 | length | 0.04 ms | |
| `[\w.-]` ×51 (unchanged) | 51 → 51 | nodes | 9.9 ms quiet, ratio 0.95 | 2.31× (existing) |
| `\p{Any}` ×2, `[\S]` ×2, `[\D]` ×4, `[\D--a]` ×2, `[[:^alpha:]]` ×2, `[_\pL]` ×27, `[\x{80}-\x{FFFF}]` ×6, `[\x{41}-\x{1E943}]` ×3, `[\x{1E943}-\x{10FFFF}]` ×4, `[\S\W\D]` ×2, `[a-z\S]` ×2 | unchanged | class | 8–11 ms, at or under the baseline | |

**Accepted after, refused before.** In the differential, 3,623 accepted candidates were refused on `main`. I re-timed the 24 slowest against the baseline in the same loop. Every one came in at 0.74–1.05× the baseline, so at most about 10.5 ms. Examples:
- `\D{0,6}(?i:[ß]){0,3}[\x{40}-\x{FF}\p{Any}\x{0}-\x{E000}]+` at 1.00;
- `[^[^\x{0}-\x{10FFFF}]\S\D][\x{41}-z]` at 0.96;
- `ı+(?-i:\D)(?i:[^\x{0}-z])+[[\S]s](?-i:Ω)` at 1.01.

Nothing came near the 50 ms bound. The single samples of 30–48 ms taken while 4 searches ran in parallel all fell back to the baseline when re-timed.

## Mutation results

Each mutation was applied in my worktree, tested, and reverted; `git status` was clean afterwards.

| Mutation (folded flag treated as unfolded) | Committed tests | Scratch differential |
|---|---|---|
| M1: nested `Bracketed` returns `false` | **fail**: `fold_costs_follow_what_gets_folded` (`[[a]b]` vs `[ab]`) | fails, e.g. `[^[\s\x{24F}-\x{FFFF}]\d\d]` |
| M2: Unicode item returns `false` | **fail**: `regex_plan` row `[\P{Any}]`×2 accepted | not run (the tests catch it) |
| M3: ASCII item returns `false` | **pass, not caught** | fails: `[^ı[:alpha:]]` model 42,358 < reference 42,408 |
| M4: binary-op result returns `lhs_folded \|\| rhs_folded` | **pass, not caught** | fails: `\x{212A}[^k--ς]` model 75 < reference 125 |
| M5: `fold_cost_of` never widens (exact everywhere) | **fail**: both the unit test and `regex_plan` | |
| M6: Union `any_folded &=` | **fail**: `regex_plan` row `[_\pL]`×28 | |

The direct N3 mutation, making the exact path also accept folded sets (M5, and M1 for brackets), is caught.

## Findings

### Blocking
None.

### Important
None.

### Nits

**1. Two of the folded flags aren't pinned by any test.**
- **Where:** `crates/search/src/regex.rs:449` (ASCII `(set, true)`) and `:427` (`Ok((lhs, true))`).
- **What happens:** flipping either flag to "unfolded" (M3, M4) passes the whole suite, so a later refactor could drop them unnoticed.
- **Impact:** small. The undercharge they would cause is bounded by the fold images an ASCII item or operand adds: at most about 75k units a bracket, and in practice about 700 units for `[:upper:]`.
- **Fix:** add two assertions to `fold_costs_follow_what_gets_folded`, for example:
  - `assert!(cost(r"[[:upper:]]") > cost(r"[A-Z]") * 10)`;
  - `assert!(cost(r"[A-Z--0]") > cost(r"[A-Z]") * 10)`.
  
  A stronger fix is a unit or proptest version of the reference differential: mirror `translate.rs`'s frames, take `fold_cost` of the real frame before each `case_fold_simple`, and assert model ≥ reference over random brackets. It caught M1, M3 and M4 here within 400 patterns.

**2. A one-codepoint property is charged nothing. This predates the PR; `class_text` isn't changed here.**
- **Where:** `crates/search/src/regex.rs:376-379`. `class_text` maps any non-class HIR to `ClassUnicode::empty()`, but `Hir::class` turns a one-codepoint class into a `Literal`.
- **Example:** `\p{Zl}` is charged 0 against a real 1 unit. `[\p{Zl}]`'s model set is empty, although its parts are folded, so it's widened and over-charged.
- **Impact:** at most one codepoint's walk (≤ 25 units) per such item, about 0.1% of the budget over 256 items. It can't loosen a bound in any meaningful way.
- **Fix:** handle `HirKind::Literal` the same way the new test does: decode the UTF-8 and return a one-range class.

**3. For RX-1, not for this slice: verifier compile time is now the larger cost for some newly accepted shapes.**
- `[\W_]`×62 (newly accepted) parses in 2.7 ms, but `regex` compiles it in about 1.8× the baseline (about 17 ms).
- The existing `[\w.-]`×51 compiles in about 2.3× (about 32 ms).
- Both are held by the compiled-size estimate, not the fold budget, and `\w`×62 was already accepted.
- So "the verifier's compile takes as long again" (`regex.rs:52-53`) understates it for these shapes.
- **Fix:** none needed here. RX-1 should keep `regex`'s own size and DFA limits and time the compile, as the decisions file already says. The comment could say "up to about twice as long" when it's next touched.

**4. Wording.**
- "No bound loosened" (decisions file, X-rx0-nits section) is right about the limits and the invariant, but the accepted set does grow (the four moved rows).
- **Suggested fix:** "no limit changed and no charge fell below the work it stands for; brackets with nothing folded before them got cheaper."
- The test's `[^/]`×127 is pinned only in the decisions file and through `cost("[^/]") == 1`, not as a row. That's fine, but a `row(r"[^/]", 127, Reason::TooManyNodes)` would pin it.

## Other notes

- **Arithmetic.** With `lo = 0`, `end.min(hi).saturating_sub(start) + 1 ≤ 0x1E944`, and the `* (start <= hi && end >= lo)` guard zeroes the out-of-span case. `b ≥ a` still holds for the taken table ranges. All sums are u64 well under 2^40, and the budget uses `saturating_add`. The fuzz run and about 123,000 differential candidates produced no panics.
- **Tests and timing.** No fixed sleeps. The timing table uses a 10 s bound per case, about 30× the debug-build time measured here, so it won't flake on Windows CI. The new unit test is meaningful: it compares against regex-syntax's own case-sensitive translation and asserts that no fold was charged before.
- **Timing claims.** "About 9–12 ms" matches what I measured: the baseline at 9.8–10.4 ms quiet, and `[\w.-]`×51 at 9.9 ms quiet and 13.4 ms under load.
- **Scope.** Only the owned files changed, plus the slice's own `slices.yaml` entry and one `dashboard.yaml` placement line. Appending a section to RX-0's decisions file follows the X-tz0-polish precedent and the slice's `owns`.

## Cleanup

I removed my worktree, `scratchpad/wt-755` (`git worktree remove`), and my target dir, `scratchpad/tgt-755`, which held the cargo-fuzz binary and the fuzz corpus. Nothing else in the scratchpad was deleted. My scratch inputs stay in the scratchpad:
- `rx-scratch/` (reference differential);
- `mut.py`;
- `regex.rs.orig`;
- `new_regex.rs.scratch-orig`;
- `diff-*.log`;
- `chk-*.log`.

I wrote `mut.py` and `regex.rs.orig` straight into the scratchpad root without first checking whether files of those names existed. If another session had files there under those names, I overwrote them.

VERDICT: MERGE
