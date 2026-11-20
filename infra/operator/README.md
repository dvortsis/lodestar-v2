# Cloud operator: agent and bridge

The operator agent (Managed Agents) and the workflows around it (OP-4). Plan: `docs/plan/2026-11-08-cloud-operator.md` §1–§4. Decisions: `docs/decisions/2026-11-11-OP-4.md`. Nothing here calls an API in CI; every test runs on fakes.

| File | What it does |
|---|---|
| `agent/agent.json`, `agent/system-prompt.md` | The agent: model placeholder (rendered from `OPERATOR_MODEL_ROUTINE` at medium effort), web tools off, the toolset on `auto` except `bash`, which is `always_allow` (the owner's decision, `docs/decisions/2026-11-14-X-operator-bash-allow.md`) |
| `agent/environment.json` | `limited` networking, `api.github.com` only, no package managers or MCP hosts |
| `agent/vault.json` | The vault and its one credential, `GITHUB_TOKEN` (header only, `api.github.com` only, no value committed) |
| `agent_apply.py` | `operator-apply.yml`: create or update the three, IDs in `claude-lock.json` in the control bucket |
| `wake.py` | `operator-wake.yml`: the gates, the ledger ($200 total, 6 an hour, 60 a day, new ids only), the S3 lock, the App token, one session, stall handling, cleanup |
| `ghapp.py` | The App JWT (RS256, standard library) and the installation token: this repo, Pull requests write and Metadata read, or refused |
| `oplock.py` | The operator lock, `locks/operator.json` |
| `message.py` | `operator-instruction v1` and `operator-report v1` |
| `bridge.py`, `apiclient.py` | Shared settings, S3 and rendering; the HTTP clients (errors never carry a body or a token) |
| `tripwire.py` | `operator-tripwire.yml` and the end-of-session sweep: the App's bot acting anywhere but a new comment on the channel PR |
| `thread.py` | Runs in the agent's sandbox: reads the channel through `infra/fleet/reader.py`, posts reports and `/fleet` commands |

`infra/operator/injection/` belongs to OP-5. Tests: `infra/fleet/run_tests.sh` runs them, or `cd infra/operator && python3 -m unittest discover -s tests -t .` (needs `requirements-test.txt` from `infra/fleet`). Any change here or to the `operator-*.yml` workflows gets a fresh Opus review and a `/security-review`.
