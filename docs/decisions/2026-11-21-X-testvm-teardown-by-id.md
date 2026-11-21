# X-testvm-teardown-by-id decisions (#PR)

Slice session, 2026-11-21. It follows #994's review. The teardown step in both test-VM runbooks (`infra/dev-vm/linux-aws.md` and `infra/dev-vm/windows.md`, step 7) used to check that `DescribeVolumes` with `tag:Purpose=X-test-vm` no longer listed "your volume". That works only if the volume got its tags, and another lane's volume is listed too, so "not listed" was hard to read. Both runbooks now check the lane's own volume by its id. Docs only: no cloud call, no template or Terraform change.

- **Who decided:** choices marked "runner" are the runner's calls. The rest are the slice session's, for the runner to confirm.

## The teardown now

1. **Before `TerminateInstances`,** the session takes the root volume's id from `DescribeInstances`: the `BlockDeviceMappings` entry for `/dev/sda1`, `Ebs.VolumeId`.
   - The id is kept in the session only. It never goes into a file, a report, a PR or a message, under the repository's rule of no instance or volume ids in records.
   - The test user may call it: its policy allows `ec2:Describe*` and denies only `ec2:DescribeInstanceAttribute`.
2. **After `terminated`,** the session calls `DescribeVolumes` with `VolumeIds=[that id]`:
   - `InvalidVolume.NotFound`, or the volume as `deleted`: it's gone;
   - `deleting`: check again in later calls until it's gone, with a 10 min deadline (no fixed sleep);
   - anything else (`available`, `in-use`, or still `deleting` at the deadline): the volume survived.
3. **A surviving volume is reported to the runner** without its id, and the owner deletes it.
   - The brief said to delete it "if the policy allows it". It doesn't: the test user's policy in `infra/dev-vm/aws-claude-testvm.json` has no `ec2:DeleteVolume`, so the runbooks say not to try.
   - Whether to grant `DeleteVolume` on test-tagged volumes is the owner's (the IAM side, owner queue item 31). This slice doesn't change the policy.
4. **The tag check stays, worded as a backstop.**
   - It lists every lane's test volumes, and every lane tags them with the same `LaunchedBy`: one IAM user, so the same `UserId`.
   - On Linux, only an `available` volume with this lane's `Name` is this lane's leftover. The Windows launch sets no `Name` tag, so on Windows an `available` one is a leftover to report, not necessarily this lane's.
   - It finds nothing if the volume never got its tags.
   - "Never filter by `Project`" stays: the owner's fleet volumes share that tag.

## Left as they are

- **`DeleteOnTermination` isn't stated.**
  - Neither launch step names it for `/dev/sda1`. Linux's step 2 gives `VolumeSize=100`, `VolumeType=gp3` and `Encrypted=true`; Windows' gives `VolumeSize=100` and `Encrypted=true`.
  - So the runbooks rely on EC2's default for the root volume, which is to delete it on termination. This slice made no cloud call to confirm that for these images.
  - The brief says not to change the launch parameters, so they're unchanged. The check by id now catches a volume that survives for any reason.
  - Adding `DeleteOnTermination=true` to both launch steps is a small follow-up for the runner.
- **Requiring the `Purpose` tag on volumes in the IAM policy** (so a launch whose volume isn't tagged is refused) stays the owner's: owner queue item 31.
- **#994's decisions file** is unchanged.

## `windows.md` nits from the same review

- **Units:** the memory, pagefile and process sizes in the "Bootstrap pitfalls" list now say GiB, not GB. The rest of the file already did.
- **Punctuation:** that list's items end in `;`, and the last ends in `.`. The item before it used to end in `.` as well, mid-list.
