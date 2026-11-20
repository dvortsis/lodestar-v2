# X-testvm-desc-limit decisions (#990)

Fixes two faults in `infra/dev-vm/aws-claude-testvm.json` that kept main's template from working. Both showed only once the owner deployed it on 2026-11-20Z. Neither loosens an owner decision.

- **The template's Description is cut to 944 characters.** CloudFormation refuses a template whose Description is over 1,024 characters before it creates any change set. The #864 review rounds had grown it to 1,262. A test now pins CloudFormation's and IAM's length limits.
- **The launch policy no longer names `ec2:MetadataHttpProtocolIpv6`.** IAM has no such condition key; EC2's keys are listed in the Service Authorization Reference. A `StringEquals` on a key the request never carries is false, so `RunOnlyTheGuardedInstance` allowed nothing. Every launch was refused, on both platforms.
  - This is not a loosening: the condition never enforced anything.
  - `HttpTokens: required` and a hop limit of 1 are still required by the policy (`docs/decisions/2026-11-12-X-test-vm.md`). They are what keep containers and other processes from the role.
  - The runbooks still launch with `HttpProtocolIpv6: disabled`, as a rule for the sessions.
  - A test now checks every condition key in the template against a list verified against the reference. A new key must be added to that list on purpose.
- **CI's `infra` job now runs `infra/dev-vm/tests`.** No job ran them before, which is how both faults reached main.
