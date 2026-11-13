# Self-update system review
Reviewed at 55e155a9.

## Coverage
- Read in full (non-test source):
  - `crates/update/src/{lib,verify,minisign,doc,chain,trust,keys}.rs`, `crates/update/README.md`, the head of `src/testing.rs`
  - `crates/lodestar/src/update/{mod,layout,overlay,supervisor,pending,control,journal,probation,testing,engine,install,download,check,trust_file,policy,window,history,rollback_cache}.rs`
  - `crates/lodestar/src/update/windows/{helper,msi,rollback}.rs`
  - `crates/lodestar/src/cli/rollback.rs`, `crates/api/src/updates.rs`
  - the updater glue in `crates/lodestar/src/main.rs` (privilege drop order, supervisor start, the exit-code hand-off)
- Skimmed: `windows/check.rs` (lines 80-240, 975-1125, 1290-1545 and 1700-1710 read; the rest outlined), `windows/portable.rs` (lines 1-470 read; the start/rollback half only grepped), `docs/architecture.md` §12.2, the update decisions files I cite, `crates/store/src/snapshots.rs` (`purge_for_restore`, `apply_source_purges` and the module doc only), `crates/api/src/auth/guard.rs` (who reaches `/updates`).
- Not read: every `mod tests` (about 17k lines) and `crates/lodestar/tests/supervisor.rs`, so I make no claim about test quality or fixed sleeps; `windows/history.rs`, `unavailable.rs`, `log_events.rs`, `cli/update_helper.rs`, `crates/update/src/tests/**`, the fuzz target. Out of area and not read: `packaging/windows/**` (WiX custom actions, the task's registration, `secure-datadir.ps1`), `windows/acl.rs`, `.github/workflows/release.yml` (signing and manifest publication). The Windows chain of trust rests on those; see Area-level concerns.

## Findings

### UPDATE-01 An update rollback restores the pre-migration snapshot without the deletion journal or the content-safety holds
- Severity: blocking
- Kind: security
- Confidence: confirmed
- Where: `crates/lodestar/src/update/journal.rs:408-435` (`choose_snapshot` applies `apply_source_purges` only), `crates/lodestar/src/update/journal.rs:438-495` (`restore` is a bare rename); contrast `crates/lodestar/src/cli/rollback.rs:375-386` and `:396-410` (`purge_for_restore`); `crates/store/src/snapshots.rs:1188-1193` (the doc says this path "renames a pre-migration snapshot over the store without `purge_for_restore`").
- What: `docs/decisions/2026-10-30-F-rollback-journal.md` ("A rollback never brings back a deleted torrent", BB-1, owner decisions §2.16/§2.17) and `docs/decisions/2026-10-30-F-rollback-holds.md` (CS-1 hashes, tombstones, quarantine, blocklist subscriptions and core switches recorded after the snapshot are carried into the restored store; flagged to the owner as content safety) are implemented only for `lodestar rollback <snapshot>`. The three update-rollback entry points share `journal.rs`: the automatic rollback after a failed probation, in-app Roll back (`POST /updates/rollback`) and `lodestar rollback --update`. They put the snapshot in place with only source purges scrubbed. Nothing merges the live store's `snapshot_deletions`, `cs_hashes`, `cs_sigs`, `tombstones`, `quarantine`, subscriptions or core switches.
- Failure scenario: 0.6.0 migrates the store and passes probation. Over the next weeks the user reports hashes (CS-1), a floor list tombstones others, and a blocklist subscription is added. The user clicks Roll back (offered while the snapshot is kept, up to 90 days: `engine.rs:1111-1142`). The restored store has none of those holds, so the crawler can fetch and index again what the safety core blocked. Torrents deleted since the last hourly snapshot purge come back too. This is the state F-rollback-holds describes as the defect it fixed for the CLI.
- Suggested fix: in journal step 2/4, run the same `purge_for_restore` (deletions, holds, lists, source purges) on the chosen snapshot before it is hashed and renamed, and refuse the rollback when it can't be applied. Add the journal-path twin of the CLI's hold tests.
- Tracked?: not found. `docs/decisions/2026-11-02-SRC-14b-2.md` ("From the runner's review") lists this path and added only the source-purge scrub; no slice in `runner/slices.yaml` covers deletions or holds for the update rollback.

### UPDATE-02 Stored revocations are never consulted, and the "installed under a revoked key" notice can't fire for a rotation learnt from `trust.json`
- Severity: important
- Kind: gap
- Confidence: confirmed
- Where: `crates/update/src/trust.rs:273-277` (`TrustState::revokes`, no caller in the workspace), `crates/update/src/verify.rs:100-189` (`check` never reads `state.revoked`), `crates/update/src/verify.rs:366-371` (`revoked_later` from embedded plus the overlay's own statements), `crates/lodestar/src/update/supervisor.rs:561-565`, `crates/lodestar/src/update/engine.rs:1014`.
- What: `docs/decisions/2026-10-23-B-update.md` ("the updater also checks `TrustState::revokes` against `trust.json`"), the crate README ("honours every revocation a newer build recorded") and architecture §12.2 ("An overlay installed under a key revoked later, as learnt from `trust.json` ... starts with a notice") all rely on a check that doesn't exist. `revoked` is parsed, capped, unioned and written, and nothing reads it. The only source of the `revoked_key` notice is the supervisor's `revoked_later`, computed from the overlay's own manifest. A manifest that `check` accepted can't carry a statement that revokes its own signer (`verify.rs:123-125`), and the supervisor never reads `trust.json`, so in practice the notice is unreachable.
- Failure scenario: the release key leaks and the owner rotates it. An installation running an overlay signed by the old key accepts the rotation at its next check. `trust.json` records the old key as revoked, but `/updates` reports `running.revoked_key: false` and shows no "Installed under a revoked signing key. Install the current release." notice, until a newer image arrives.
- Suggested fix: in the engine, compare the running overlay's signer (re-derive it from `<bin>/<running>/manifest.json`) with the chain loaded from `trust.json` and with `state.revokes`, and raise `revoked_key` there. Either enforce `revoked` in `check` or drop the field and the README claim.
- Tracked?: not found.

### UPDATE-03 An update rollback that restores the store keeps the shared search index, which is then ahead of the store
- Severity: important
- Kind: correctness
- Confidence: likely
- Where: `crates/lodestar/src/update/journal.rs:136-142` and `:377-382` (only `index/` entries created since `pending.index_before` are removed; none at all when the rollback is asked for outside probation, `supervisor.rs:879`); contrast `crates/lodestar/src/cli/rollback.rs:11-13` and `:347-357` (the whole index is moved aside because "ids reused after the restore would show other torrents' data"); Windows removes nothing (`windows/rollback.rs`, `windows/portable.rs`).
- What: when the failed or rolled-back version migrated the store but kept the same index folder (`index/v{schema}-t{tokenizer}` unchanged), it has indexed every torrent stored since the snapshot into the folder the previous version also uses. After the snapshot is renamed over the store, the index holds documents and a watermark for ids the store doesn't have. SQLite hands those ids out again to different torrents.
- Failure scenario: 0.6.0 bumps the store schema only. A week later the user rolls back. The previous version opens an index whose watermark is thousands of ids ahead. Newly stored torrents with those ids are never indexed, and searches return the rolled-back era's titles under ids that now belong to other torrents.
- Not confirmed: whether `lodestar-search` detects an index ahead of the store at start and rebuilds. I found no such check in `crates/search/src/{index,live,rebuild}.rs` by grep, and the CLI's own comment says the hazard is real.
- Suggested fix: when the journal carries a snapshot (the store is restored), move the whole index aside or delete it in step 5, as the CLI does. Do the same on both Windows paths.
- Tracked?: partly. Windows "Removing a failed version's index folders" is a listed follow-up in `docs/decisions/2026-11-09-PK-win-upd-3.md` and `2026-11-11-PK-win-portable-upd.md`, described as a disk cost only. The same-folder case isn't mentioned for either platform and no slice was found.

### UPDATE-04 The supervisor's attempt count is lost when `pending` can't be written, so a failing version restarts forever
- Severity: important
- Kind: correctness
- Confidence: confirmed
- Where: `crates/lodestar/src/update/supervisor.rs:630-643` (`charge`), `:785-795` (`settle_time`), `:903-906` (`after`), `:647-653` (`out_of_attempts`).
- What: `charge` re-reads `pending` from disk on every loop iteration, adds one in memory and tolerates a failed write ("The attempt still counts in memory for this run"). The next iteration reads the disk again, so nothing carries over inside the same supervisor process. `settle_time`'s `used_ms` write fails the same way, so the cumulative deadline never accumulates either.
- Failure scenario: the data volume fills (or is remounted read-only) while a new version is in probation. The child fails to start, `after` sees `attempts == 1 < 3` and returns `Next::Again`, every time. The supervisor respawns the child in a tight loop with no backoff (each pass copies and hashes the binary), never rolls back, never halts, and never exits to let Docker's or systemd's restart policy take over.
- Suggested fix: keep the charged attempts and the used time in the `Supervisor` and take the larger of memory and disk; or treat a `pending` that can't be written before a start as "don't start" and exit non-zero, as §12.2 already requires for the switch.
- Tracked?: not found.

### UPDATE-05 Both Windows rollbacks restore a snapshot with no scrub at all, which breaks SRC-14b's "every restore entry point" condition
- Severity: important
- Kind: security
- Confidence: confirmed
- Where: `crates/lodestar/src/update/windows/rollback.rs:1093-1167` (`choose`, `restore`), `crates/lodestar/src/update/windows/portable.rs:858-882` onwards (`choose`, `restore_store`; grep found no purge call in the file).
- What: `docs/decisions/2026-11-02-SRC-14b-2.md` made "Every restore entry point applies recorded purges" a blocking review condition and stated "No other path renames a snapshot over the store". PK-win-upd-3 and PK-win-portable-upd then added two more paths that rename a pre-migration snapshot over `lodestar.db` by name alone. A purged source's credentials and consent come back with the snapshot, and the purge's record is lost with the store that held it. UPDATE-01's deletions and holds apply here too.
- Failure scenario: today the window is narrow, because both Windows rollbacks are automatic and run within minutes of the install (60 s for the MSI, up to three starts for the portable build): a source purged, a torrent deleted or a hash reported in that window comes back or is forgotten when probation fails. PR 795 (manual Roll back) reuses this restore and widens the window to the snapshot's 90 days, which is the UPDATE-01 scenario on Windows with source credentials restored as well.
- Suggested fix: SYSTEM must not open the store, so have the restored (old) version apply the scrub at its first start, before serving, from a small record the newer version keeps outside the store; or have the service prepare a scrubbed copy before the helper stops it.
- Tracked?: partly. Listed as a follow-up ("The source-purge scrub before a restore") in `docs/decisions/2026-11-09-PK-win-upd-3.md`, which understates it ("may hold torrents a source purge had removed, until the next purge runs"). No slice found; the open PR 795 (manual Roll back) would add a third caller of the same restore.

### UPDATE-06 Windows inventory: what still answers `not_built_yet` or is silently absent
- Severity: important
- Kind: gap
- Confidence: confirmed
- Where and what:
  - `windows/check.rs:1706-1708`: `POST /updates/rollback` always answers `not_built_yet` (PR 795).
  - `windows/check.rs:1078-1086` and `:1121-1160`: the policy is always given `Reason::NotBuiltYet`, so nothing ever installs automatically on Windows. "Install automatically" and "Always install security fixes" are accepted and shown but do nothing; only Install now installs.
  - `windows/check.rs:1315-1317`: `when: window` answers `not_built_yet`.
  - `cli/rollback.rs:127-130`: `lodestar rollback --update` refuses on Windows.
  - `windows/check.rs:1103` and `:1115`: the portable or MSI install without an installer handle answers `not_built_yet`.
  - `update/mod.rs:168-172`: any non-Unix, non-Windows target answers `windows_helper`.
  - Windows probation is "service Running for 60 s", one attempt, with no self-test; no `history.json` from the helper; no index cleanup (all recorded in `docs/decisions/2026-11-09-PK-win-upd-3.md`).
  - Nightly has no key on any platform (`crates/update/src/keys.rs:17`): the channel is selectable and every check answers `no_nightly_key`.
  - Not built anywhere in this area: worker updates over the cluster channel (§12.2, v0.6). I didn't check the "Notify me" before/after/failure notifications.
- Failure scenario: a Windows user chooses Install automatically with security fixes on, and a security release is never installed without a click.
- Suggested fix: finish the paths, or have Settings refuse or grey out the options the platform can't honour (the API already exposes `capability.reason`).
- Tracked?: Roll back is PR 795 / X-win-upd-manual-rollback; the probation, history and index gaps are follow-ups in the PK-win-upd-3 decisions file. I found no slice for automatic or windowed installs on Windows.

### UPDATE-07 A `trust.json` that can't be saved is silently ignored after a check
- Severity: polish
- Kind: security
- Confidence: confirmed
- Where: `crates/lodestar/src/update/engine.rs:816-818`, `crates/lodestar/src/update/windows/check.rs:1001-1003`.
- What: `let _ = trust_file::save(...)`. A high-water mark, freshness mark or accepted key rotation that wasn't persisted is neither logged nor raised as a notice (unlike `check_not_saved` for `updater.json`). The next process starts from the older marks. The Windows helper does fail closed here (`helper.rs:605-606`).
- Failure scenario: a full disk during a check that carried a rotation. After a restart the installation accepts manifests the rotation should have shut out, until a check succeeds in writing.
- Suggested fix: log the failure and add a notice; refuse installs in that process when the state that verified the manifest couldn't be written.
- Tracked?: not found.

### UPDATE-08 After the documented recovery-key remedy, an existing `trust.json` with any rotation in it reads as damaged for good
- Severity: polish
- Kind: gap
- Confidence: likely
- Where: `crates/update/src/trust.rs:118-157` (`load` re-verifies every stored statement under the embedded recovery key), `crates/lodestar/src/update/trust_file.rs:113-115` (a damaged file is never overwritten).
- What: §12.2's remedy for a lost or compromised recovery key is "a new build with a new embedded key, which users install by hand once". That build can't verify the statements the old recovery key signed, so `trust.json` is `trust_damaged`, which blocks every install and is never rewritten. The file is fine when it holds no statements.
- Failure scenario: one release-key rotation has happened; later the recovery key is replaced. Every upgraded installation shows `trust_damaged` and can't self-update until someone deletes `<data>/bin/trust.json` by hand. I found no instruction for that; I didn't read `docs/operations.md` in full.
- Suggested fix: document the manual step with the remedy, or version the trust file by recovery-key id so a new root starts a fresh state.
- Tracked?: not found (the recovery key is a recorded accepted risk; this consequence isn't).

### UPDATE-09 File I/O on async workers in the Unix engine
- Severity: polish
- Kind: resource
- Confidence: confirmed
- Where: `crates/lodestar/src/update/engine.rs:1211` and `:1323` (`may_install` reads `skipped`, calls `statvfs` and `access`), `:435` and `:463` (`until_install` runs the same on every pass of the schedule loop), `:1336` (`room_for`), `:1359`, `:1370`, `:1379` (`remove_overlay` deletes a folder inline).
- What: the module's own rule ("Each reads files under `<data>/bin` ... off the async workers") is kept for `status`, `skip` and `pause` but not for these. On a slow or hung data volume (the Unraid FUSE case the code mentions) they stall a runtime worker. `status()` and `with_inputs` also hold the `found` mutex across that I/O.
- Suggested fix: route them through `blocking`, as `status` is.
- Tracked?: not found.

### UPDATE-10 A refused overlay leaves `active` pointing at it, and `<version>.bad` folders are never removed
- Severity: polish
- Kind: maintainability
- Confidence: confirmed
- Where: `crates/lodestar/src/update/supervisor.rs:584-594`.
- What: on a definitive verification failure the folder is renamed to `<version>.bad` (kept for diagnosis, by decision) but `active` isn't cleared, so every later start re-reads it, fails with "missing", and raises `bad_overlay` again until a newer image arrives. Nothing ever deletes a `.bad` folder (about 40 MB each). When `<version>.bad` already exists the rename fails silently and the bad folder stays under its version name.
- Suggested fix: remove `active` (and `pending`) when the overlay is set aside; have `newer_image_wins` or `passed` prune `.bad` folders.
- Tracked?: the `.bad` rename is recorded in `docs/decisions/2026-10-23-B-update-r2.md`; the cleanup isn't.

### UPDATE-11 Stale module docs say Windows installs nothing
- Severity: polish
- Kind: docs
- Confidence: confirmed
- Where: `crates/lodestar/src/update/mod.rs:18-23` and `:109-110`, `crates/lodestar/src/update/windows/mod.rs:8`.
- What: both still say "installing waits for PK-win-upd-2 and PK-win-portable-upd, so Install and Roll back answer `not_built_yet`" and "installs nothing yet". The MSI and portable installs exist; only Roll back, the window and automatic installs are missing.
- Suggested fix: update the comments with PR 795.
- Tracked?: not found.

## Checked and found sound
- **Signature primitive** (`crates/update/src/minisign.rs`): prehashed `ED` only, legacy refused; key number, signature over the BLAKE2b-512 prehash and the global signature all required; strict four-line parse, strict base64, size caps before decoding.
- **Verify, then parse** (`verify.rs`): the body is parsed only after its signature verifies under the current key of the channel's role. The one read-ahead (`doc::statements_ahead`) yields only statements, each verified under the recovery key before its own body is read.
- **What is signed**: channel, timestamp, expiry, every release's version, level, flags, `min_from`, and per target and kind the URL, SHA-256 and size are all inside the signed body (`doc.rs`). A `type` tag stops a manifest, key statement, freshness statement or `SHA256SUMS` signature being read as another. Unknown and duplicate fields, floats, a BOM and build metadata are refused; every list is capped.
- **Channel and key separation**: Stable/Beta need the release key, Nightly the nightly key, freshness its own key; `WrongChannel` is checked on the signed body; a build with no nightly key refuses the channel with a reason.
- **Rollback, replay and freeze** (`verify.rs`, `trust.rs`): per-channel high-water mark on the signed timestamp, the same timestamp only with the same body, at most one day ahead of the clock, at most 30 days of life, freshness bound to the manifest's SHA-256 with its own mark. `TrustState::merge` is forward-only. The floor is rebuilt from embedded statements and verified overlays, so deleting or restoring `trust.json` doesn't lower it (`trust_file.rs`).
- **Version monotonicity**: `policy::plan` and `install_now` need `version > running`; the supervisor's `switch` and `plan` refuse anything not newer than the running version and the image; the Windows helper applies `msi::newer` to its own build version, not to anything in the data folder.
- **Key rotation** (`chain.rs`): recovery signature required, one contiguous sequence, equivocation and key or key-number reuse refused, stored statements normalised.
- **No runtime key override in release builds**: the test trust root and every fault hook are behind `cfg(debug_assertions)` (`update/testing.rs`); the release profile doesn't enable debug assertions; the `testing` feature of `lodestar-update` is a dev-dependency only.
- **Download** (`download.rs`, `check.rs`): HTTPS only, every redirect hop checked against the three-host allowlist with no port or credentials, at most 5 hops, the SSRF-guarded client, the size cap enforced while streaming, 60 s idle and 30 min overall timeouts, the hash and size checked before anything else touches the file, the `.part` created `O_EXCL|O_NOFOLLOW` 0600, no resume, failures and stale parts removed. The check sends a fixed user agent and no identifier.
- **Verify-to-exec TOCTOU on Unix** (`overlay.rs`, `supervisor.rs`): the supervisor copies the binary into a sealed memfd, hashes the copy and executes the copy. The no-memfd fallback is announced and recorded as a residual in `docs/decisions/2026-10-23-B-update-r2.md`. `<data>/bin` is opened `O_NOFOLLOW`, must be owned by the process's user, and is forced to 0700.
- **Nothing is extracted on Unix**: the artifact is a single binary, and overlay folder names are canonical versions, never paths (`layout::parse_version`, `install.rs:53-60`). The portable zip is hashed and extracted from one handle with the bounded extractor, and only the one named entry is taken (`portable.rs:327-360`).
- **Privilege on Unix**: the PUID/PGID drop happens in `main.rs` before the config is read and before `supervisor::run`, so the supervisor (PID 1 in Docker) and its child never run as root when a target account is set. The supervisor never opens the network or the store for writing.
- **Unix rollback journal** (`journal.rs`): each step is idempotent and journaled by temp file, fsync and rename; the snapshot's SHA-256 is recorded before the rename and a repeat is recognised by it; the WAL is removed before the rename; a journal that can't be read stops every start; a rollback that would strand the store is refused before anything is written. The UPDATE-01/03 gaps are about what the restored store contains, not about crash safety.
- **Windows helper** (`windows/helper.rs`, `msi.rs`, `rollback.rs`): it refuses unless it is `%ProgramFiles%\Lodestar\lodestar.exe`; never reads `--config` or `LODESTAR_DATA_DIR`; treats `request.json` as untrusted (capped, strict, canonical version, hex id); copies the envelope and MSI into the SYSTEM-only staging folder and verifies the copies; keeps its own forward-only `trust.json` there, raised but never lowered by the data folder's; re-hashes the MSI in the detached copy right before `msiexec`; passes fixed `msiexec` arguments as separate argv entries; starts every tool by absolute `System32` path with `LODESTAR_*` removed; quotes the task's command; keeps the journal and pins where only SYSTEM and Administrators can write; nothing in the data folder decides a rollback.
- **API** (`crates/api/src/updates.rs`): every route needs a session; API keys and feed tokens are read-only and can't reach the POST routes; writes are same-origin JSON; `days` is restricted to 0, 1, 7 or 30; the version is parsed canonically; Roll back needs `confirm`. There is one admin role, so no lower-privilege caller exists to escalate.
- **Policy and window** (`policy.rs`, `window.rs`): withdrawn, skipped, manual-steps and unreachable releases never install automatically; the wait counts from the signed release date; saturating arithmetic; a pause that can't be saved holds installs; DST gaps and repeated hours are handled by scanning minute and transition boundaries.

## Area-level concerns
- **Three store-restore implementations have drifted.** `cli/rollback.rs` (copy, full purge and hold carry, index aside), `update/journal.rs` (rename, source purges only, new index entries only) and the two Windows paths (rename, nothing). UPDATE-01, -03 and -05 are the same root cause. One shared "prepare this snapshot to become the store" function in `lodestar-store`, used by all of them, would stop the next restore path (PR 795) inheriting the gap.
- **`engine.rs` and `windows/check.rs` are parallel implementations** of the same scheduler, state file, policy wiring and API (about 1,700 non-test lines each), already different in small ways: Windows passes `held_in_process: false`, has no install-time re-verification on the portable path, and keeps no "failed until the next check" memory. `check.rs:1568` says the helpers "mirror `engine.rs`'s". Fixes such as UPDATE-07 have to be made twice.
- **The Windows chain of trust continues outside this area.** The helper's safety rests on the staging folder's ACL (`windows::acl::secure_dir`), the data folder's ACL check, the scheduled task's descriptor as registered by the MSI, and the MSI's SYSTEM custom actions that walk the service-writable data folder. I reviewed none of `packaging/windows` or `windows/acl.rs`; the reviewer of that area should confirm that the task SDDL in `msi.rs:591-593` is what gets registered, and how the custom actions treat junctions under `<data>`.
- **Release engineering wasn't reviewed.** Whether `release.yml` produces what `crates/update/README.md` specifies (statements carried after a rotation, weekly freshness, manifests dated at or after a rotation) decides whether clients ever freeze; M8-SEC-5 in `runner/slices.yaml` plans that review.
- **Overlays have no downgrade or expiry protection by design** (`verify::overlay`: no clock, no mark). Anyone who can write `<data>/bin` can plant an older, validly signed release. It's a recorded accepted risk (§12.2 "Residual risk"); it makes the 0700 owner check on `<data>/bin` the only barrier, and that check accepts any filesystem that reports the right uid.
- **Windows probation is weak and recorded as such**: "Running for 60 s", one attempt, before the store is even opened. Combined with UPDATE-05, a Windows rollback is both easier to trigger and less careful about what it restores than the Unix one.
- **Tests were not read.** The crash-point and vector suites look extensive from their names, but I can't say whether any waits with a fixed sleep or can't fail.
