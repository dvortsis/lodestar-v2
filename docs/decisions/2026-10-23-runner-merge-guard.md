# Runner merge guard and steward decisions (#106)

These record how the owner's decisions of 2026-10-23 (`docs/owner-decisions.md` §2.20 and §2.21) were put into practice.

- **`ci-ok` is the only required check.** It aggregates `changes`, `lint`, `openapi`, `test`, `importer` and `fuzz`, and counts skipped jobs as passing. Two reasons:
  - Since #90, path filtering skips jobs. A required check that never reports would stall a PR forever, and skipped matrix jobs keep their unexpanded names.
  - The size, search-latency and filter budgets stay outside it, because they can be red on `main` for hardware reasons (§2.18). The runner still gates on them as before.
- **The hook identifies the runner by a marker file** (`~/.lodestar-runner`), not by branch. The runner sometimes works on `slice/*` branches itself, and every session shares one GitHub account. The hook blocks with exit code 2. Any other failure lets the call through, so a broken hook can't stall the runner.
- **The hook checks the push command itself.** A chained `git fetch origin main && … git push origin slice/x` is allowed; a push whose refspec is `main` is refused.
- **Freshness rule instead of a merge queue.** Merge queues need an organization-owned repository. The runner brings `main` into a PR before merging only when the commits the PR lacks touch the same area: the Rust, web and workflow path classes from `.github/scripts/ci_changes.py`. Requiring every branch to be up to date would make every open PR re-run CI after each merge.
- **A red `main` comes first.** The runner reverts or fixes a breaking merge before merging anything else in the same area. A revert PR's review checks only that it is a clean revert.
- **Without Authenticode, the update engine trusts only the signed manifest** (architecture §12.2). The manifest's Ed25519 signature and SHA-256 and size pins are the trust anchor. Authenticode is checked only on artifacts that carry a signature. This narrows architecture §12.2's "Windows artifacts are also Authenticode-signed (M6), and that signature is checked too" until a certificate exists. When it does, B-update records whether a signed release makes later unsigned Windows artifacts refusable.
