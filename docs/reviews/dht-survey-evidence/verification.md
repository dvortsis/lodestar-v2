# Scheduler proposal verification — 2026-11-16

All Rust compilation and tests ran in temporary crates. No production Rust file was edited. No crawler or cloud operation was started. These tests use mock clocks and local temporary SQLite files.

Toolchain: Rust/Cargo 1.99.1, Windows. Dependencies are locked in [Cargo.lock](Cargo.lock): rusqlite 0.40.2 (bundled SQLite), Tokio 1.53.3, tracing 0.1.44, tempfile 3.27.0. The proposal did not supply an exact Cargo manifest, so these results are for the explicitly disclosed harness, not an unspecified original project.

Input: the user's second attachment, SHA-256 `AFEB690C3765BF07E67A1485D96527CB2BF4BBDFEE60429EA0993F0CF7AC0E68`. Its first Rust fence contains 2,227 module lines. Attachment line = module line +23. The unchanged extracted module is now preserved as [original-scheduler.rs.txt](original-scheduler.rs.txt), UTF-8 with LF and no added trailing newline, SHA-256 `044754DD300000D7E1D612514E8C7D4153382C21F91B3772B9F590B6435D824C`. It is a review fixture, not a production module. The harness normalizes checkout line endings before verifying that digest.

1. **Unmodified extracted proposal: fails compilation.** Three E0499 errors, at module lines 1278, 1285, 1530. [Compiler output](01-original-build.txt)
2. **Mechanical borrow repairs only:** capture record support/novelty before changing other guard fields; move sequence/counter assignments after the last record use. This changes borrow scope, not clock, scheduling or SQLite policy. The exact transformations are in [verify-proposal.ps1](verify-proposal.ps1).
3. **Ten supplied tests: all pass after those repairs.** Harness places the proposal in `pub mod scheduler`, matching the subprocess test's exact test-name filter. [Test output](02-supplied-tests.txt)
4. **Clippy with warnings denied: passes after those repairs, before adding review tests.** [Output](03-clippy-after-borrow-repair.txt)
5. **Formatting check: fails.** An excerpt showing original proposal formatting is preserved; it is not a production formatting check. [Excerpt](04-format-excerpt.txt)
6. **Two added invariant tests: both fail, as expected for the defects.** Forward jump followed by WAL commit and restart loses a six-hour restriction after ten monotonic seconds. A second owner loaded before the first owner's reservations can subsequently overwrite a live cooldown. The latter requires multiple owners and demonstrates the need to enforce the documented single-writer assumption. [Regression tests](review-regressions.rs), [output](05-added-regressions.txt)
7. **Five follow-up invariant tests: all five fail.** With real SQLite `BEFORE UPDATE` / `BEFORE INSERT` abort triggers: (1) a failed completion preserves only the reservation-time deadline and permits redispatch ten seconds before the received six-hour interval expires (no restart or clock jump needed); (2) a scheduler accepts and disarms a matching endpoint/sequence permit belonging to another scheduler with a different database (distinct from same-file ownership and requires incorrect integration); (3) once `max_cooldown_entries` fills with productive endpoints, `reclaim_expired_locked` never inspects `hot_heap` and permanently wedges `consider_candidate` with `skipped_ledger_full`; (4) failing an `ExplicitUnsupported` completion commit on a previously productive node preserves stale `Supported` novelty and re-inserts the unsupported node into `hot_heap` ahead of `ready_queue`; and (5) failing an initial WAL reservation leaves `next_allowed_epoch_sec` poisoned in RAM for six hours even though SQLite rolled back and no UDP packet was sent. [Follow-up tests](followup-regressions.rs), [output](06-followup-regressions.txt)

Reproduction with PowerShell 7, the recorded Rust/Cargo 1.99.1 and cached dependencies; no attachment is required:

```powershell
pwsh -File docs/reviews/dht-survey-evidence/verify-proposal.ps1
```

The optional `-ProposalAttachment <path>` uses the original attachment instead, checking both its full-file hash and the extracted-module hash, and `-SyncEvidence` refreshes the scrubbed `01..06-*.txt` logs in `docs/reviews/dht-survey-evidence/`. The script creates a new temporary directory and preserves its scrubbed logs and extracted code (replacing personal filesystem prefixes with `<TEMP_VERIFY_DIR>`, `<REPO_ROOT>`, and `<USER_HOME>`). It never deletes an existing directory. Offline mode requires the locked dependencies to be cached. It rejects an unexpected compiler/Cargo version and verifies the ten-test count and all seven regression failures. A successful reproduction-script exit means the expected build failure and all seven counterexamples reproduced, not that the proposal passes verification. This is a historical counterexample harness, not an acceptance suite for a corrected scheduler; a future implementation needs positive safety and liveness gates.

The self-contained harness was rerun successfully after adding the follow-up tests. PowerShell parsing and local file-link checks passed. The unchanged source fixture deliberately preserves three trailing spaces inside SQL string lines (856, 1673, 1685); `git diff --check` reports those original bytes. The review changes excluding that fixture pass the whitespace check. Formatting the fixture would change the evidence hash.

Public source snapshots inspected from GitHub codeload archives (no upstream code was executed):

- bitmagnet-io/bitmagnet `e31b30d7351c269dc801876f92ebc59d31fee545`, main.
- boramalper/magnetico `5546a941e4f3a2fa6a2bc8bff8876aa69780034b`, master.
- the8472/mldht `902906f51a911f29c71fd7f0b65519aadaa9013d`, master.
- shiyanhui/dht `5a20f319926383187ead6e7eb2509b08599a9ee9`, master.
- 0xcaff/dht-crawler `1fbf4154a1f310176f0a1e236c2905b9ef505c56`, master.
- btdig/dhtcrawler2 `82f14c458c808bc4ef2f1f90486cfcc88830dce6`, src branch.

The survey links file paths and line ranges at these revisions. No benchmark was manufactured from source constants, README packet rates, discovery counts or unmatched tests. Alexandria was removed after the user's clarification.
