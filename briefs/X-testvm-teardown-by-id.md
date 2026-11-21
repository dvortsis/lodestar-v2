# X-testvm-teardown-by-id

Runner brief, 2026-11-21Z. Slice id `X-testvm-teardown-by-id`, branch `slice/X-testvm-teardown-by-id`, PR title `X-testvm-teardown-by-id: <title>`. The plan entry is in `runner/slices.yaml` (#1012). Docs only; no cloud call, no terraform.

## Why
#994's review: the teardown step in both test-VM runbooks checks `DescribeVolumes` with `tag:Purpose=X-test-vm` and expects "your volume" not to be listed. That only works if the volume got its tags, and another lane's volume is listed too, so "not listed" is hard to read. Checking the lane's own volume by its id is exact.

## What
1. **`infra/dev-vm/linux-aws.md` step 7 and `infra/dev-vm/windows.md` step 7 (Teardown):**
   - Before `TerminateInstances`, take the root volume id from `DescribeInstances` (`BlockDeviceMappings`, `/dev/sda1`). Say plainly that the id is kept only in the session, never written to a file, report, PR or message (the repo's rule: no instance or volume ids in records).
   - After `terminated`, call `DescribeVolumes` with `VolumeIds=[that id]`: it must fail with `InvalidVolume.NotFound`, or return the volume as `deleted` or `deleting` (then check once more until it's gone or NotFound, with a deadline). Anything else (`available`, `in-use`) means the volume survived: delete it if the policy allows it, and report it to the runner either way.
   - Keep the tag check as a second line, worded as a backstop, and keep "never filter by `Project`".
   - If the launch step doesn't already say `DeleteOnTermination=true` for `/dev/sda1`, check what the runbook says and note the gap in the decisions file; don't change the launch parameters (the IAM side is the owner's, owner queue item 31).
2. **`windows.md` nits from the same review:** GiB, not GB, for memory, pagefile and volume sizes (lines 77–78 and anywhere else in that file); and the sub-list punctuation at lines 77–78 (the last bullet of the "Bootstrap pitfalls" list starts lower-case after a list whose items end in `;`; make the list consistent).
3. **A decisions file** `docs/decisions/2026-11-21-X-testvm-teardown-by-id.md`, heading with `(#<this PR>)`, recording the change and that requiring the `Purpose` tag on volumes in the IAM policy stays the owner's (owner queue item 31). Don't edit #994's decisions file.
4. This slice's `slices.yaml` entry only if something in it is wrong; nothing else in that file.

## Owns
- `infra/dev-vm/linux-aws.md`
- `infra/dev-vm/windows.md`
- `docs/decisions/2026-11-21-X-testvm-teardown-by-id.md`

Send me the list before you edit anything else.

## Bars
`runner/standards.md`. Never write an instance id, volume id, account number, address or firewall detail. Run whatever docs lint CI runs on `infra/dev-vm/` and `docs/` (see `.github/workflows/`). Push once per round, and send "ready <pr> <sha>" when CI is green.
