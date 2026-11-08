# Fleet control plane

`fleet.yml`'s code (OP-1, **dry-run**: no cloud call). Plan: `docs/plan/2026-11-08-cloud-operator.md` §2–§4. Decisions: `docs/decisions/2026-11-08-OP-1.md`.

| File | What it does |
|---|---|
| `fleet.py` | Entry point: `gate`, `refuse`, `act`, `reap`, `dispatch-down` |
| `command.py` | The strict `/fleet` parser |
| `gates.py` | Channel PR, `created` only, the bot's numeric ID, `OPERATOR_STOP` (not for `down`) |
| `reader.py` | The operator's thread reader: unedited comments from allowed author IDs only |
| `lock.py` | The fleet lock: conditional writes, busy, reaping marker, fencing |
| `state.py` | `fleet.json` and the dry-run backend |
| `limits.yaml`, `limits.py` | One fleet, the `screen` preset, 44 h, the $40 monthly cap |
| `reaper.py` | Expiry, the 15-minute emergency path, the 44 h age reap, the audit hook |
| `status.py`, `comments.py` | The status comment (no labels, addresses, fingerprints or IDs) |
| `ancestry.py` | Commit arguments must be ancestors of `main` |

Run the tests: `pip install --no-deps --require-hashes -r requirements-test.txt && ./run_tests.sh` (the hashes are for CPython 3.13 on Linux x86_64; elsewhere, install the same versions without `--require-hashes`).

`steps/` belongs to OP-3 and `apply.py` to OP-2. Any change here or to `fleet.yml` gets a fresh Opus review and a `/security-review`.
