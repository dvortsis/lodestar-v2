# Runner follow-ups after #562–#566

**Decided by:** the build runner, 2026-11-07T07:00Z. Plan bookkeeping only. These are runner calls under §2.33, flagged to the owner under §2.19 where they refine an owner decision. No owner decision is loosened.

## 1. New slices
| Slice | Why |
|---|---|
| **WK-9b** | The runner split WK-9 at about 1,700 lines (2026-11-07T04:45Z). WK-9 (#568) keeps results intake, fencing, content safety, late results, hints and RECENT_COMMITS. WK-9b takes counters into rollups with the worker dimension, grant backpressure and remote leases in `fetch_queue`. It also opens WK-8's sealed records before the rebuild, so it depends on WK-8 too. |
| **D-v051-release-docs** | The owner had another vendor's agent align the README and user guide with the published v0.5.1 (#565). The runner reviews and merges it on its normal rules. D-v051-main shrinks to what #565 leaves out. |
| **X-soak-screen-polish** | Two optional findings from #562's last review. One test assertion passes on the old code. `screen_slope` uses `** 2`, which overflows to `screen_error` instead of the per-counter `unusable` path. |

## 2. Requirements recorded in slice notes
- **WK-13:** late results are refused by the token's grant sequence, or by the coordinator's own grant time, against the purge. A worker-supplied `metadata_at` is never used (#568 review).
- **WK-10:** carries the hand-offs from WK-3, WK-5, WK-8 and WK-2. These cover `at_cap` logging, `stored` meaning `Have::len()`, and one message loop. They also cover the `serve.rs` placement hook, the wait after a worker restart for a fresh wire key, and WK-2's nits.
- **X-consent-gate-lift:** step 1 tightens `consent::need` to the declared credential spec. It was deferred from X-sources-api-spec, and the owner flag for that tightening moves with it. When the gate opens for Account sources, it asks for the admin password again (from SRC-15c).
- **WK-14:** the #560 delta review's items.

## 3. Owns approved during builds
Recorded in `runner/slices.yaml`:
- **WK-3:** `crates/cluster/src/proto.rs` (a manual `Debug` on `ToWorker`, so the key never reaches logs) and `fuzz/README.md` rows.
- **WK-8:** `crates/filter/src/seal.rs` (`seal_bytes`/`open_bytes` with AAD, with no format change) and `crates/pipeline/src/wire/results.rs` (a `SEALED` flag).
- **WK-9:** named functions in cluster, pipeline, store and lodestar files, listed in its notes.
- **SRC-15c:** its e2e specs, its decisions file and `web/.size-limit.mjs`.
- **X-soak-screen:** its own `.github/workflows/soak-screen.yml`. `ci.yml` is untouched.
- **X-sources-api-spec:** one assert each in three plugin tests.
- **WK-10:** `crates/lodestar/src/cluster/serve.rs`.
- **X-consent-gate-lift:** `crates/pipeline/src/adult_enrich.rs` and `crates/pipeline/src/lib.rs`.

## 4. Sizes accepted
- **WK-3 (#566):** about 2,900 lines.
- **WK-9 (#568):** about 2,600 lines.

Both are under the ~3k split line, and tests and fuzz targets make up a third to a half of each.

## 5. Flags for the owner (§2.19)
- **WK-3:** Q6's "swap in place above 64 MB" is read as the old and new filters together above 64 MiB. That is stricter than the wording, and it keeps a worker at 50M stored inside 256 MB. Each filter is capped at 128 MiB, which is about 107M stored.
- **WK-8 (at merge):** a held-back record travels as one sealed blob under a per-day wire key that the coordinator derives. The worker keeps that key in memory only. This is stricter than Q1's plan row, which sealed the spool with the worker's own SecretStore: a worker can't open its own spooled names.

## 6. Dashboard
`ext:experiment-host` holds for one booked run: cr-ab-comp-1, 2026-11-07T13:00Z to about 2026-11-08T14:01Z. It goes back to false when that run ends.
