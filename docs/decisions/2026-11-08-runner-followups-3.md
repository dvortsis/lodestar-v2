# Runner follow-ups, 2026-11-08 (runner PR)

**Decided by:** the build runner, 2026-11-08. Runner calls under §2.26 and §2.33, flagged to the owner under §2.19. No owner decision is loosened.

## 1. The cloud-screen stage (owner-approved stage; the runner's rules under §2.26)
The owner approved adding a standing cloud-screen stage (2026-11-08) and left its design to the runner. A **fail-only screen** on the two cloud Linodes runs before an experiment-host run. Its rules:
- **Fail-only.** It stops a candidate for a clear failure only: a crash or restart, a memory or disk bound breached, stored rate at zero, or a named error class. It never ranks variants or shows a gain.
- **Rules before the run.** Thresholds, block shape and analysis are committed before the screen starts. A wrong rule means a re-run under corrected rules, recorded with its reason.
- **Shape.** One fresh start, then an ABBA block of 2–3 h resumed segments, the same on both Linodes, with images frozen by digest.
- **Never evidence.** Screen results are never gate evidence, never stitched into or pooled with experiment-host segments, and never cited as a gain. Gate evidence stays on the experiment host under CR-SPEC.
- **Never the owner's soak host,** and it never delays an experiment-host booking.

The cloud test plan carries the details.

## 2. The uTP lossy-link flake: the test fix only (X-flake-utp-lossy)
The recurring Windows failure of `fetches_over_a_lossy_link` (#196, #600, #617) had two causes:
- **The test's loss model** dropped packets independently, so several lost round trips in a row could take 30 s under the backoff uTP must apply.
- **The RTO backoff** in `lodestar-utp` kept doubling across timeouts separated by progress.

The test now drops at most one packet in a row each way, at the same 15% long-run rate. That matches the cap the state-machine simulation already uses. Over 400 seeds on the real stack, failures went from 5 to 0.

The proposed product change (reset the backoff on any progressing ACK) was **rejected** by a fresh Opus review. It reintroduces Karn's ambiguity: once a link's round trip rises above the RTO, one-packet messages are resent 2–3 times, and LEDBAT collapses on every spurious timeout. The timer fix moves to its own core slice, **X-utp-backoff**, with the review's options.

## 2a. The export WAL flake: the test fix only (X-flake-export-wal)
`the_wal_stays_bounded_beside_a_writer` (`crates/api/tests/export.rs`) failed on Windows with "only 2 checkpoints" (CMP-5's runs). The export streamed 400 rows at 400 a second, so it stayed open for only about 1 s, and the slowest runner sometimes fitted just 2 writer checkpoints inside it. The export now streams at 100 rows a second (about 4 s). The assertion is unchanged. No product code changes.

## 3. New slices
| Slice | Why |
|---|---|
| **X-pk-manifest-hardening** | #618's non-blocking review notes: duplicate versions, strict semver prerelease tags, the nightly job outside the `update-manifest` concurrency group, carried-statement types, a key-rotation note. |
| **PK-nightly-host** | Publishing nightly manifests needs a host and key arrangement the owner approves (`ext:owner-pk-nightly-host`). The proposal is in `docs/decisions/2026-11-08-PK-manifest.md`. |
| **X-wk10d-unknown** | #615's review notes, done after WK-10e. |
| **X-utp-backoff** | See §2. |
| **X-soak-screen-calibrate** | Replayed on r4 and r5, the advisory screen didn't separate pass from fail. It tripped on the passing r5 (mimalloc growth 37.79 against 20) and couldn't evaluate the failing r4 (`mmap_cache_excess` absent). It stays advisory. |
| **X-collect-checklist-docker** | See `docs/decisions/2026-11-08-cr-ab-comp-1-accepted.md`. |
| **X-cloud-infra** | The owner's one-off 5th slot: #625 replaces #623 without the live cloud IDs, and adds an infra CI job. |
| **X-cloud-infra-2** | Two items the X-cloud-infra session agreed are outside #625: an admin-address list for Main's Linode firewall, and a root-volume size parameter for the AWS Windows VM. |
| **P-cloud-test-plan** | The owner asked a session to review and fix the cloud test plan (#627). The plan files are its own. |
| **P-cloud-operator** | The owner answered Q1–Q6 on a hosted operator that runs the cloud fleet, with the runner managing it through draft-PR comments. The same session that wrote #627 writes the plan. |
| **CMP-5a, CMP-5b** | CMP-5 is split into three stacked PRs so each stays reviewable. |
| **WK-10a–e, WK-10d-store, WK-10d-polish** | The WK-10 parts, landed one PR at a time. All have merged, so `dashboard.yaml` marks WK-10 itself `done` (`state_override`), and WK-11 and WK-13 see it as satisfied. |
| **X-macos-pidfile-fifo** | main's macOS job went red after #613: `rustix` has no `mknodat` on macOS, so the test makes its FIFO with `mkfifo(1)`. PR CI runs macOS only with `full-ci`, and that label was added too late to reach #624's run. |

Merged flake and parity fixes now have entries too: X-natural-strict-parity, X-flake-email-bcc, X-flake-ops-pause, X-cmp4b-mounts, X-flake-query-token-style.

## 4. Dashboard
- `ext:experiment-host` goes back to `met: false`: cr-ab-comp-1 ended 2026-11-08T14:01Z. The next booking is CR-AB-ADM.
- The owner queue gains PK-nightly-host's decision (item 10), paired with item 3, the nightly key.
- A note explains the cloud screen in plain words.
