# X-ci-advisories decisions

Spec:
- `runner/slices.yaml`, entry `X-ci-advisories` (Track Q, INFRA-09).
- The runner's brief of 2026-11-13.
- `docs/build-plan.md` (the "Supply chain and release" row).

## The advisory check
- **CI checks every crate in `Cargo.lock` against the RustSec advisory database.**
  - It's the new `advisories` job ("RustSec advisories") in `.github/workflows/ci.yml`: `cargo deny --locked check advisories`.
  - It's inside the required `ci-ok` gate, so a failure blocks a merge.
- **What fails it:** a vulnerability, an unsound crate, an unmaintained crate (`unmaintained = "all"`, transitive ones included) or a yanked version. The exception is an advisory that `deny.toml` ignores with a reason and a review date.
- **The tool is pinned:** cargo-deny 0.20.2, released 2026-07-09, built from crates.io with `cargo install --locked`. So its own dependencies are the ones its release locked, not whatever resolves on the day.
  - The brief allowed a hash-checked binary download instead. Building it costs a few minutes on a cold cache, and `Swatinem/rust-cache` keeps the built binary afterwards.
- **`--locked` on the check itself** refuses to run if `Cargo.lock` would have to change, so the check reads exactly the lockfile in the PR.
- **`deny.toml` holds only the advisory configuration.**
  - The graph covers the four shipped targets with all features, so a crate used only on Windows or macOS is checked too.
  - Licences stay with `.github/scripts/licenses.py`.

## When it runs (`ci_changes.py`)
- **On a pull request,** only when it touches `Cargo.lock`, `deny.toml`, `ci.yml` or `ci_changes.py` (`ADVISORIES`). A `Cargo.toml` change that moves a version also changes `Cargo.lock`.
- **On main, the nightly and manual runs, always.** An advisory published against a crate already in the lockfile turns the next nightly red, without any PR touching dependencies.
- **`fuzz/Cargo.lock` isn't checked.** The fuzz harness is its own workspace, built only in CI's fuzz job and never shipped.
- **Tests:** `test_ci_changes.py` covers the matching paths and near misses (`Advisories`). Its end-to-end tests check three things: a lockfile-only PR runs the check, a docs-only PR doesn't, and a non-PR event always does.

## No permission, secret or trigger changes
- The job inherits the workflow's `permissions: contents: read` and uses no secret.
- Checkout has `persist-credentials: false`.
- `pull_request_target` isn't used. Those decisions are the owner's.

## The advisories open in Cargo.lock (main at 667479cb)
cargo-deny reported three.

### RUSTSEC-2026-0385: `ctutils` 0.4.2: upgraded
- **The advisory:** non-constant-time mask generation and `u128` comparisons. It reaches Lodestar through `digest` → `hmac`, `argon2`, `sha1` and `pbkdf2`.
- **Fixed:** `cargo update -p ctutils` moves it to 0.4.8, the latest patch, MSRV 1.87. Nothing else in `Cargo.lock` moves.

### RUSTSEC-2026-0486: `rustix` 1.1.5: (pending the runner's decision)

### RUSTSEC-2026-0459: `libsqlite3-sys` 0.38.2: (pending the runner's decision)
