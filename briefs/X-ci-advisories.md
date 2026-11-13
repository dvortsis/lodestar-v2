# X-ci-advisories (runner brief, 2026-11-13)

The spec is the slices.yaml entry `X-ci-advisories` (Track Q, INFRA-09).

**Owns:**
- `.github/workflows/ci.yml`;
- `.github/scripts/ci_changes.py`;
- `deny.toml`;
- `Cargo.lock`, for the triage only;
- `docs/decisions/*-X-ci-advisories.md`.

**Do:**
1. **Add a RustSec advisory check** inside the required `ci-ok` gate: cargo-deny `advisories`, or cargo-audit, pinned by version and installed with a hash check or `--locked`.
   - It runs on PRs that touch Cargo.lock or `deny.toml`, and on main.
   - Wire `ci_changes.py` so the job runs on those paths, plus a test of the filter.
2. **Triage the open advisories** in Cargo.lock: upgrade where a compatible fixed version exists. Otherwise add an ignore entry with the advisory id, a one-line reason, why it doesn't reach Lodestar, and a review date at most 90 days out.
3. **Record each one** in the decisions file, by advisory id and crate.

**Hard limits:**
- no workflow `permissions:` changes, no new secrets, no `pull_request_target` (those are the owner's);
- Cargo.lock changes only for the triage.

Bring main in before review: other PRs may touch Cargo.lock. Opus reviews it with /security-review. Steward skill; PR title "X-ci-advisories: …".
