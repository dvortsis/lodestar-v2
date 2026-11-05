# X-log-url-redact-2: the remaining URL-credential leak classes in log scrubbing
**Model:** Opus (untrusted text, security). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-05.

## Why
X-log-url-redact (#432, merged 36a96ff6; in rc.4) closed the stray-`://` leak and every case where it showed a secret `main` hid. Its differential fuzz and reviews also found classes that leak on `main` and on every release so far, which it left out of scope (runner call, 2026-11-04, flagged under §2.19). This slice closes them. Its decisions file and PR body list them: `docs/decisions/2026-11-04-X-log-url-redact.md` ("Known gaps for X-log-url-redact-2").

## Scope
1. Password classes that leak when the URL stands alone: `://` before or after the secret; a raw line break in the password, including with no userinfo `@` before it (`ssh://u:\n<s>@h`, `wss://u%3A<s>\n@h`); `/` plus a hard stop with nothing marking the userinfo.
2. Whitespace or a line break inside a magnet link.
3. A magnet inside a `://` URL's path, query or fragment ends at the outer URL's soft end (B-log1); a naive cut breaks the `Next::from` cache, so design it.
4. Percent-encoded `magnet%3A%3F` in free text.
5. An escaped `\"` inside a Debug-form magnet value.
6. Malformed IPv6 `[::1]]` in a nested query.
7. URL forms without `://`: `//u:p@h`, `http:/u:p@h`, `http:%2F%2F`; credential values glued on with `:` or `%3D`; mixed encodings such as `%2F%2F`.
Rule: never too little. Over-redaction is acceptable when named in the decisions file; a regression against `main` (a secret this PR shows that `main` hides) is blocking.

## Do
Same method as #432: deterministic tests that fail on `main` for every class; extend `planted_secrets_never_get_through` and the other properties; the differential fuzz against `main` (report inputs, seeds, counts both ways: 0 new leaks); speed stays linear with the 1 MiB adversarial bound; no panics on any input.

## Own
`crates/log/src/url.rs`, `crates/log/tests/**`, `crates/log/proptest-regressions/**`, `docs/decisions/2026-11-05-X-log-url-redact-2.md`.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `/security-review`. PR "X-log-url-redact-2: …" against `main` from `slice/X-log-url-redact-2`. Follow `.claude/skills/steward/SKILL.md`; message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with PR number and head via the claude-code-remote `send_message` tool. Never merge. Push once per round. No fixed sleeps.
