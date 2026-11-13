# X-scrub-guard-fields decisions (#848)

#845 (X-gluetun-guard-reboot) records host-guard drops and a `damaged` mark in the block directory, but the copy-off scrubber didn't carry them. That was follow-up N7 from #845's review. The scrubbed document now has two more allow-listed fields.

## The fields
- **`host_guard_drops`: a count.**
  - **The source:** the same records `guard_drops` in `block-lib.sh` counts, matched more strictly. It counts the lines of `$BLOCK/outages` that are exactly `<unix seconds> host_guard_dropped`, split on newlines only. `guard_drops` counts any line that ends with the kind, so the two can differ on a corrupted file.
  - **Exact matches only.** A line with anything after the kind, or anything other than digits before it, isn't counted. Nothing from the file reaches the document but the number.
- **`damaged`: a boolean.**
  - **True when** the `damaged` mark exists, or the outages hold a `host_guard_lost` line.
  - **The mark is only tested for existence, never read.** Its text is not evidence and can't pass.
  - **copy-off mounts a stand-in for the mark,** read-only, beside the outages and only when it exists. A block without one copies off as before. (The runner added `copy-off.sh` to this slice's owns for this one change.)
  - **What it mounts is an empty stand-in** (`/dev/null`, a character device that reads as empty, at `/block/damaged`), never the mark itself (X-scrub-guard-fields-nits). The scrubber only needs to know the mark exists, so the container never holds the mark's text, even when the mark is a symlink. A dangling link still counts as a mark.
  - **Why the outages line counts too:** `guard_lost` is the only writer of either, and it writes both once: the mark first, then the line. Either one is enough.
  - **No false negative is left.** A reboot between the two writes, or a failed append to the outages (a full disk, say; both writes are silenced), leaves the mark without the line. A stand-in for the mark is mounted, so the evidence still says `damaged: true`, as vm-status's `fail block_intact` does. A test runs copy-off with the mark and no `host_guard_lost` line, then scrubs exactly the files it mounted.
- **A new shape, `count`, for `host_guard_drops`.** It's a whole number from 0 up, narrower than `num`, which also allows negatives and floats. `damaged` uses the existing `boolean`. Both go through `clean()`, so the control plane's `scrub(doc)` drops a value of the wrong shape, as it does for every other field.
- **The scrubber stays an allow-list.** No free text passes, and no shape is broader than the existing ones.

## Older blocks
- **A block from before #845 still scrubs.** With no outages file and no mark, the fields are present as `host_guard_drops: 0` and `damaged: false`, rather than absent. Before #845 there was nothing to record, so 0 and false are true statements for those blocks too.
- **A document from an older scrubber has neither field.** The control plane's pass keeps it valid, since `clean()` treats every field as optional.

## Tests
New tests in `infra/fleet/tests/test_block.py`, class `ScrubGuardFields`:
- a block with drops and a mark: the count, the boolean, and a stable second pass;
- a clean block, and one from before #845: 0 and false;
- a `host_guard_lost` line alone marks the block damaged;
- **a hostile mark and hostile outage lines:** an address, a hex ID, a path and an instance ID, all fake documentation values. Only the boolean and the exact records survive, through both `collect()` and the `scrub.py collect` command line;
- malformed values (negative, float, string, boolean, oversized, an address, a list) are dropped by `scrub(doc)`.

In `CopyOff`: copy-off mounts a stand-in for the mark, read-only, when it's there, and not when it isn't. With the mark and no `host_guard_lost` line, scrubbing exactly the files copy-off mounted gives `damaged: true`.

## Left as it is (for the runner)
- **Readers:** see "Readers and the mark's creation" below. The control plane's copy-off report reads the new fields. Nothing reads `outages` or `outage_notes` from the scrubbed document yet.

## Follow-up (X-damaged-mark-links)
- **Every check of the mark agrees with copy-off.** vm-status's `block_intact`, the outage-watcher step and `guard_lost` all use `damaged_marked` in `block-lib.sh` (`-e || -L`), so a dangling link counts as a mark everywhere. The scrubber counts a link too. `guard_lost` never writes through a link: a link already counts as a mark and is left in place.

## Readers and the mark's creation (X-scrub-fields-readers)
- **The control plane reports the guard's state.** copy-off's report line (`copied_off` in `steps/run.py`), the one reader of the scrubbed document, now ends with the host guard's state, in fixed words and a count:
  - `not clean: damaged, the host guard couldn't be held` when `damaged` is true. The block's result is not clean, and this says why.
  - `host_guard_drops <n>` when there were drops;
  - `host guard held` when both fields say nothing went wrong;
  - `host guard not reported` for a document from before #848, with neither field. It's never read as clean or as damaged. When both values are malformed, the scrub drops both first, so it reads the same way.
  - **One field missing, or dropped by the scrub for its shape,** gives its own part: `damaged not reported` or `host_guard_drops not reported`. `host guard held` needs both fields present and both saying nothing went wrong. A `damaged: true` still reads as not clean whatever the count says.
  - **Why "not clean:" and not "failed: <name>":** copy-off itself succeeded, and "failed:" in the report line means a step's check failed. So a damaged block is said in its own words, and the step still reads `done` (the runner's call).

  The state is reported even when the evidence couldn't be stored.
- **The mark is created race-free.** #851 created it with `noclobber`, which only refuses a link to a regular file or a dangling link. A link to a FIFO, a device or a directory would have been opened or written. `mark_create` now writes a new temp file in `$BLOCK` (`mktemp`) and hard-links it into place with `ln -T`. That fails if anything is already at the mark's path, and never writes through it. The temp file is always removed. A test puts a link to a file, to nothing, to a FIFO and to a directory where the mark goes, then calls `mark_create`: each is refused and left as it was.
- **Rollout.** Blocks already running keep the guard copy installed when they started (`$BLOCK/tools/block-lib.sh`, staged by start-block) until they're restarted, or until the outage-watcher step starts a stopped watcher, which stages the current copy again. The outage-watcher and vm-status steps themselves always run the current copy. The new mark creation and readers apply to blocks started after this merges; the control plane's report applies at once.
- **The runbook (outside `infra/fleet/`) isn't changed here.** §3.7's example "works" line, `evidence stored in the private bucket`, now gains the guard's state. The runner decides whether to update it.
