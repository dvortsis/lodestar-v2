# X-testvm-teardown-by-id decisions (#1017)

Slice session, 2026-11-21. It follows #994's review. The teardown step in both test-VM runbooks (`infra/dev-vm/linux-aws.md` and `infra/dev-vm/windows.md`, step 7) used to check that `DescribeVolumes` with `tag:Purpose=X-test-vm` no longer listed "your volume". That works only if the volume got its tags, and another lane's volume is listed too, so "not listed" was hard to read. Both runbooks now check the lane's own volume by its id. Docs only: no cloud call, no template or Terraform change.

- **Who decided:** choices marked "runner" are the runner's calls. The rest are the slice session's, for the runner to confirm.

## The teardown now

1. **Once the instance is `Online` (step 3),** the session takes the root volume's id from `DescribeInstances`: the `BlockDeviceMappings` entry for `/dev/sda1`, `Ebs.VolumeId`, and keeps it for teardown (runner).
   - A session that resumed without it reads it before `TerminateInstances`. If the instance is already gone (the expiration guard ended it), its mappings are empty, so the check by id is skipped, the tag check is relied on, and the runner is told.
   - `TerminateInstances` waits for `terminated` with a 10 min deadline, and tells the runner if it isn't reached.
   - The id is kept in the session only. It never goes into a file, a report, a PR or a message, under the repository's rule of no instance or volume ids in records.
   - The test user may call it: its policy allows `ec2:Describe*`, and the only Describe call it denies is `ec2:DescribeInstanceAttribute` (beside its deny of all of `ec2:*` outside us-east-1).
   - The entry is the one whose `DeviceName` is the instance's `RootDeviceName`, as the expiration guard picks it; that's `/dev/sda1` for both launches. The mappings empty once the instance is terminated, so the id has to be taken first.
2. **After `terminated`,** the session calls `DescribeVolumes` with `VolumeIds=[that id]`:
   - `InvalidVolume.NotFound` (a raised `ClientError` in `run_script`, so the runbooks say to catch it and read its code), or the volume as `deleted`: it's gone;
   - `deleting`: check again in later calls until it's gone, with a 10 min deadline (no fixed sleep);
   - `available`, `in-use` or any other state (`error`, say): the volume survived. Still `deleting` at the deadline: its deletion didn't finish. Both are reported the same way.
3. **A surviving volume is reported to the runner** without its id, and the owner deletes it.
   - The brief said to delete it "if the policy allows it". It doesn't: the test user's policy in `infra/dev-vm/aws-claude-testvm.json` has no `ec2:DeleteVolume`, so the runbooks say not to try.
   - Whether to grant `DeleteVolume` on test-tagged volumes is an IAM change for the owner. No owner queue item covers it yet (item 31 is the `Purpose` tag on volumes), so the runner may add one. This slice doesn't change the policy.
4. **The tag check stays, worded as a backstop.**
   - It lists every lane's test volumes, and every lane tags them with the same `LaunchedBy`: one IAM user, so the same `UserId`.
   - Any `available` test volume there is a leftover to tell the runner about, without its id, in both runbooks. On Linux, one with this lane's `Name` is probably this lane's (two lanes with the same role on the same day share a `Name`); the Windows launch sets no `Name` tag, so there it may be any lane's.
   - It finds nothing if the volume never got its tags.
   - "Never filter by `Project`" stays: the owner's fleet volumes share that tag.

## Left as they are

- **`DeleteOnTermination` isn't stated.**
  - Neither launch step names it for `/dev/sda1`. Linux's step 2 gives `VolumeSize=100`, `VolumeType=gp3` and `Encrypted=true`; Windows' gives `VolumeSize=100` and `Encrypted=true`.
  - A launch that overrides the image's `/dev/sda1` mapping without `DeleteOnTermination` takes the value from the image's own mapping, which deletes the root volume on termination for the stock Ubuntu and Windows images. This slice made no cloud call to confirm it for the current images.
  - The brief says not to change the launch parameters, so they're unchanged. The check by id now catches a volume that survives for any reason.
  - Adding `DeleteOnTermination=true` to both launch steps, and to `infra/akamai/terraform/scripts/Deploy-AwsWindows.ps1`'s mapping, which doesn't set it either, is a small follow-up for the runner.
- **Requiring the `Purpose` tag on volumes in the IAM policy** (so a launch whose volume isn't tagged is refused) stays the owner's: owner queue item 31.
- **#994's decisions file** is unchanged.

## `windows.md` nits from the same review

- **Units:** the memory, pagefile and process sizes in the "Bootstrap pitfalls" list now say GiB, not GB. The rest of the file already did.
- **Punctuation:** that list's items end in `;`, and the last ends in `.`. The item before it used to end in `.` as well, mid-list.
