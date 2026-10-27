# Remaining import, updater and documentation findings

The owner authorized all three proposed additional items on October 26. Work is isolated on `work/soak-followups`, above `40509d36`, in `target/worktrees/soak-followups`. The original [follow-up record](2026-10-26-soak-followups.md) describes the prerequisite stack and soak gate.

## Prepared changes

| Item | Change | Status |
|---|---|---|
| Verified partial imports | Replace authoritative name/encoding fields and upsert the configured raw archive in the existing transaction | Source and regression tests prepared; Rust execution pending |
| Updater persistence | Hold the updater state lock through persistence so an older save cannot overwrite a later pause | Source and coordinated concurrency/restart regression prepared; Unix execution pending |
| Documentation lint | Add format/audience/source metadata, correct heading case, explain the snapshot filename argument, replace unexplained precedence terminology | All three material files pass lint without warnings |

Decision records: [import upgrades](../decisions/2026-10-26-soak-import-upgrade.md) and [updater ordering](../decisions/2026-10-26-soak-updater-order.md). These address the three remaining findings in the October 24 backend gap review. They do not repair already-complete historical import rows, add a retry producer, or change updater disk-error behavior.

## Executed checks

- Rust formatting/parser check and diff whitespace check passed. No Rust build, type check or regression executable was run.
- An in-memory SQLite check executed V1–V12 and the actual changed upgrade SQL: all 12 update bindings, UTF-8 raw/encoding clearing, legacy-name replacement, archive replacement and transaction rollback passed. This checks SQL behavior, not the Rust call chain.
- A fresh read-only security review of the changes above `40509d36` found no actionable issues in the transaction, verification gate, archive handling, updater locking or regression logic. It executed no builds or tests and does not replace final review after full validation.
- `material-composer` preflight and catalog drift check passed. The existing operations/Linux/Docker Markdown references use the User Guide format; no new rendered document was produced. `lint_material` passes for all three files with 0 failures and no warnings, resolving the prior combined 27 failures and two warnings.
- User-guide sync check is up to date. **LINT: 1,156 checks, 0 failed, 0 warnings.** No generated guide content or screenshots changed.
- All three Unraid template validators passed. No container or runtime fixture was started.
- BRAND: Lodestar, existing `brand/brand.json`. Visual rendering was not run: this cleanup changes metadata, heading capitalization and prose in existing Markdown; no layouts or figures were added.

The collector still reported baseline `running`, HTTP 200 and valid metrics at its `2026-10-26T21:49:47-06:00` sample. The running binary, collector, data, configuration and stopped Docker crawler were not changed.

## Post-soak acceptance

The actual collector completion gate and isolated fixture rules in the [validation sequence](2026-10-26-soak-followups.md) still apply. From this worktree, use its own `CARGO_TARGET_DIR` and first run:

```powershell
$env:CARGO_TARGET_DIR = Join-Path (Get-Location) 'target/cargo'
cargo test --locked --offline -p lodestar-store --test store
cargo test --locked --offline -p lodestar-search -p lodestar-pipeline
```

On Linux, from an isolated checkout, additionally run:

```sh
export CARGO_TARGET_DIR="$PWD/target/cargo"
cargo test --locked --offline -p lodestar update::engine::tests::
```

Require a nonzero selected-test count that includes `a_pause_cannot_be_overtaken_by_an_older_updater_save`; Windows excludes this engine. Then execute full workspace formatting, strict Clippy and tests, plus the inherited frontend/package/scale gates and final review before publishing. No push, merge, deployment, Docker restart or change to the running soak is included here.
