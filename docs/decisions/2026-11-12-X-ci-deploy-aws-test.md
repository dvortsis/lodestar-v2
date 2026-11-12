# X-ci-deploy-aws-test decisions (#774)

The runner's decision, after #771: CI parsed `Test-DeployAwsWindows.ps1` but never ran it, so nothing guarded `Get-SafeAwsError`'s cases or the launch arguments.

- **One new step in the existing `infra` job, not a new job.** It runs after the `StateBackend.ps1` test, under the job's existing path filter and `pwsh` (GitHub's ubuntu runners ship pwsh 7). The required `ci-ok` set is unchanged. Nothing new is pinned, and no workflow `permissions:`, secrets, environments or OIDC settings changed.
- **The deploy script runs in CI only against a stub.** The job's rule was "no script that can reach a cloud runs here". This is the one exception, and the job's header comment says so. The guards:
  - the step removes every `AWS_*` variable first;
  - the test's PATH holds only the stub's folder and `/usr/bin:/bin`, and the runner's AWS CLI lives in `/usr/local/bin`;
  - the test stops before any case unless the deploy script's own resolver finds the stub;
  - the stub records any `AWS_*` name that reaches it, and a check asserts none did.
- **The test now ends with `exit 0`, which is the real fix.** A passing test used to leave `$LASTEXITCODE` at 1, from its last case's child process, which is meant to fail. GitHub's pwsh wrapper ends every step with `exit $LASTEXITCODE`, so the step would have failed every time. The step's own `if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }` only keeps that check visible and independent of the wrapper. One pwsh script calling another doesn't stop on the inner `exit 1`; it only sets `$LASTEXITCODE`.
- **Shown to guard the cases (2026-11-12, local, pwsh 7.4.6).** I ran the step's script exactly as `ci.yml` has it, on throwaway local commits that were never pushed:
  - removing the IPv6 mask: exit 1, 3 checks FAIL;
  - going back to `-replace` for the ASCII strip: exit 1, 1 check FAIL;
  - the clean tree: exit 0, all checks pass, in about 7 s.

  The sandbox had three `AWS_*` variables set, and the step removed them.
- **Small extras:** a lone CR mid-line is now a table case. It is stripped, not treated as a line break. The long comment above `Get-SafeAwsError` is reflowed.
