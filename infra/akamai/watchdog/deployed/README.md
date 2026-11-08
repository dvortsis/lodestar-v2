# Lodestar Akamai 48-hour expiration watchdog

The canonical **source** CloudFormation template in `lodestar-akamai-watchdog.json` was reconciled against the live AWS `lodestar-akamai-watchdog` stack before its account-specific SNS topic ARN was converted to the corresponding AWS pseudo-parameter expression. The embedded `index.py` Lambda source was verified identical to the deployed inline code at reconciliation time. Two changes have been made since (`docs/decisions/2026-11-08-X-cloud-infra.md`); the deployed copy has neither until the stack is next updated:
- the API path allowlist no longer admits `/networking/firewalls?page=`, a path the code never calls;
- with `TOKEN_REQUIRED=true`, which the template now sets, a missing or empty secret fails the run so the error alarm fires, instead of returning `unconfigured` silently.

Tests: `python3 -m unittest test_watchdog -v` from this folder. Rebuild check: `python3 build_template.py && git diff --exit-code`.

- AWS Lambda: `lodestar-akamai-expiration-guard`
- Schedule: EventBridge, every 30 minutes
- Alert topic: `arn:${AWS::Partition}:sns:${AWS::Region}:${AWS::AccountId}:lodestar-expiration-alerts`
- Token location: AWS Secrets Manager secret `lodestar/akamai-token`, **not** in Git
- Eligible instances: label `lodestar-*`, tags `lodestar` and `test`, Akamai region `us-east`, approved instance types, created after activation threshold, age at least 48 hours

The token has been installed and a live, non-destructive Akamai inventory scan was verified. Actual deletion against an expired test Linode is **not** yet verified.

`build_template.py` only refreshes the embedded Lambda source from `index.py`; it makes **no** AWS API calls. Compare resulting CloudFormation changes with the live stack and obtain human authorization before deployment. Never deploy an older or draft watchdog alongside this service.

**Limitations:** The watchdog does not delete detached Linode firewalls, volumes or reserved addresses. Terraform must be reconciled after out-of-band deletion, and a subsequent Terraform apply can recreate deleted resources. Do not treat a dry-run as end-to-end deletion verification.

To run the local token installer again, explicitly supply the expected AWS account ID in `LODESTAR_AWS_ACCOUNT_ID`, and authenticate interactively with `lodestar-admin`. Never commit the token or AWS session credentials.
