# CMP-G (Opus; runner brief 2026-11-28)

**Branch:** `slice/CMP-G`. **PR title:** `CMP-G: Companion gate: checksum and signature refusal, localhost binding, egress and kill-switch, Needs setup, credentials, orphans, install smoke`.

## Why
The v0.6 tag needs the crawler, sources, workers and companions all green. This is the companion manager's gate (architecture §18.10, §18.9, §12; `runner/slices.yaml` entry CMP-G). All its dependencies have merged, and owner question cq7 is answered.

## Task
Write the gate as end-to-end tests in `crates/companions/tests/gate.rs`, using a test catalogue in `crates/companions/tests/data/test-catalogue/**`. Each gate item is a test that would fail if the guarantee broke:
1. **Checksum and signature refusal:** a companion whose download fails its checksum, or whose catalogue entry fails its signature, is refused, and nothing is installed or run.
2. **Localhost binding:** managed companions bind to localhost only, unless the catalogue entry and the user's setting say otherwise.
3. **Egress and the kill switch:** a companion's egress follows its setting (direct, the SOCKS5 or the VPN path, per X-cmp-egress-api). With the VPN path chosen and the tunnel down, the companion has no route out.
4. **Needs setup:** a companion missing required configuration shows Needs setup and is not started.
5. **Credentials:** secrets are stored as the credentials module requires and never appear in logs, the API or compose output.
6. **Orphans:** a companion process or container left from an earlier run is found and handled as the spec says.
7. **Install smoke:** `.github/workflows/companions-smoke.yml` installs a test companion from the test catalogue on each supported runner OS, checks it, and removes it.
8. **No override in release builds:** a release build has no catalogue override (the slice note). Assert it.

Read the CMP-* slices' decisions files for what each guarantee actually is today. If a guarantee is missing or broken in the product, don't fix it in this PR. Tell the runner, with the test that shows it, and the runner opens a fix slice.

## Owns
- `crates/companions/tests/gate.rs`
- `crates/companions/tests/data/test-catalogue/**`
- `.github/workflows/companions-smoke.yml` (inside `ci-ok` only if the runner agrees; ask first)
- `docs/decisions/2026-11-28-CMP-G.md`

Ask the runner before touching anything else.

## Rules
- Run `/security-review` on the diff, and do the unanchored second read the slice note asks for.
- Tests use no network beyond localhost, and no fixed sleeps: condition waits have deadlines of at least 10 s.
- No cloud calls. Never touch the owner's machines.
- No model identifiers in commits, PRs or code. Never merge.

## Done
- CI green.
- Self-review per `.claude/skills/steward/SKILL.md`.
- Send the runner `ready <full sha>` with the claude-code-remote send_message tool.
