# Runner follow-ups after #550–#555 (#557)

**Decided by:** the build runner, 2026-11-07T00:30Z. Plan bookkeeping and one correction of numbers; no owner decision changes.

## 1. ENR-9b's cost numbers, corrected
This supersedes the cost figures in `docs/decisions/2026-11-06-ENR-9b.md`. The fresh Opus delta review of #551 measured them again:
- **Ingest, worst case:** about **150 MB** of judged text per step, not 40–70 MB. That's `batch_max` 256 × about 0.6 MB for a full details section, plus 4 KB names, and roughly twice that for hybrids.
- **Names:**
  - Verified names are capped at 4,096 raw bytes, but a decoded name can reach about 12 KiB (legacy encodings).
  - The worst refile row is about 1.3 MB.
- **Refile:** real work can reach about twice `REFILE_JUDGED_BYTES`, because swarm hashes are counted too.
- **Not counted:** adult-section pairs. This was already true before ENR-9b.

None of this is a defect in ENR-9b: no plugin sends details sections yet. The budget belongs to the first slices that do, so `runner/slices.yaml` now asks **ENR-10** for a judged-bytes budget at ingest, before any plugin sends sections, with an Items-phase budget test. **UN-1** carries the same requirement.

## 2. A new slice: X-fetch-meta-reserve
X-fetch-pool-bytes (#554) caps the bytes held in finished fetches. A peer's announced `metadata_size` can still be in flight on every socket at once (`max_sockets` × `max_metadata`).

X-fetch-meta-reserve reserves the announced size against the same byte budget before requesting pieces, and adds a shutdown drain deadline. `metadata_size` comes from the peer and isn't trusted, so the slice gets a `/security-review`. Workers need it, because WK-10 sizes a worker's memory budget from it, so WK-10 now depends on it. It launched at 2026-11-06T23:31Z.

## 3. `owns` corrections the runner approved during builds
Recorded in `runner/slices.yaml`:

| Slice | Added | Runner's call |
|---|---|---|
| SRC-15b | `web/e2e/settings-sources.keyboard.test.ts` | its keyboard test |
| X-consent-gate-polish | `crates/pipeline/src/lib.rs` | 2026-11-06T22:05Z: `prepare`'s condition loop moved into `control::switch_off_unapproved` |
| X-consent-gate-polish | `docs/releases/v0.5.2.md` | 2026-11-06T22:19Z |
| X-fetch-pool-bytes | `crates/lodestar/src/metrics.rs`, `docs/metrics.md` | 2026-11-06T21:52Z |
| WK-2 | `crates/lodestar/src/cli/mod.rs`, `crates/lodestar/src/main.rs` | 2026-11-06T23:14Z and 23:47Z: one variant, the dispatch arm, privdrop passing `worker --data` through under the same guard, and removing `LODESTAR_TOKEN` from the environment |
| X-fetch-meta-reserve | `crates/peer-wire/src/lib.rs` | 2026-11-06T23:36Z: one re-export |

**WK-2's notes also record a rule.** No tokio `test-util` and no `testing` features on the `lodestar` package. The CI guard refuses test-only features in the binary's tree, and WK-2 injects its clock instead.

## 4. Notes for later slices
- **X-sources-api-spec:** the `patchSource` description in `crates/api/src/ops.rs` still says an Account source gets 409 `credentials_required`, and describes the consent flow as live. Fix the wording (found in #553).
- **P-v052-cut:** `docs/releases/v0.5.2.md` is about 500 words by material-composer's count, against its 450-word cap. Trim it at the cut.

## 5. The v0.5.2 feature manifest names the gate lift
In `docs/plan/2026-11-06-v052-feature-manifest.md`, "until SRC-15b" now reads "until X-consent-gate-lift" in three places (the table row, §3.4 and the release-note summary). X-consent-gate-052's title changes to match.

These follow `docs/decisions/2026-11-06-runner-sources-split.md` (#550): the consent gate stays closed in v0.5.2, and X-consent-gate-lift opens it after the cut.
