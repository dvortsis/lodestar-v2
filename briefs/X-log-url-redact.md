# X-log-url-redact: a URL secret survives `scrub_text` after a stray `://@`
**Model:** Opus, with `/security-review`: this is credential redaction in logs. The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-04; auto-approved under owner §2.33. Priority: security, ahead of feature slices.

## Finding (SRC-3's session, from a proptest failure on CI, reported 2026-11-04)
- The proptest `crates/log/src/url.rs` `tests::planted_secrets_never_get_through` failed once and passed on the next run.
- Shrunk input: the text before is `"://@"`, the scheme is `http`, the secret is `"a00aa0"`. The text is `://@http://u:<secret>@host.example/x`, and the secret comes out unredacted.
- So a leading bare `://` followed by `@` makes `scrub_text` (`url.rs:304`) consume or skip the real URL's userinfo. Suspects: the bare-`://` step at :365, `userinfo_in_text` (:389) choosing the first `@` across a following `://`, and the `next` bound.
- It's intermittent only because the proptest's generator rarely produces that prefix. The bug is deterministic.

## Do
1. **Reproduce first:** a deterministic unit test with exactly that input, which fails on `main`.
2. **Fix it** so that no prefix of junk can stop a later `scheme://user:pass@host` from being scrubbed. Each `://` occurrence gets its own userinfo search, bounded by the next `://`.
   - Keep the bounded look-ahead, so a line of `a://` or `magnet:` stays linear.
   - Keep the existing behaviour for every current test.
3. **Strengthen the property test** so it can't miss this class again:
   - generate prefixes from an alphabet weighted towards `:`, `/`, `@`, `?`, `#` and `[`;
   - plant the secret in userinfo, query credentials and passkey path segments;
   - add a regression seed file, or `proptest-regressions` (use whichever the crate already uses), for this case.
4. **Check the other scrubbers** in the file (`scrub`, `scrub_with`, `scrub_debug_forms`) for the same pattern, and fix any with the same cause.
5. **Linearity:** a bench or test showing `scrub_text` on 1 MiB of adversarial input (`://@` repeated) stays linear and finishes well under a second.
6. **Decisions file:** `docs/decisions/2026-11-04-X-log-url-redact.md`: the cause, the fix, and whether any released build (v0.5.1-rc.1 to rc.3) carries the bug. Check `release/v0.5.1`'s `url.rs`. If rc.3 carries it, say so. The runner decides whether it's backported; it's a log-redaction leak, class B (no crawler change).

## Own
`crates/log/src/url.rs`, its tests in the same file or `crates/log/tests/`, any proptest regression file for it, and the decisions file.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass, the new deterministic test fails without the fix, `/security-review` is clean, and a fresh self-review is done.

Branch `slice/X-log-url-redact`. The PR is titled "X-log-url-redact: Scrub URL credentials after a stray scheme separator". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with `send_message`. Never merge. Push once per round.
