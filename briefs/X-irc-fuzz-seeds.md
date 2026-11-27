# X-irc-fuzz-seeds: Seeds for the irc_announce fuzz target, with coverage recorded

**Runner brief, 2026-11-27T07:40Z.** Core, Opus (an untrusted-input fuzz corpus). Target < 300 changed lines, most of it seed files.
- **Branch:** `slice/X-irc-fuzz-seeds` from fresh `origin/main` (with #1130 merged as 68799e1b).
- **PR title:** `X-irc-fuzz-seeds: Seeds for the irc_announce fuzz target, with coverage recorded`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do. A fresh Opus review gates the merge.
- The `runner/slices.yaml` entry already exists (r18). Don't add another; edit nothing else in that file or `runner/dashboard.yaml`.

## Why
The runner's audit of #1111 (IRC-1) found that `fuzz/fuzz_targets/irc_announce.rs` has no seed corpus, so the fuzzer starts from nothing and rarely reaches the matcher's deeper paths through a parsed definition.

## Read first
`fuzz/fuzz_targets/irc_announce.rs` (its input layout: two bytes of big-endian definition length, the definition, then the server line), `crates/sources-irc/data/announce/template.def`, `crates/sources-irc/src/announce*`, IRC-1's decisions file, `fuzz/README.md` (how other targets' seeds are made and where they live, e.g. `fuzz/seeds/krpc/`), and the CI fuzz job in `.github/workflows/`.

## Do
1. Add `fuzz/seeds/irc_announce/`: the shipped `template.def` framed in the target's input layout with a matching announce line, plus a handful of ordinary announce lines (matching, non-matching, with and without size, with tags, the fixed definition's shape) and one or two definitions at their list caps. Make them with a small generator or a test that writes them, as other targets do; never hand-edit binary bytes. Keep seeds small (each well under 4 KiB).
2. Seeds are ordinary, valid or near-valid inputs. Don't craft inputs aimed at the IRC-2 carry-forwards in `runner/slices.yaml` (passkey host, loopback hosts, redirects); those belong to IRC-2.
3. If cargo-fuzz is installed, run the target for at least 10 minutes from the new corpus and record the coverage reached (edges or features, and the command) in `fuzz/README.md`'s `irc_announce` row or a short note under it. If it isn't installed, say so in the PR and record what CI's fuzz job reports. Don't change the target itself, and keep the CI fuzz job's time as it is.

## Owns
`fuzz/seeds/irc_announce/**`, `fuzz/README.md` (the irc_announce row or a note under it), `docs/decisions/*-X-irc-fuzz-seeds.md` (only if you make a call worth recording), and a generator test if you need one (tell the runner where before adding it, since `crates/sources-irc/**` is held by X-sec-S8-25; a generator under `fuzz/` is preferred). Not `fuzz/Cargo.lock` (X-sec-S8-25 touches it) and not any other target's seeds.

## Done
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` if you touched Rust. Report `ready <n> <full sha>` to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) once ci-ok, web-ok and e2e-ok are green. Push once per round of fixes.

No AWS or cloud calls, no new accounts, no spending, no force-push. Held security detail stays off every branch.
