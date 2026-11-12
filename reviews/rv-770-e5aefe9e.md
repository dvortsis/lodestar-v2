VERDICT: MERGE

# Security review: PR #770 (X-test-infra-blockers), head e5aefe9ececdc1696daff8afd83b31f798cbf6bd

Offline only. I made no cloud, AWS, PIA or Terraform calls. The one network call was a read-only `gh api` for this commit's check runs: `ci-ok` and `infra` are green, and `infra` includes CI's PowerShell parse check of every `infra/**/*.ps1`.

## What I ran
- **PIA tests:** `python3 -m unittest discover -s tests -t .` in `infra/pia-bridge` (this suite includes the new stage-13 and stage-15 cases in `tests/test_vm_step.py`). 45 tests, all pass.
- **shellcheck 0.11.0** on `infra/pia-bridge/vm/pia-wireguard.sh` (the only changed shell file): clean.
- **The hostname regex, alone in bash 5.2 with `LC_ALL=C`:**
  - Accepted: `us-brooklyn.privacy.network`, `brooklyn402`, `a`, `1.2.3.4`, a 63-byte label, and a name of exactly 253 bytes.
  - Rejected:
    - a 64-byte label and a 254-byte name;
    - `:`, `/`, `@`, `?`, `#`, `%`, `_`, `*`, `[`, `]`, `{`, `}`, `\`, `,`, `;`, `'` and `"`;
    - space, tab, LF, CR, `\x01` and `\x7f`;
    - a leading `-`, `--connect-to`, `a-`, `a.-b` and `a-.b`;
    - `a..b`, `.a`, `a.` and the empty string;
    - `A` (lowercased before the check) and UTF-8 `é`.
- **The full jq-then-bash pipeline, run under `LANG=C.UTF-8`:** rejected `"abc\nfoo"`, `"\nabc"`, `"a\r"`, `"a\u0000b"`, `"ab\u0000"`, `"a.b\n:1.2.3.4:"`, Kelvin `K` (U+212A) and `İ`. `"ABC"` became `abc`, as intended. `"abc\n"` became `abc` (see Optional 2).
- **Real curl against a localhost-only test server** (no endpoint the scripts use), to confirm how `--output` and `--write-out '%{http_code}'` behave without `--fail`:
  - A 401 reply: exit 0, `http=401`, body saved.
  - A chunked reply over 64 KiB: exit 63, `http=200`, 65,536 bytes written and truncated, so jq fails and stage 15 refuses.
  - Connection refused: exit 7, `http=000`, no output file.
- **Not run: `Test-DeployAwsWindows.ps1`.** `pwsh` isn't installed here (nor is dotnet), and I didn't install it. I reviewed that test and the deploy script by reading them; CI's parse check passes.
- **IDs and IPs in the added lines:** only `111122223333`, plus obvious placeholders:
  - `ami-`, `sg-`, `subnet-` and `i-0123456789abcdef0`, and `sg-0fedcba98765432100`;
  - the IPs 192.0.2.x (a documentation range), 10.9.9.9 and 1.2.3.4, all test fixtures in the existing style.

  No real IDs or IPs.

## Blocking
None.

## Important
None.

## Checked and correct

### pia-wireguard.sh, stage 13 (lines 45–53 and 74–97)
- **The regex is strict enough.** `HOSTNAME_RE` expands to `^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)*$`, with a separate `${#1} <= 253` check.
  - Bash ERE anchors match only at the ends of the string, and the character classes exclude LF.
  - `export LC_ALL=C` (line 21) makes `[a-z]`, `${,,}`, `${#}` and `tr` byte-wise.
  - Nothing that reaches `--connect-to "$wg_cn::$wg_ip:"` or `https://$wg_cn:1337/` can carry `:`, `/`, `@`, `?`, `#`, `%`, glob characters (`[]{}`) or whitespace.
  - The name can't start with `-`. It's never a separate argv element anyway.
- **The jq pre-filter (line 90) handles NUL bytes before bash sees the name.** Bash's `$(...)` drops NUL bytes, so `"a\u0000:1.2.3.4:"` must never come out as a valid-looking name. Anything outside `[A-Za-z0-9.-]` becomes `invalid:"…"`, which the bash check then rejects.
- **The IP gets the same treatment:** a jq pre-check, then `$IPV4` in bash.
- **The new error lines leak nothing.** Every `fail13` line is cut to 200 bytes and passed through `clean` (printable ASCII only). The line includes the validated region and the rejected name, which is not secret. It never includes the WireGuard server's IP, which is the exit address.

### pia-wireguard.sh, stage 15 (lines 104–127)
- **The HTTP status is printed safely.** It's forced to `^[0-9]{3}$`, or `000`.
- **PIA's `.status` and `.message` are bounded and stripped.** They're read only when the reply is a JSON object. jq's `gsub("[^ -~]";"")` strips non-ASCII characters (NUL included), then `clean` runs, then the masks, then the 32 and 200-byte cuts.
  - Stripping runs before masking, so a token split by an inserted control character still gets masked.
  - Masking runs before cutting, so a token can't survive half-cut.
  - The `[token]` and `[key]` replacements contain no `&`, so bash 5.2's `patsub_replacement` doesn't apply.
- **The secrets and the full reply are never printed.**
  - The token is read into a variable only to mask it.
  - The private key never leaves the VM. Only the public key is sent, and the private key is masked as well.
