# Lodestar v0.5 code review: Area 2 results

Date: 2026-10-24

Review instructions: [codex-review-v0.5.md](codex-review-v0.5.md).

Reviewed a fresh clone of `main` at `2d5e761d30cb1b7947fb09493959668eabcb84ab`, newer than the instructions' v0.5 gate commit `4667277`. Locations below refer to that commit. This session covered **Area 2: authentication, API keys and request limits**. [Area 1](codex-review-v0.5-area-1-results.md) is complete; Area 3 remains unreviewed.

No source files were changed and no GitHub actions were taken. This report was saved at the owner's request.

Validation and scope:

- Reviewed auth state, session/key guards, password confirmation, proxy trust and forwarded-client handling, bounded rate-limit buckets, key creation/regeneration/revocation, and secret logging. Traced route protection, including events, logs, exports/downloads and saved-search feeds, through the production router and server's TCP peer configuration.
- Read architecture sections 12 and 20.11, the M2 auth implementation decisions, relevant owner decisions, runner standards, and the B12 implementation decisions. Checked existing unit, property and integration tests against candidate findings.
- `cargo test --locked --offline -p lodestar-api` passed: **392 passed, 0 failed, 2 ignored**. The ignored tests are release-mode timing/overhead measurements. This was a Windows run; Unix-only permission tests were not exercised.
- Two isolated `rustc --test` probes, supplied through stdin and linked against the compiled production crate, failed the expected-state assertions below. No test files were added to the clone. Its final `git status --short` was empty.
- Excluded missing Search-key route wiring from findings because the reviewed v0.5 user guide explicitly marks API-key integrations as planned. Also excluded the documented shared-overflow limiter behavior and regeneration's deliberate insert-before-delete failure semantics. Saved-search deletion cascades to its scoped feed tokens and already has test coverage.

### 1. Failed auth-file saves still replace the active credentials

Severity: medium  
Location: `crates/api/src/auth.rs:355` (also setup at lines 296–298)  
Input / scenario: An authenticated client sends `POST /api/v1/password` with the correct old password and a valid new password while writing or renaming `auth.json.tmp` fails, for example after the data volume becomes read-only or runs out of space. A deterministic reproduction creates a directory at the temporary-file path in a disposable auth fixture before calling the same production method. Initial `POST /api/v1/setup` has the same failure.  
Expected: The atomic auth persistence described in architecture's M2 implementation decisions should leave the previous credentials and sessions in effect when the operation returns a storage error. A failed initial setup should remain unclaimed. The existing `set_proxy` and `rotate_sessions` implementations already restore their previous state on save failure.  
Actual: `state.password = Some(hash_password(new)); state.key = random::<32>(); self.save(&state)?;` changes shared state before persistence succeeds. The HTTP handler maps that error to HTTP 500 and returns no replacement cookie, but all old sessions and the old password have already stopped working. The new password works until restart, when the unchanged file restores the old password and session key. Setup similarly executes `state.key = random::<32>(); state.password = Some(encoded); self.save(&state)?;` before clearing the token: after a failed save, `needs_setup()` is false, login accepts the attempted password, and the setup token is still pending; restart returns the instance to setup mode. The entry points are `auth/http.rs:295` and `auth/http.rs:218`. Existing tests cover successful saves/restarts and rotations, but not these failed-save state transitions.  
Confidence: high  
Suggested fix: Construct and persist the replacement state before publishing it under the write lock, or restore both the old password hash and old session key on every save error. Apply the same transaction boundary to setup, and consume its token only after successful persistence.

Reproduction evidence (production `Auth` methods, temporary fixtures only):

```text
After failed save: old_cookie=false, old_password=false, new_password=true;
after restart: old_password=true, new_password=false
failed_password_save_preserves_active_credentials ... FAILED

After failed setup: needs_setup=false, token_pending=true, accepts_password=true;
after restart: needs_setup=true
failed_setup_save_leaves_setup_pending ... FAILED

test result: FAILED. 0 passed; 2 failed
```

Minimal reproduction of the password-change case, using the crate's existing `tempfile` dependency:

```rust
use lodestar_api::auth::{Auth, AuthError};

#[test]
fn failed_password_save_preserves_active_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.json");
    let auth = Auth::load(&path, false).unwrap();
    let ip = "127.0.0.1".parse().unwrap();
    let old = "correct horse battery";
    let new = "a different password";
    let cookie = auth.setup(ip, &auth.setup_token().unwrap(), old).unwrap();

    std::fs::create_dir(path.with_extension("json.tmp")).unwrap();
    assert!(matches!(
        auth.change_password(ip, old, new),
        Err(AuthError::Storage(_))
    ));

    // Fails: the unsuccessful operation already invalidated this session.
    assert!(auth.verify(cookie.split(';').next().unwrap()));
}
```

Checked and clean: `crates/api/src/auth/http.rs`, `crates/api/src/auth/limit.rs`, `crates/api/src/auth/proxy.rs`, `crates/api/src/auth/log_events.rs`, `crates/api/src/keys.rs`, `crates/api/src/dto/keys.rs`, `crates/api/src/lib.rs`, `crates/api/src/request_log.rs`.
