# X-ci-advisories decisions (#820)

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
- **Tests:** `test_ci_changes.py` covers the matching paths and near misses (`Advisories`). Its end-to-end tests check four things: a lockfile-only PR runs the check, a docs-only PR doesn't, a non-PR event always does, and so does a PR with the `full-ci` label.

## No permission, secret or trigger changes
- The job inherits the workflow's `permissions: contents: read` and uses no secret.
- Checkout has `persist-credentials: false`.
- `pull_request_target` isn't used. Those decisions are the owner's.

## The advisories open in Cargo.lock (main at 667479cb)
cargo-deny reported three.

### RUSTSEC-2026-0385: `ctutils` 0.4.2: upgraded
- **The advisory:** non-constant-time mask generation and `u128` comparisons. It reaches Lodestar through `digest` → `hmac`, `argon2`, `sha1` and `pbkdf2`.
- **Fixed:** `cargo update -p ctutils` moves it to 0.4.8, the latest patch, MSRV 1.87. Nothing else in `Cargo.lock` moves.

### RUSTSEC-2026-0486: `rustix` 1.1.5: upgraded (runner, 2026-11-13T06:13Z)
- **The advisory:** the xattr functions mark unwritten bytes as initialized when given a zero-length buffer. Lodestar never calls them: it uses `fs::OFlags`, `statvfs`, `process` and `thread`.
- **Fixed:** `crates/companions/Cargo.toml` pinned it exactly, at `=1.1.5`. It's now `=1.1.6`, and `Cargo.lock` moves only `rustix`.
- **The pin stays exact.** It comes from CMP-1 (`docs/decisions/2026-11-08-CMP-1.md`), which pins this crate's network and OS dependencies to the lockfile versions, as with `reqwest` `=0.13.5`. It names no audited surface of its own.
- **1.1.6 is the patch release** that carries the advisory's fix (its announcement commit). The point of moving is that soundness fix, not a new feature. `crates/lodestar/Cargo.toml` asks for `rustix` `1.1.5` as a caret requirement, which 1.1.6 meets.
- **The runner added** that one `crates/companions/Cargo.toml` line, and its `Cargo.lock` lines, to this slice's owns. The companions crate's tests and clippy pass on 1.1.6.

### RUSTSEC-2026-0459: `libsqlite3-sys` 0.38.2: ignored until 2026-12-13 (runner, 2026-11-13T06:13Z)
- **The advisory:** the bundled SQLite before 3.54.0 can return uninitialized page-cache memory when a process opens a database file together with a crafted `-wal` whose page size differs. 0.38.2 bundles SQLite 3.53.2.
- **No compatible fix:** 0.39 (SQLite 3.54) comes only with `rusqlite` 0.41. That's a semver-major bump in `crates/store` and `crates/sources-importers`, a core storage change that doesn't belong in a CI slice. The runner opens **X-rusqlite-041** for it (Opus, with the full test run).
- **Why the exposure is narrow:**
  - Lodestar's own database is written only by Lodestar.
  - The one database Lodestar opens that someone else may have written is a Magnetico import, and the importer refuses a source with a non-empty `-wal` or a `-journal` (`quiescent`, `crates/sources-importers/src/magnetico.rs`). The attack needs a WAL with content.
  - The Bitmagnet importer reads PostgreSQL, not SQLite.
- **Not closed:** this lowers the exposure; it doesn't remove it. The refusal is a stat before the open and before each batch, not atomic with what SQLite reads. Someone who can write to the source folder while the import runs could still place a crafted `-wal` between the check and the read. That's why the review date is short.
- **The date isn't enforced by a check.** Nothing fails on 2026-12-13 by itself, and cargo-deny only warns about an ignore that no longer matches. The runner tracks the date through X-rusqlite-041. A CI check that fails on a passed review date is a possible follow-up.
- **The ignore** is in `deny.toml` with its reason, "review by 2026-12-13; fix: X-rusqlite-041". That's 30 days, inside the brief's 90. When X-rusqlite-041 lands, the entry is deleted. If it's still there on 2026-12-13, whoever reviews it decides again.
