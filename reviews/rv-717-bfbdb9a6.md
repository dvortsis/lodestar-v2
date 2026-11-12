VERDICT: CHANGES

# Delta review: PR #717 (RX-0), head bfbdb9a6, since 8424bfa6

Scope: the PR's own changes since 8424bfa6 (`regex.rs`, `tests/regex_plan.rs`, fuzz seeds/README/lock, decisions file, `logging/refs.rs`). The merge brought nothing else under those paths. `fuzz/Cargo.lock`'s only PR line is `regex-syntax` under `lodestar-search`. N1–N3 are fixed as claimed. The B1 fix covers bare `\P{..}` and `[\P{..}]`, but the same bypass still works through Perl classes and nested negated brackets inside a bracket.

## Blocking

### B2. A negated Perl class or nested negated bracket inside `[...]` still gets past the codepoint budget, at up to 0.69 s per pattern
- **Where:** `crates/search/src/regex.rs:381-385` (complement charge), `:495` (`ClassSetItem::Perl`), `:498` (`ClassSetItem::Bracketed` charged nothing).
- **Cause:** `hir_perl_unicode_class` builds `\D`, `\S` and `\W` unfolded and negates them (translate.rs:1095). Standalone, they are never folded. Inside a bracket, though, they are unioned into the class (`folded = false`), and the bracket's `unicode_fold_and_negate` (translate.rs:416) then folds the negated set itself. The PR charges the complement (`\d` is about 760 codepoints) for something that folds about 1.1M. The same happens with `[[^a]b]`: the inner `[^a]` is folded and negated, the literal `b` clears `folded` (interval.rs:118), and the outer bracket folds the whole complement. Each of those costs 2 codepoints. Set-op operands (`[\D--a]`, `[\D&&\D]`) fold the same way (translate.rs:627).
- **Measured** (release, `parse` including the plan, worst accepted repetition, the next step refused only by node or length limits):

  | Pattern | Accepted | Time |
  |---|---|---|
  | `[\S]` × 127 | yes | **691 ms** |
  | `[b[^a]]` × 51 | yes | 295 ms |
  | `[[^a]b]` × 51 | yes | 285 ms |
  | `[[^\x{0}]x]` × 46 | yes | 258 ms |
  | `[\D]` × 127 | yes | 255 ms |
  | `[a[^b]]{100}` × 42 | yes | 237 ms |
  | `[^\D]` × 102 | yes | 203 ms |
  | `(?i:[\D])` × 56 | yes | 111 ms |
  | `[\W\D\S]` × 17 | yes | 96 ms |

  That is about 90 times the claimed ~7–8 ms. RX-1's `regex` compile folds again (the decisions file says "as long again"), so the total is about 1.4 s of CPU, against §3's regex p95 < 500 ms. It sits just under the fuzz target's 1 s guard, which the fuzzer has not hit.
- **Fix:** charge what the enclosing bracket folds.
  1. Inside a bracket (`visit_class_set_item_pre`), charge a negated Perl item its own width, not the complement: `named_class(span, false)`.
  2. Charge a nested `ClassSetItem::Bracketed` with `negated` (and each set-op operand) `UNICODE_SCALARS` minus its items' width, or simply `UNICODE_SCALARS`.
  3. Keep the standalone `Ast::ClassPerl` at its complement, or at 0, since it isn't folded.
  4. Add `[\S]`×3, `[\D]`×3, `[[^a]b]`×3 and `[\D--a]`×3 to `refusals_have_stable_codes` and to the I1 table.
  5. Add a `[\S][\S]` seed and `[\D]`/`[[^a]b]` proptest leaves.
  6. Re-measure and correct `docs/decisions/2026-11-11-RX-0.md:31`. Its line "a negated Perl class ... counts its complement, which is exactly what gets folded" is wrong for Perl classes inside brackets.

## Important

### I2. The I1 test is deterministic but misses this shape
- **Where:** `crates/search/tests/regex_plan.rs:297-347`.
- **Assessment:** no sleeps; one `Instant` with a 10 s bound, which leaves about 1000× headroom over release. It is deterministic in practice and fine as written.
- **Gap:** its "one more step is refused" cases only probe the shapes already fixed. Add the B2 rows above. They need no clock and pin the regression.

## Nits

- **N4.** The comment at `regex_plan.rs:291` ("about 8 ms for the slowest") goes stale with B2. Update it with the re-measure.

## Verified clean
- **Nested negation and set ops over Unicode classes:** `[^\P{L}]`, `[\p{L}&&\P{Lu}]` and `[\P{L}~~\P{N}]` are capped at 17, at ≤ 6 ms. `\P{Any}`/`[\P{Any}]`/`\p{Any}`/full range are capped at 2, at 11–17 ms. `\W\D\S` standalone takes 0.2 ms. `[[:^alpha:]]` × 42 takes 0.2 ms.
- **Case-insensitive scoping:** `(?-i:\P{Any})` is still charged (conservative). `(?-i)[\D]` × 56 takes 0.17 ms.
- **Flags:** `(?s)`, `(?m)`, `(?x)`, `(?u)`, `(?U)`, `(?R)` and `(?is)` are all refused with `regex_flag_not_allowed`.
- **Plan soundness for negated classes:**
  - planning runs on the translated HIR, which folds before it negates, so the semantics are regex-syntax's own;
  - a negated class over 16 codepoints is "any";
  - `[^\s\S]` gives `Nothing`, which is correct.
  - No gram can exclude a true match.
- **Fuzz:** the target is unchanged. Seeds 11 (refused at the edge) and 12 cover bare negated classes, but none covers a Perl class inside a bracket (B2.5). The README row matches the target.
- **`refs.rs` flake fix:** a correct, minimal assertion.

## Commands
- `cargo fmt --check`: OK.
- `cargo +1.99.0 clippy -p lodestar-search --all-targets -- -D warnings`: OK.
- `cargo +1.99.0 test -p lodestar-search regex`: 13 unit tests passed (the filter skips the integration file); `--test regex_plan`: 6 passed in 4.4 s, I1's test included.
- **Release probe:** `crates/search/tests/zz_probe.rs`, a scratch file, since deleted.
