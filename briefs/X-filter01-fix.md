# X-filter01-fix (runner brief, 2026-11-13)

The spec is the slices.yaml entry `X-filter01-fix` (Track Q, blocking, child-safety core).

**Held:** this finding's detail and the fix chosen for it are held under docs/security/v1/README.md §5. This brief doesn't hold them. The runner sends both to the session directly, by message, at launch.

**Owns (component level only, §5):** `crates/filter/**`, `crates/store/src/**`, `crates/search/src/**`, `crates/pipeline/src/**`, `crates/api/src/**`, `crates/api/tests/**`, `web/src/lib/features/filters/**`, `web/src/lib/api/openapi.json`, `web/src/lib/api/schema.d.ts`, and `docs/decisions/*-X-filter01-fix.md`. The runner gives any narrower scope by message only.

**Rules (security README §5):**
- No file on any branch may describe the finding or the chosen fix. That covers code comments, test names, commit messages, the PR title and body, the decisions file and runner-state.
- Name things by behaviour only where the code needs a name. When unsure, ask the runner.
- The decisions file records "FILTER-01 fixed (Track Q, detail held)" and whatever measured numbers the runner asks for by message. Nothing that explains the flaw.
- The PR title is "X-filter01-fix: Track Q fix", with a body by id only.

**Quality:**
- Opus builds it. /security-review, plus an unanchored second read.
- Every crate's tests stay green, and the filter budget and filter bench jobs stay green.
- Property tests where the change touches parsing of untrusted input.
- No fixed sleeps.

**Order:** X-filter03-fix and X-apifeat01-fix share these owns and wait for this slice to merge.

Steward skill. When CI is green, message the runner "ready <PR> <full sha>".