- **The exit code stays 15.**
- **No new temporary file:** `addkey.json` was already there, inside `$work`, which the EXIT trap removes.
- **`set -Eeuo pipefail` still holds:** `|| true` and `|| st=""` are explicit, and no ERR trap exists.
- **Dropping `--fail` doesn't weaken success.** Success now needs a 2xx status **and** `.status == "OK"` in a JSON reply that parses completely, then stage 16's checks.
  - A TLS or pinning failure (`--cacert`) gives `000`, so it fails.
  - A reply cut short by `--max-filesize` doesn't parse, so it fails.
  - This is stricter than before: a 3xx reply with OK used to pass.
- **deliver-pia still discards all of the step's output:** `deliver.py:521` sends stdout and stderr to DEVNULL. The new lines show only to a person running the step by hand.

### Deploy-AwsWindows.ps1
- **The `--network-interfaces` entry is correct (lines 187–191).**
  - The shorthand `DeviceIndex=0,SubnetId=…,Groups=<sg>,AssociatePublicIpAddress=true,DeleteOnTermination=true` is valid for `run-instances`.
  - `--subnet-id` and `--security-group-ids` are gone from `$runArgs`, so the two can't be combined, which AWS rejects.
  - `DeleteOnTermination=true` avoids an orphaned network interface.
  - Nothing else in `$runArgs` conflicts.
- **The security-group check is read-only and fails closed (lines 144–160).**
  - It makes one `describe-security-groups` call with `--group-ids` set to the group the launch uses.
  - `length(IpPermissions)` is non-zero for any inbound rule: IPv4 ranges, IPv6 ranges, prefix lists and referenced groups all live inside `IpPermissions` entries.
  - It refuses when:
    - the call exits non-zero;
    - the answer isn't exactly one row of two fields;
    - the row names another group;
    - the count isn't 1–4 digits;
    - the count is greater than 0.
  - `-or` short-circuits, so an empty answer never indexes `$sgRows[0]` under StrictMode.
  - `ConvertFrom-AwsTextRows` returns `, $rows`, so one row has `.Count == 1`.
  - The check runs after the active-instance check and before the plan, the dry-run return and the typed phrase, so it covers dry runs too.
- **Every existing guard is still there.** The diff removes only the two arguments and one plan line. The parameter-ID checks, the identity and account checks, the expiry policy, the active-instance refusal, the dry run, `Assert-InteractiveConfirmation`, the typed phrase and the state writes are unchanged.

### Test-DeployAwsWindows.ps1 (read, not run)
- **The stub comes first on PATH.** PATH is `<stub>:/usr/bin:/bin`. `Resolve-AwsCli`'s `C:\Program Files\…` path doesn't exist on Linux or macOS, and `Get-Command aws` finds the stub first. No `aws` exists in `/usr/bin` or `/bin` on this machine.
- **The stub refuses a live launch:** it exits 99, and exits 98 on any unexpected call.
- **The test checks that the stub was the CLI used** (`Calls.Count -ge 4`).
- **The refusal cases match the script's branches.**

## Optional
1. **`infra/pia-bridge/vm/pia-wireguard.sh:109`: curl's exit code is now ignored (`|| true`).** Success relies on a 2xx status plus a reply that parses completely and says OK. I couldn't build a case where that passes after a failed transfer: a truncated body doesn't parse, and a TLS failure gives `000`. Requiring rc 0 as well, by saving `$?` instead of `|| true`, would make "success" mean what it did before, at no cost.
2. **`pia-wireguard.sh:90`: jq's `$` also matches before a final newline,** as in Perl. So `"abc\n"` passes the jq pre-filter, and `$(...)` strips the newline, leaving the valid name `abc`. That's harmless (nothing can be injected), but the comment says jq checks the characters. Anchoring with `\\A…\\z` in the jq pattern (`test("\\A[A-Za-z0-9.-]{1,253}\\z")`) would make the two layers agree exactly. Not needed for safety.
3. **`infra/akamai/terraform/tests/Test-DeployAwsWindows.ps1:72-79`: the test inherits the parent's environment.** It points the AWS config and credential files at nothing, but doesn't unset `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN` or `AWS_PROFILE`.
   - **When it matters:** a developer machine with apt's awscli at `/usr/bin/aws` and credentials in the environment, where the stub somehow didn't resolve (for example, `chmod` failed and pwsh skipped the file as not executable). Then the dry-run case would reach real AWS.
   - **Today the stub is first on PATH,** so this is defence in depth only.
   - **The fix:** clear `AWS_*` in `$vars`, and check up front that `Get-Command aws` under the test PATH resolves to the stub. Worth doing before the runner wires the test into ci.yml.
4. **`Deploy-AwsWindows.ps1:148-150` and `.agents/skills/lodestar-fleet/SKILL.md:226`: a refused security-group read hides its reason.**
   - The script now also needs `ec2:DescribeSecurityGroups` on the provisioner role. The policy isn't in the repo, so I couldn't check it.
   - If the call is refused, the human sees only `Could not read security group … (exit 254)`. It fails closed, which is right, but the reason is hidden.
   - The runbook's `UnauthorizedOperation` note covers only the run-instances dry run.
   - **The fix:** a short sanitised `$r.Err`, or one runbook line saying this read can be refused too.
5. **Stage 13's root cause is unconfirmed,** as the decisions file says. The PR assumes capitals in the name, and the new per-check message tells the tester which check failed if that's wrong. That's a sound way to handle it, so this is not a defect.
6. **Path style (trivial):** `SKILL.md:231` writes `tests/Test-DeployAwsWindows.ps1`, while the sentence before it uses `tests\…`.
