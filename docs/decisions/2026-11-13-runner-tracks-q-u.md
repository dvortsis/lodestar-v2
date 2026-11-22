# Runner records: Tracks Q and U, the WK-18 split, owner questions (2026-11-13, #813)

The runner's records PR for build-plan §0.2 r4.1 and §0.9 item 10. It turns the parts of the two reviews that hold no held detail into slices in `runner/slices.yaml`, and adds the owner questions in §0.3 (rows 13 to 21, plus one) to `runner/dashboard.yaml`. It follows `2026-11-13-master-plan-r4-reviews.md` ("For the runner", items 2 to 4).

Nothing here reverses or loosens an owner decision, weakens a security or content-safety decision, or changes an owner-set number. Security and content-safety findings are named by id, severity and surface only (owner §2.47, `docs/security/v1/README.md` §5).

## What the runner sliced, and why
- **Track Q, the code review's findings that hold no held detail:**
  - **X-lease-reconcile-every-session** (CLUSTER-01's follow-up after #779, with CLUSTER-02 and CLUSTER-05, plus a kill-and-restart chaos scenario inside the lease TTL). It's added to WK-G's deps, since the §19.8 gate can't pass while reconciliation waits for a spool removal.
  - **X-watch-stall-retry**: rung 5, beside UP-2 and X-watch-env. It owns different files from both, so it has no dep on them.
  - **X-utp-spurious-rto**: after X-utp-backoff (#788).
  - **X-e2e-orphan-specs** (TEST-01), **X-ci-advisories** (INFRA-09, with the three open advisories in `Cargo.lock` triaged in the same slice) and **X-fixed-sleep-asserts** (TEST-02, DHT-03, LOGSET-04, WEBCORE-05). They sit in a new workstream, `ws-review-fixes`, under the polish phase, because they belong to no feature area. X-ci-advisories changes no workflow permission or secret; those are the owner's.
- **Track U, the UI review (§2.66):** **X-results-drawer-adaptive** (I1 and I3 in one slice, with `2026-11-13-runner-ui-review-decisions.md`'s drawer rule), **X-insights-scroll-a11y** (B3), **X-offline-retry** (L1) and **X-session-draft-keep** (L2).
  - X-insights-scroll-a11y is added to X-ui-guards' deps. X-ui-guards' fixture-backed axe run covers `/insights` and would otherwise be red, and widening X-ui-guards beyond icon imports and dead classes would break its launch rule.
  - X-session-draft-keep waits for X-wizard-labels-signout, since both own `settings/form/**` and `auth/**`.
- **Follow-ups the runner had named:**
  - **ENR-13b**: the detail view offers the opened item to the identification runtime, API side only. IdentifySource's wiring in serve, the Legal Notice's Wikidata line and X-identify-route-switch land with X-consent-gate-lift-external, as ENR-14's decision (`docs/decisions/2026-11-12-ENR-14.md`) recorded. It carries `ext:owner-v07-q5`, as ENR-10 does.
  - **X-gluetun-templates-hostfile**: replaces X-gluetun-templates' plan A, a post-rules file the user saves, with Lodestar writing the file from its embedded copy.
  - **X-consent-gate-lift-r2**: Account's conditions, from r1's "For r2" list. The gate stays closed for Account.
  - **X-consent-gate-lift-account** and **X-consent-gate-lift-external**: the gate flips, each behind the owner's Legal Notice sentence. SRC-02 (important, sources) is settled before either flip, as build-plan §0.3's veto list says.
  - **X-worker-service-log**: worker progress lines get log events under the SCM.
- **Roadmap:** two new steps, Track U (row 4, column 5) and Track Q (row 4, column 6). Track Q feeds the M8 step and the 1.0 step; Track U feeds the M8 step. The v0.5.2 step now says INFRA-01 is checked before the tag.

## The WK-18 split (runner, 2026-11-12)
- **WK-18 merged as #797** with the compose and Unraid templates. Its `owns` drops `packaging/wix/join.wxs`, which was the wrong folder anyway (the WiX source is in `packaging/windows/`), and adds its decisions file.
- **WK-18s** (#801, Opus): `lodestar worker --join-only`, and the worker under the Windows SCM.
- **WK-18w** (#802, helper): the MSI's "Join an existing Lodestar as a worker" mode, after WK-18s. It keeps WK-18's design: a protected DACL on the token file, `Hidden=yes` on the token property, and the token passed by file, never in a command string. It stays on the v0.6 tag list.
- **WK-19's deps gain WK-18w,** so the workers chapter documents the Windows join it ships.

## New owner conditions
- **Added to `external` and `conditions`:**
  - `owner-filter01-choice`, `owner-apifeat01-choice`, `owner-filter03-choice` (rows 13 to 15);
  - `owner-infra01-image-gate` (row 16, before the v0.5.2 tag);
  - `owner-ui-part4` (row 17);
  - `owner-v06-tag-scope` (row 18, open since r4, which had no condition);
  - `owner-notice-torrent-cache` (row 21);
  - `owner-notice-account-wording` (the Account sentence, owner queue item 22).
- **Only the last two gate a slice today.** The held fix slices take the first three when the runner adds them. No slice depends on `owner-v06-tag-scope`: the tag follows P4 until it holds.
- **The owner queue** has items 13 to 22 for these, and item 23 lists the runner calls open to veto (§2.19): the restore-path rule (UPDATE-01, UPDATE-05, STORE-01), SRC-02 and TEST-05's drawer budget.

## G-v1-security's scope
- **It now covers Track Q's blocking and important findings,** by id only. None may be open at the release candidate unless held under `docs/security/v1/README.md` §5.
- Its `deps` don't change yet: the fix slices join them as they're sliced.

## Not sliced yet
- **The held findings' fix slices** (the content-safety and hostile-input groups, CMP-01 and Track Q's other blocking and important security or content-safety ids) aren't in `runner/slices.yaml` yet. The runner adds them later, named by id only, as each owner choice or placement settles.
- **The rest of Track Q and Track U steps 5 to 8** are triaged by the runner later: blocking and important before 1.0, the rest as Track B.

## Flags for the owner (§2.19)
- **X-gluetun-templates ships plan A,** a read-only post-rules file the user saves next to the template. X-gluetun-templates-hostfile replaces it with a file Lodestar writes.
- **Track U's split** into four slices: I1 with I3, B3 on its own (ahead of X-ui-guards), L1 and L2. This differs from the review's own grouping by cause.
