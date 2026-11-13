# X-scrub-guard-fields decisions (#848)

#845 (X-gluetun-guard-reboot) records host-guard drops and a `damaged` mark in the block directory, but the copy-off scrubber didn't carry them. That was follow-up N7 from #845's review. The scrubbed document now has two more allow-listed fields.

## The fields
- **`host_guard_drops`: a count.**
  - **The source:** the same records `guard_drops` in `block-lib.sh` counts, matched more strictly. It counts the lines of `$BLOCK/outages` that are exactly `<unix seconds> host_guard_dropped`, split on newlines only. `guard_drops` counts any line that ends with the kind, so the two can differ on a corrupted file.
  - **Exact matches only.** A line with anything after the kind, or anything other than digits before it, isn't counted. Nothing from the file reaches the document but the number.
- **`damaged`: a boolean.**
  - **True when** the `damaged` mark exists, or the outages hold a `host_guard_lost` line.
  - **The mark is only tested for existence, never read.** Its text is not evidence and can't pass.
  - **Why the outages line counts too:** `guard_lost` is the only writer of either, and it writes both once: the mark first, then the line. copy-off mounts the outages into the scrubber's container, but not the mark (see "Left as it is").
  - **That leaves a false negative.** A reboot between the two writes, or a failed append to the outages (a full disk, say; both writes are silenced), leaves the mark without the line. The VM then reports `fail block_intact`, but the evidence says `damaged: false`. Mounting the mark in copy-off closes it.
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

## Left as it is (for the runner)
- **copy-off doesn't mount the `damaged` mark (a priority for the next copy-off brief).** `copy-off.sh` mounts only `evidence`, `block.env`, `tools`, `outages` and `notes`. The scrubber therefore learns of the damage from the outages line. Adding `damaged` to copy-off's mount list (outside this slice's paths) would make the mark itself the source, as the brief intended. The code reads it already.
- **Nothing reads the new fields yet.** No analyzer, control-plane report or runbook section reads `outages` or `outage_notes` from the scrubbed document either. A report that shows them is a follow-up.
