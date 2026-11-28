# X-testvm-delete-on-termination decisions

The runner's brief (2026-11-28Z): #1017 (X-testvm-teardown-by-id) found that both test-VM launch steps and `Deploy-AwsWindows.ps1` leave the root volume's `DeleteOnTermination` to the image's default. An image that says false would leave a paid volume behind after termination. No owner decision changes.

- **The root (`/dev/sda1`) mapping sets `DeleteOnTermination=true` explicitly** in `infra/dev-vm/linux-aws.md` step 2, `infra/dev-vm/windows.md` step 2 and `Deploy-AwsWindows.ps1`.
- **Teardown step 7 is unchanged.** Deleting the root volume by id if it still exists stays as the backstop; each runbook says so next to the new setting.
