# X-cmp0-polish: Companion catalogue follow-ups from the CMP-0 review
**Model:** Opus. The catalogue checks untrusted release URLs and names, so treat this as security-adjacent: the PR gets a `/security-review` pass. A fresh Opus reviewer follows. Runner brief, 2026-11-07T05:10Z. It takes the slot SRC-15c freed (#564 merged), within owner §2.42's four slots.

## Spec
- **The plan:** `docs/plan/2026-11-06-v06-companions.md`, CMP-0 in §4.3.
- **Decisions:** `docs/decisions/2026-11-06-CMP-0.md` (CMP-0a, then CMP-0).
- **The #548 review notes**, as listed in `runner/slices.yaml`'s `X-cmp0-polish` entry. They are the whole scope.
- **Owns:**
  - `crates/companions/src/{catalogue.rs,inputs.rs,adapter.rs,lib.rs}`;
  - `crates/companions/tests/catalogue.rs`.

  CMP-13 shares the test file later, so this slice lands first.

## Do
Each item gets a test that fails without the fix:
1. **FlareSolverr is matched exactly,** not by `contains`. A name that merely contains it, for example `my-flaresolverr-fork`, isn't treated as FlareSolverr.
2. **`%3b`/`%3B` is refused like a literal `;`** wherever a literal semicolon is refused (percent-decoded before the check, or both forms checked). Also check the other percent-encoded forms of every character refused there, and say in the PR which ones you covered.
3. **Templates are tied by id,** not only by role. A template whose role matches but whose id doesn't is refused.
4. **Release folders** must have the `/<owner>/<project>/releases/download/` shape. Anything else is refused, for example `/<owner>/<project>/archive/` or extra path segments before `releases`.
5. **The `compile_fail` doctests name their error codes** (E0624, E0616), so they can't pass for the wrong reason.
6. **A `ConfigPlan` constructor for `tests/` fakes (CMP-5g).** It must not widen the public API for normal builds.
   - Use a `#[doc(hidden)]` constructor, or a feature on the **companions** crate only.
   - **Never** add a `testing` or tokio `test-util` feature to the `lodestar` package (a CI guard refuses that).
   - Say which approach you chose, and why.

## Done when
- Every item above has its test.
- **No fixed sleeps.**
- **Checks:** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`.
- **Decisions:** a short section added to `docs/decisions/2026-11-06-CMP-0.md`, or a new `docs/decisions/2026-11-07-X-cmp0-polish.md`.
- **Size:** about 300 lines. Ask the runner before editing outside your owns.
- **Process:** follow `.claude/skills/steward/SKILL.md`, push once per round, and never merge. Title the PR `X-cmp0-polish: Companion catalogue follow-ups`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA once CI is green.
