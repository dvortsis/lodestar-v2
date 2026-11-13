# Review: PR #797 (WK-18) at b8f0a5bc

**VERDICT: CHANGES** (one cheap text fix, in owned files)

## Blocking
**B1. The join steps fail against a default main Lodestar.** Files: `packaging/docker/compose.worker.yml:4-15` and `packaging/unraid/lodestar-worker.xml:13`.
- The steps say "the worker only has to reach the main instance's cluster port (8091/tcp)".
- Neither main template publishes 8091. `docker-compose.yml` maps only 8090/tcp and 6881/udp+tcp, and `lodestar.xml` has no 8091 port. The port is also unbound until "Allow workers" is on (plan Q3, `docs/operations.md:28`).
- A reader who follows these steps on Docker or Unraid can't connect. That is "a step that needs knowledge the page doesn't give" (`runner/standards.md` §8).
- **Fix:** add a step 0 to both. In the main Lodestar, turn on Allow workers (Settings, Workers), and publish `8091/tcp` on its container: `- "8091:8091/tcp"` in its compose, or a Port 8091 in its Unraid template. Keep it on the LAN or a private link.
- **Tell the runner:** `docker-compose.yml` and `lodestar.xml` need an 8091 mapping, commented or advanced. That's outside WK-18's paths.

## Important
**I1. WiX was dropped from the slice.**
- `packaging/wix/join.wxs` isn't built. `docs/decisions/2026-11-12-WK-18.md:19-29` defers it to WK-18w as "runner's call".
- The findings are verified:
  - `read_join_token_file` (`crates/cluster/src/join.rs:232`) only opens the file and never deletes it.
  - `plan()` (`worker.rs:100`) always joins when `--token-file` is given.
  - The WiX sources are in `packaging/windows/`.
- The design follows the brief: a one-shot SYSTEM action, then delete the file, then the service runs plain `worker`.
- **Fix (runner):** add WK-18w to `runner/slices.yaml` and its brief, and drop `packaging/wix/join.wxs` from WK-18's `owns`. The v0.6 tag needs it (plan §6). In WK-18w:
  - set the ACL with an explicit protected DACL (SYSTEM and Administrators, `inheritance removed`), reusing `secure-datadir.ps1`;
  - mark the property `Hidden="yes"`;
  - pass the token to the CLI by file, never through a cmd or PowerShell string.

**I2. The decisions-file heading misses the PR number.** `docs/decisions/2026-11-12-WK-18.md:1`. The README requires `# WK-18 decisions (#797)` and the architecture sections each decision touches. **Fix:** retitle it, and cite §19.6 and plan Q2/Q3.

## Optional
- **O1.** `compose.worker.yml:37`. `LODESTAR_TOKEN: ${LODESTAR_TOKEN:-}`, plus `LODESTAR_TOKEN=… docker compose up -d` once, keeps the token out of the file. A later plain `up -d` recreates the container with the token blank, which the CLI treats as unset, so it also leaves `docker inspect`. Keep the remove-after comment.
- **O2.** `compose.worker.yml:71`. `./join_token.txt` resolves inside the git checkout (`packaging/docker/`). Suggest a path outside the repo, for example `/etc/lodestar/join_token`, so it can't be committed.
- **O3.** `compose.worker.yml:39-40`. The PUID/PGID lines lack the main compose's explanation: default 65532 is the safe choice, never 0. Copy it.
- **O4.** `compose.worker.yml:22`. Consider `security_opt: ["no-new-privileges:true"]` (it doesn't block the setuid drop), for parity with a future main-compose change.
- **O5.** Disabling the healthcheck is correct: the worker binds no HTTP and `healthcheck()` (`main.rs:385`) probes `/healthz`.

## Security
- **Token not in image, argv or logs: yes.**
  - Compose and Unraid have placeholders only: `paste-one-time-token-here`, and `Default=""` with `Mask="true"` in Unraid.
  - Token parse errors never echo the value (`TokenFormatError` Display).
  - `prepare()` re-execs without `LODESTAR_TOKEN` once joined, so it leaves `/proc/<pid>/environ`.
- **Token file: OK.** A compose `file:` secret is a bind mount. The comment gives `chown 65532` and `chmod 600`.
- **Least privilege: OK.** No caps, no privileged mode, bridge network. Only `6882:6881/udp` is published, and the data volume is separate from the main one (`lodestar-worker-data`). Root isn't read-only, but neither is the main service's.
- **Image:** `:latest`, the same as the main compose. That keeps worker and coordinator versions in step, so it's acceptable.
- **WiX checks** (hidden property, ACLs, no shell custom action): not applicable, since no WiX code ships.

## Checks run
- `validate.py`: all three templates ok. `unittest` in `packaging/unraid`: 5 tests ok (#781 not merged).
- `docker compose -f packaging/docker/compose.worker.yml config -q`: ok. It also passes with the token-file alternative uncommented as described (the command and secrets resolve).
- XML well-formed. `git diff --check`: clean.
- No shell scripts or tests added (no shellcheck target, no sleeps).
- **Ownership:** only the compose file, the Unraid template and the decisions file changed. No crates, no Cargo changes.
- **Hygiene:** no 12-digit numbers, no model identifiers, no local times in the diff. Commit trailers are the harness's.
