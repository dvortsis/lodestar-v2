# CMP-3: Companion credentials (generated, sealed, rotated, written into each companion's config file)

**Model:** Opus. **Review:** a fresh Opus reviewer, plus `/security-review` and an unanchored second read. **Branch:** `slice/CMP-3`, cut from `origin/main`. **PR title:** `CMP-3: <title>`. **Steward:** follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Read first
- `docs/plan/2026-11-06-v06-companions.md`: §3 (store impact, no migration), §4's CMP-3 bullet, and §5 Q5, Q10, Q13 and Q19.
- `docs/owner-decisions.md` §2.46 and §2.47. cq5 (revealing a credential) is decided, but the reveal itself is CMP-6. This slice only provides what the reveal will need.
- `docs/architecture.md` §12, §18.7 and §18.10.
- `crates/integrations` `secret::SecretStore`: `seal`, `open`, `reseal` and the context strings. SRC-4's sealed credentials are the pattern to follow.
- `crates/companions/` as CMP-0 left it: `catalogue.rs`, `inputs.rs` (`CompanionId`) and `adapter.rs`.
- `runner/standards.md` is the bar.

## Owns
- `crates/companions/src/credentials.rs`
- `crates/companions/src/config_files.rs`
- `crates/companions/tests/credentials.rs`

**Shared edits the runner allows:**
- one `mod` line each in `crates/companions/src/lib.rs`, plus `pub use` if needed;
- `crates/companions/Cargo.toml`: the new dependency `pbkdf2` (with `hmac` and `sha2`, which are already in the lockfile) and `quick-xml` (also already in the lockfile), plus `rand` or `getrandom` if the crate needs the OS RNG and one is already a workspace dependency. Pin them like the crate's other dependencies;
- `Cargo.lock`;
- `web/licenses/rust.json`, if the licence generator lists the new crate;
- one `docs/decisions/2026-11-07-CMP-3.md`.

Anything else goes to the runner first.

## What to build
1. **Generation.**
   - Credentials come from the OS RNG: 32 bytes, encoded as each companion needs. Prowlarr's `ApiKey` is 32 lowercase hex characters, matching Prowlarr's own format. qBittorrent's WebUI password is URL-safe base64 or similar, without characters its config escaping mangles.
   - Nothing is seeded from time or a PRNG.
2. **Sealing.**
   - Each credential is sealed with `SecretStore::seal` in context `companion:<id>:<field>`. It is stored as a `settings` row `companions.<id>.secret.<field>` (plan §3). No migration.
   - `reseal` (rotation) covers these rows the same way it covers SRC-4's. Add a test that a rotation reseals them and that they still open afterwards.
   - **Regenerate** replaces a credential, seals the new one and returns it for writing into the config file. It never returns the old one. Write it as an API that CMP-5 and CMP-6 call; build no UI or endpoints here.
3. **Config writers (`config_files.rs`).**
   - **qBittorrent:** `qBittorrent.conf` gets `WebUI\Password_PBKDF2` in qBittorrent's own format: `@ByteArray(<salt b64>:<hash b64>)`, PBKDF2-HMAC-SHA512, 100,000 iterations, a 16-byte salt from the OS RNG. Also set the WebUI username, and bind the WebUI address to `127.0.0.1`.
   - **Prowlarr:** `config.xml` gets `ApiKey` and `BindAddress` set to `127.0.0.1`, written with `quick-xml`, never by string formatting.
   - **Writers emit only the keys the template names.** When a file already exists, update only those keys and keep the rest as it was. Refuse input that would inject keys or sections: newlines in values, a `[` section header, or XML special characters (which quick-xml escapes).
   - Write atomically: a temp file in the same directory, fsync, then rename. Give the file private permissions (0600 on Unix; on Windows inherit the companion folder's ACL; no new Windows ACL code here).
   - Everything is written before the companion's first start.
   - Use a golden test per companion, with a fixed salt injected through a test-only seam, so the output bytes are exact. Add a test that qBittorrent accepts the format: verify the PBKDF2 against a known vector computed independently, for example with Python's `hashlib.pbkdf2_hmac` in the test comments.
4. **Guided installs (Q19).**
   - A pasted API key or temporary password is accepted once, validated (length and charset), and sealed the same way.
   - For qBittorrent, the temporary password is replaced by a generated one. This module provides the function; CMP-8b calls the API.
   - No secret is ever written into a generated compose service or template.
5. **Never logged, never exported.**
   - Use a `Secret`-style wrapper whose `Debug` and `Display` print redacted text, and zeroize on drop if the workspace already uses `zeroize`.
   - Add a test that runs generate, seal, regenerate and the writers with logging captured, then greps every emitted event and string for the plaintext. It must find nothing.
   - Make sure the exports and support-bundle code paths can't see these rows: they use the same `secret.` row filter SRC-4's do. Add a test if that filter is reachable from this crate; otherwise note it for CMP-5.
6. **Errors** are typed, carry no secret material, and contain no `unwrap` or `expect` outside tests.

## Tests and gates
- Property tests for the config writers: arbitrary existing files plus our keys produce a file that parses and contains our keys exactly once, with other content unchanged. Arbitrary values are either refused or round-trip.
- No fixed sleeps.
- Before pushing: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (at minimum `-p lodestar-companions`).
- **Size:** about 550 lines per the plan; stay well under 1,500.

## Decisions you may make (§2.33; record each in the decisions file)
Encodings, the qBittorrent username default, how to treat an existing malformed config file (refuse with a clear error, never overwrite blindly), and the API shape for CMP-5, CMP-6 and CMP-8b. Anything that loosens a plan rule goes to the runner.

When CI is green, Windows included, message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA.
