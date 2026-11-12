# X-test-infra-nits decisions (#771)

Follow-ups to #770 from its security review's optional findings (`runner-state:reviews/rv-770-e5aefe9e.md`). The runner set the list; none changes a decision in `2026-11-12-X-test-infra-blockers.md`.

- **addKey success needs curl's exit 0 again** (`pia-wireguard.sh`, stage 15). Success now needs exit 0, a 2xx status and `"status": "OK"`, so "success" means what it did with `--fail`, plus the 2xx rule. The stage-15 line now leads with curl's exit code (`curl 63` is the 64 KiB cap, `curl 60` a certificate the pinned CA didn't sign). It stays bounded and stripped, and the exit code stays 15.
- **jq's pre-filters anchor with `\A…\z`.** jq's `$` also matches before a final newline, and `$(...)` then strips it, so `"abc\n"` became `abc`. That was harmless, but now jq and bash agree exactly. The same anchor applies to the server address.
- **The offline deploy test clears AWS credentials and checks the stub first.** The child process gets no `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, `AWS_PROFILE` or `AWS_DEFAULT_PROFILE`. Before any case runs, the deploy script's own resolver (`AwsCli.ps1`), under the test's PATH, must find the stub; otherwise the test stops with nothing run. This is defence in depth before the runner wires the test into CI. The stub records whether any of them reached it, and a check asserts none did.
- **A refused security-group read says why.** The script prints AWS's first error line, printable ASCII only. ARNs, access key IDs (`AKIA`/`ASIA`), 12-digit account IDs, and IPv4 and IPv6 addresses are masked, and AWS's encoded authorization message is dropped. The line is at most 200 characters. The refusal names the permission the read needs, `ec2:DescribeSecurityGroups` on `LodestarProvisioner`. That permission can't be checked from the repo. Granting it is the owner's call, and the runbook says so. No IAM policy changed.
- **Review round (runner, `rv-771-4b618cbb`):**
  - The printable-ASCII strip uses `-creplace`. `-replace` ignores case, so U+212A KELVIN SIGN matched `a-z` and survived.
  - `Get-SafeAwsError` is table-tested directly, with one mask per line, so the 200-character cut can't hide one. The function is loaded from the deploy script's own syntax tree.
  - Deleting any one mask, or switching back to `-replace`, now fails the test.
  - The test clears every `AWS_*` variable but its own three, and the stub checks that none reached it.
- **Example account IDs in four test lines on `main` became `111122223333`** (runner-approved, so the repo's only 12-digit numbers are the two placeholders): `test_watchdog.py` (two lines), `test_guard.py` and `test_templates.py`.
