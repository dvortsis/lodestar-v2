# X-enr-page-flag: the enrichment parsers report what they saw, replacing the plugins' stopgap checks
**Model:** Opus (untrusted-input parsers). `/security-review`, then a fresh Opus reviewer. Runner brief, 2026-11-05T20:25Z.

## Why
ENR-4 (#503) and ENR-5 (#506) shipped with a HOLD: neither plugin may be registered until the bounded parsers in `crates/sources-feeds` report the shape of the answer they read.

Today each plugin re-reads the raw body to tell a real miss from an error object:
- `tpdb.rs` uses `data_start`, a hand-written byte scanner.
- `stashdb.rs` uses `shape`, a second, typed serde read of the whole body.

Two readers can disagree. That gap is what the reviews found:
- `findScene` taken as the match;
- duplicate keys read as a miss;
- `errors` after an early stop.

The fix is one reader. The parser itself says what it saw.

## Do (branch `slice/X-enr-page-flag` from `origin/main`)
1. **`crates/sources-feeds/src/tpdb.rs` `Page`.** Report whether the root `data` key was seen and its kind: list, object, null, or other/absent. Report any root keys other than `data`/`meta`, and whether a second `data` was seen. The first `data` wins, as the parser's house rule says, so a repeat is reported, not used.
2. **`crates/sources-feeds/src/stashdb.rs` `Page`.** Report whether `data.searchScene` was seen and its kind, whether `findScene` was present (even null), whether a non-empty `errors` array was present wherever it appears, and any repeated key at root or in `data`. StashDB's parser already skips a scene on a repeated key; extend that to the envelope. Report whether trailing bytes followed the root value.
3. **The plugins** (`crates/sources-plugins/src/tpdb.rs`, `stashdb.rs`).
   - Replace `data_start` and `shape` with decisions made on those flags.
   - Keep today's exact rules: a miss only on an empty list (or null where allowed); an error on everything else; a match only as the service's top result.
   - Every test in `tests/tpdb.rs` and `tests/stashdb.rs` must still pass unchanged, including the probes from the #503 and #506 reviews. Add any probe that isn't there yet: findScene variants, duplicate keys at each level, `errors` after the first item, trailing garbage, a body cut after the first item.
   - Remove the HOLD's parser precondition line from both decisions files. Leave the other HOLD items (the owner's terms check, host wiring) in place.
4. **Fuzz.** Extend the `tpdb` and `stashdb` fuzz targets to assert the flags are consistent: for example, never `NoMatch` together with a non-empty `errors`. Run each target for 10 minutes and state that in the PR.
5. **No behaviour change for a well-formed answer.** Keep the property tests, and keep the caps on bytes and depth exactly as they are.

## Own
- `crates/sources-feeds/src/{tpdb.rs,stashdb.rs}` and their tests;
- the answer-parsing functions in `crates/sources-plugins/src/{tpdb.rs,stashdb.rs}` and those plugins' tests;
- `fuzz/fuzz_targets/{tpdb.rs,stashdb.rs}`;
- `docs/decisions/2026-11-05-X-enr-page-flag.md`, plus the HOLD-line edits in the ENR-4 and ENR-5 decisions files.

**Concurrent edits.** X-adult-catalogue (#507, open) changes only `const MANIFEST` to `pub(crate)` in those plugin files. Whichever PR merges later brings `main` in.

## Done when
- `cargo fmt --check`;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- `cargo test --workspace`, or at least `sources-feeds` and `sources-plugins`. Say which.
- `/security-review` on the diff.

Then:
- Open the PR "X-enr-page-flag: The enrichment parsers report what they saw" with the label `full-ci`.
- Add the `runner/slices.yaml` entry.
- Follow `.claude/skills/steward/SKILL.md`.
- Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head.

Never merge. Push once per round. No fixed sleeps.
