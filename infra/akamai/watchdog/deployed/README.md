# Lodestar Akamai 48-hour expiration watchdog

The canonical **source** CloudFormation template in `lodestar-akamai-watchdog.json` was reconciled against the live AWS `lodestar-akamai-watchdog` stack before its account-specific SNS topic ARN was converted to the corresponding AWS pseudo-parameter expression. The embedded `index.py` Lambda source was verified identical to the deployed inline code at reconciliation time.

- AWS Lambda: `lodestar-akamai-expiration-guard`
- Schedule: EventBridge, every 30 minutes
- Alert topic: `arn:${AWS::Partition}:sns:${AWS::Region}:${AWS::AccountId}:lodestar-expiration-alerts`
- Token location: AWS Secrets Manager secret `lodestar/akamai-token`, **not** in Git
- Eligible instances: label `lodestar-*`, tags `lodestar` and `test`, Akamai region `us-east`, approved instance types, created after activation threshold, age at least 48 hours

The token has been installed and a live, non-destructive Akamai inventory scan was verified. Actual deletion against an expired test Linode is **not** yet verified.

`build_template.py` only refreshes the embedded Lambda source from `index.py`; it makes **no** AWS API calls. Compare resulting CloudFormation changes with the live stack and obtain human authorization before deployment. Never deploy the separate archived Antigravity draft alongside this service.

**Limitations:** The watchdog does not delete detached Linode firewalls, volumes or reserved addresses. Terraform must be reconciled after out-of-band deletion, and a subsequent Terraform apply can recreate deleted resources. Do not treat a dry-run as end-to-end deletion verification.

To run the local token installer again, explicitly supply the expected AWS account ID in `LODESTAR_AWS_ACCOUNT_ID`, and authenticate interactively with `lodestar-admin`. Never commit the token or AWS session credentials.
