# X-gluetun-restart-recover: keep Lodestar working when gluetun restarts

Runner brief, 2026-11-15. Opus, cloud-testing fix slice (testing first, §2.62). Gets an Opus review and a /security-review pass, because it touches the VPN path. Branch `slice/X-gluetun-restart-recover` from current main. PR title `X-gluetun-restart-recover: keep Lodestar working when gluetun restarts`. Target ≲ 1,200 changed lines.

## Why (X-test-vm, AWS Linux, main aa185384, gluetun v3.41.3, PIA WireGuard)
1. **The default health check makes gluetun restart the VPN about every 3 min.** gluetun's small check pings 1.1.1.1 and 8.8.8.8 through the tunnel, and PIA doesn't answer ICMP: 5 restarts in 12 min. With `HEALTH_SMALL_CHECK_TYPE=dns` there were 0 restarts in the next 30 min (3 in the first 20 s after start while DNS warms up). The post-rules weren't the cause: their ICMP DROP counter stayed at 0. Neither the fleet compose (`infra/fleet/steps/vm/compose.yaml`) nor our recipes set it.
2. **After gluetun's container restarts or is recreated (stop/start, an image update), Lodestar is left in the old, dead network namespace with only `lo`.** The container still reads "running", so `restart: unless-stopped` never fires. The API and DHT stay dead until someone restarts Lodestar by hand (`docker restart lodestar` recovers it in 2 s). Confirmed twice. Every user hits this on each gluetun update.

## Owns
- Every gluetun config we generate or ship:
  - `crates/companions/src/compose.rs` and `crates/companions/src/unraid.rs` (gluetun service and template generation), their golden files under `crates/companions/tests/golden/**`, and their tests;
  - `packaging/companions/compose/*gluetun*.yml`;
  - `infra/fleet/steps/vm/compose.yaml`;
  - `infra/akamai/terraform/pia-example/crawler-service.example.yaml`;
  - Lodestar's own Docker/Unraid gluetun recipe files under `packaging/` (find them).
- Lodestar-side recovery code, if the approved plan needs it (see step 2).
- Gluetun-only guide text:
  - `docs/user/03-keep-lodestar-safe.md`: the G2 Compose recipe (~line 135) and the "restarting VPN, again and again" troubleshooting row (~line 255). That row should name PIA (and other providers) not answering ICMP as the common cause, and recommend `HEALTH_SMALL_CHECK_TYPE: dns`;
  - `docs/user/06-*.md` §6.1 gluetun row. Symptom: "the web UI won't load and nothing new is found after gluetun restarts or updates". Give the fix, and say what a manual restart costs today (the start wait) for the released version.
- `docs/decisions/2026-11-15-X-gluetun-restart-recover.md` (new).

Not: §3.3 and §3.7 of 03 (D-guide-backup-fixes owns them), `crates/companions/src/gluetun-post-rules.txt` (don't change the leak rules), anything in the sampler or start-wait code (CR-0w owns it).

## Steps
1. **Health check.** Set `HEALTH_SMALL_CHECK_TYPE=dns` in every gluetun config we generate or ship, with golden updates and a test that fails if it goes missing. Confirm by reading the code that leak protection doesn't depend on the ICMP check: the post-rules and the kill switch are unchanged.
2. **Recovery. Plan first.** Send the runner a short plan before writing recovery code. The runner decides (UX first, least moving parts). Options to weigh:
   - (a) Compose: `depends_on: { gluetun: { condition: service_healthy, restart: true } }` on Lodestar and on every companion that shares gluetun's namespace. Name the minimum Compose version and what older Compose does with the key.
   - (b) Unraid templates and plain `docker run` have no `depends_on`. Options: Lodestar notices it has lost its network (only `lo` and no default route for a sustained period, only when it was started sharing another container's namespace or with an explicit setting our templates set) and exits non-zero with one clear log line, so the restart policy brings it back into gluetun's current namespace; or another mechanism you find. It must never make a native or offline laptop install exit.
   - (c) Whatever covers the fleet VM (its compose and watcher).

   Say which surfaces each option covers. Also say how the user sees it: a log line, the UI's offline state, and one sentence for the guide.
3. Tests for whatever is approved: golden and unit tests; for (b), a test of the detection rule with injected interface/route readers, including the no-exit cases. No fixed sleeps; condition waits with deadlines of at least 10 s. A test cfg-gated to Linux needs a counterpart asserting the behaviour elsewhere.

## Rules
- Workspace fmt, clippy `-D warnings` (Linux and `--target x86_64-pc-windows-gnu`), `cargo test --workspace`. In `web/`, only if you touch it.
- No cloud calls. No test-VM addresses, IPs, account names or instance ids in the repo.
- Follow `.claude/skills/steward/SKILL.md`. `/security-review` before ready. Message the runner "ready <PR> <sha>". The runner merges.
