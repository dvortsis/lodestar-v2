# PIA delivery (OP-2b)

The owner's PIA broker, made unattended: `fleet.yml`'s `deliver-pia` job delivers the dedicated test PIA login to the fleet's Linodes over SSH, and a committed step on each VM turns it into a root-only WireGuard configuration for gluetun. The Linodes get no AWS credentials, and the operator never sees the login.

Plan: `docs/plan/2026-11-08-cloud-operator.md` §3–§4. Decisions: `docs/decisions/2026-11-09-OP-2b.md`.

| File | What it is |
|---|---|
| `deliver.py` | The `deliver-pia` job's program: the gates again, `fleet.json` (targets and pinned host keys), the two secrets, the job's own firewall `/32`, SSH with the step on stdin, and the `/run` clean check |
| `vm/pia-wireguard.sh` | The VM step: PIA's manual-connections flow, `wg0.conf` written root-only, and the login deleted in an EXIT/HUP trap |
| `vm/pia-ca.rsa.4096.crt` | PIA's CA (from `github.com/pia-foss/manual-connections`), pinned by a test |
| `tests/` | Fakes for S3, the Linode firewall API, the Actions API and SSH; the VM step against stub `curl` and `wg`; one end-to-end run of the real bootstrap and step |

**Reading a failure by hand.** `deliver-pia` discards the step's output, so only its exit code reaches the job. Run by hand, two stages also print one line to stderr: stage 13 which check failed, with the rejected certificate name (it must be a DNS hostname; capitals are allowed and lowercased), and stage 15 addKey's HTTP status (`000` for no reply) with PIA's `status` and `message`. Both lines are cut short and stripped of control characters, and neither prints the token, the key or PIA's reply.

Run the tests with `infra/fleet/run_tests.sh` (CI's infra job runs it). Nothing here calls a network in a test. Every change here gets a fresh Opus review and a `/security-review` (plan §3).
