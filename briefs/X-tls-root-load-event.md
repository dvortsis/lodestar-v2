# X-tls-root-load-event: A catalogued event that counts root certificate load errors from the OS store

**Runner brief, 2026-11-27T08:58Z.** Core, Opus (TLS trust and logging of outside input). Target < 400 changed lines. `/security-review` before you report ready.
- **Branch:** `slice/X-tls-root-load-event` from fresh `origin/main` (with #1125, X-sec-S8-25, merged as e3c5f20c).
- **PR title:** `X-tls-root-load-event: A catalogued event that counts root certificate load errors from the OS store`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do. A fresh Opus review gates the merge.
- The `runner/slices.yaml` entry exists (r18). **The runner refines its owns now (below); update that entry's `owns` in your PR to match**, and edit nothing else in that file or `runner/dashboard.yaml`.

## Why
Since #1125, routed connections verify with webpki over the roots in the OS store, read once per process (`roots()` in `crates/integrations/src/tls.rs`). When the store can't be read, a certificate in it fails to parse, webpki rejects one as a trust anchor, or Windows' Disallowed list removes one, nothing tells the operator: routed sources just fail with "the server's certificate isn't trusted". One catalogued event at load time makes that diagnosable.

## Read first
`docs/decisions/2026-11-27-X-sec-S8-25.md`, `crates/integrations/src/tls.rs` (`roots`, `os_roots`, `os_disallowed`, `without`, `usable`), how catalogued log events are declared and checked (`crates/lodestar/src/settings/golden/logs_catalog.txt` and the code that produces it; find the catalogue's source of truth), the log-safety rules in `runner/standards.md`, and docs/user §8.6.

## Do
1. When `roots()` first loads, emit **one** catalogued event with counts only: roots found in the store, load errors `rustls-native-certs` reported, certificates dropped as unusable trust anchors, certificates dropped by the Disallowed list (Windows), roots kept, and whether the store or the Disallowed list couldn't be read (a fail-closed load). Level: info when roots were kept and nothing failed; warn otherwise.
2. **Never log anything from outside input:** no subject, issuer, fingerprint, file path or error text from a certificate or the store. Counts and fixed reason words only. A test asserts the event's fields are exactly the catalogued ones.
3. Add the event to the catalogue and regenerate `logs_catalog.txt` with the repo's tooling, never by hand.
4. If it fits in a sentence, mention the event in docs/user §8.6's troubleshooting text ("if routed sources all say the certificate isn't trusted, look for <event> in the log"). Use the user-docs skill. Ask the runner first if this needs more than that one file.
5. Tests: counts for a mix of good, unparsable and duplicate certificates (fixtures exist under `crates/integrations/tests/fixtures`), a fail-closed load, and a store with nothing usable. No fixed sleeps.

## Owns
`crates/integrations/src/tls.rs`, `crates/integrations/tests/**`, `crates/sources/**` (only if the event is emitted from there), the log catalogue's source file(s) and `crates/lodestar/src/settings/golden/logs_catalog.txt`, `docs/user/08-add-companions.md` (one sentence), `docs/user/revisions.md` (if the guide changes), `docs/decisions/*-X-tls-root-load-event.md`, and this slice's own `runner/slices.yaml` owns line. **Not** `crates/lodestar/src/config.rs`, `crates/lodestar/src/metrics.rs` or `docs/metrics.md` (CR-0w holds them), and not `crates/dht/**`. If the catalogue's source is in a file CR-0w owns, stop and message the runner.

## Done
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (or the touched crates plus `lodestar` if the disk allowance can't hold a full build; say which). Run `cd tools/status-connector && npm test` after the slices.yaml edit. Report `ready <n> <full sha>` to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) once ci-ok, web-ok and e2e-ok are green. Push once per round of fixes.

No AWS or cloud calls, no new accounts, no spending, no force-push. Held security detail stays off every branch.
