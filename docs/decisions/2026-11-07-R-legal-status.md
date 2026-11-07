# R-legal-status decisions (#558)

**The operator guidance on `release/v0.5.1` names its release baseline** (`docs/owner-decisions.md` §2.48 on `main`; build-plan §0 row R-legal), before the owner tags `v0.5.1`.

- **The §2.48 check of rc.5** (`release/v0.5.1` at `a0eb0a9`): every factual claim in `docs/legal/operator-guidance.md` is true of v0.5.1 and every "(planned)" marker is correct. The Legal Notice is current (version 3, "Applies to Lodestar v0.5.1"). The one gap: the guidance's status line said "the current version", not the baseline §2.48 asks for, and its date predated #540.
- **Status line:** now names Lodestar v0.5.1 and version 3 of the Legal Notice, dated 2026-11-06, as `main`'s copy already does for its own baseline.
- **Two editorial corrections for this build:** v0.5.1 has no inbound integration endpoint, so "or an integration" is dropped from the opening; and keys here carry search, torznab and feeds scopes, so the filters sentence says API keys and feed tokens only read and can't reach the filters API, instead of "only the search scopes" (`crates/api/src/keys.rs` scopes; every `/filters/*` route sits behind `auth::protected`, session only; only `GET /feeds/{id}` takes `auth::feed_token`).
- **`NOTICE_VERSION` stays 3.** The substantive-change rule (X-legal-docs, §2.33) applies to the Legal Notice, which users accept; this PR doesn't touch the notice, its copies or `crates/api/src/legal.rs`. The guidance isn't embedded in the binary.
- **rc.5's evidence carries over:** no code changes; the tagged binary differs only in its embedded commit hash.
