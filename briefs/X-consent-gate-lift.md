# X-consent-gate-lift: lift the consent gate for External, Restricted and Overlay sources whose approved dialog ships

**Runner brief, 2026-11-12 (UTC).** Opus, /security-review (consent and content safety). v0.6 work; the v0.5.2 cut is done (P-v052-cut), so main is free for it.
- **Branch:** `slice/X-consent-gate-lift` from fresh `origin/main`.
- **PR title:** `X-consent-gate-lift: Lift the consent gate for External, Restricted and Overlay once their approved text is accepted`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Read first
- `runner/slices.yaml`, the X-consent-gate-lift entry (its notes are the spec's history).
- `docs/owner-decisions.md` §2.49 (the owner's consent texts and classes) and §18.7 of `docs/architecture.md`.
- The decisions files of SRC-15b, SRC-15c (#564), X-consent-gate-polish and X-sources-api-spec in `docs/decisions/`.
- `crates/sources/src/consent.rs`, `crates/pipeline/src/control.rs`, `crates/pipeline/src/adult_enrich.rs`, `crates/pipeline/src/lib.rs`, and `crates/lodestar/tests/consent_gate.rs` (today's gate).

## What to build (this PR is r1; Account is r2)
1. **The gate lets a source through only when all of these hold:**
   - its class is External, Restricted or Overlay;
   - `consent_text_approved` is true for that class's text (the §2.49 texts SRC-15b shipped);
   - the user accepted exactly that `text_version`.
   Every other case stays refused exactly as today: Account, an unknown class, a stale accepted version, or no acceptance. Fail closed.
2. **Step 1, carried from X-sources-api-spec:** tighten `consent::need` to the declared credential spec. An Account source needs exactly its declared required credentials. Update the call sites in control.rs, adult_enrich.rs and lib.rs. This is a §2.19 flag; describe it in the decisions file for the runner to relay.
3. **Not in this PR:** Account and `text_version(Account)` (r2, after SRC-15c's dialog), and the admin-password re-ask when the gate opens for Account sources (SRC-15c's note). Write both up as r2's scope in the decisions file.
4. **Accepted text versions:** a bump in the shipped text re-closes the gate for that class until the user accepts the new version. Test it.
5. **Adult enrichment** (adult_enrich.rs) keeps its own gates (Show adult content, the runtime's caps). Lifting consent never bypasses them. Test that a consented Overlay source with Show adult off still offers nothing adult.

## Tests (add to the owned test files)
- A matrix over class × approved × accepted version × credentials, each case asserting allowed or refused, with refusal as the default.
- A version bump re-closes the gate; a restart keeps acceptance; the gate holds across a source re-registration.
- No fixed sleeps. No tokio `test-util` or `testing` feature on `lodestar`.

## Owns
`crates/pipeline/src/control.rs`, `crates/pipeline/tests/control.rs`, `crates/sources/src/consent.rs`, `crates/lodestar/src/ops/tests.rs`, `crates/lodestar/tests/consent_gate.rs`, `crates/pipeline/src/adult_enrich.rs`, `crates/pipeline/src/lib.rs`, and `docs/decisions/2026-11-12-X-consent-gate-lift.md`.

Ask the runner before touching any other file. A change to the consent text wording is never yours: that's the owner's (Legal Notice and consent wording).

## Stop and ask the runner
If anything would loosen a gate beyond "the approved text, accepted, for those three classes", stop and ask. The runner asks the owner.

## Done
- `cargo fmt --check`;
- `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`;
- `cargo test -p lodestar-pipeline -p lodestar-sources -p lodestar`;
- a /security-review of your own diff;
- send the runner `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green.

**Session window (owner, 2026-11-12):** sessions are cut back at 00:00Z on 2026-11-13. If the runner keeps this session, carry on. If not, push your work in progress, open the PR as a draft and report its state.
