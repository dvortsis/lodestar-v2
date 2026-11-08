# Fleet control: AWS stacks

The AWS side of the cloud fleet control plane (OP-2a). Plan: `docs/plan/2026-11-08-cloud-operator.md` §3–§4 and "Owner setup" step 4. Decisions: `docs/decisions/2026-11-08-OP-2a.md`.

**The owner deploys these, never CI or an agent.** Review a change set before each create or update.

| Template | Account | Creates |
|---|---|---|
| `fleet-control.json` | the fleet's AWS account | the GitHub OIDC provider (unless one exists), `LodestarFleetOperator`, `LodestarOperatorBridge`, the state, evidence and control buckets, the empty `lodestar/akamai-fleet-token` secret, the unattached `LodestarFleetDenyAll` policy |
| `pia-delivery.json` | the account that holds `lodestar/pia-vpn` | `LodestarPiaDelivery`, the empty `lodestar/akamai-firewall-token` secret, the unattached `LodestarPiaDenyAll` policy |

Both use named IAM resources, so a deploy needs `CAPABILITY_NAMED_IAM`. Neither holds an account ID: they use AWS pseudo-parameters and the parameters below.

## Deploy, in order (us-east-1)
1. **`fleet-control.json`.** Parameters:
   - `ExistingOidcProviderArn`: leave empty, unless the account already has a `token.actions.githubusercontent.com` provider (an account holds one per URL); then give its ARN.
   - `PiaDeliveryRoleArn`: leave empty for now.
2. **`pia-delivery.json`.** Parameters:
   - `ExistingOidcProviderArn`: as above, for this account. If it's the same account as step 1, give the provider step 1 created.
   - `PiaSecretArn`: the exact ARN of `lodestar/pia-vpn`, with its six-character suffix.
   - `StateBucketName`: step 1's `StateBucketName` output.
3. **Only if the two stacks are in different accounts:** update the `fleet-control` stack with `PiaDeliveryRoleArn` set to step 2's `PiaDeliveryRoleArn` output. That grants the role `s3:GetObject` on `fleet/fleet.json` alone.
4. Put the token values into the two empty secrets in the console (owner setup steps 5 and 6.3), never in a parameter, file or chat.
5. Copy the outputs into the GitHub environment secrets each output's description names.

## Build and test
The JSON is generated: edit `build_templates.py`, then run it. CI fails when the committed JSON differs from what the builder makes.

```
python3 infra/aws/fleet-control/build_templates.py
python3 -m unittest discover -s infra/aws/fleet-control -p 'test_*.py'
```
