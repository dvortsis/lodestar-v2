# The Linux lane on the test VM (X-test-vm)

You are a Claude session running **on** the Linux test VM `claude-dev-<yyyymmdd>` (Ubuntu 24.04, Linode `g6-dedicated-4`: 4 dedicated vCPU, 8 GiB plus 8 GiB swap, 160 GB disk), as the user `lodestar`, under the owner's login through Remote Control. The testing-environment session (`session_01MMjhnBD1KHxsn7QRHHmpeM`) coordinates, and the runner (`session_01EYhgh4JX3AmHzKXPjYCoUg`) owns the build.

**Authority:** the owner's decisions in `docs/decisions/2026-11-12-X-test-vm.md`, and the exception at the top of `.agents/skills/lodestar-fleet/SKILL.md`. You may run commands **on this VM only**. You hold no cloud credentials and need none.

**Rules (all from `docs/plan/2026-11-08-cloud-test-plan.md` ground rules, and `CLAUDE.md`):**
- **Crawl numbers are leads, never CR-SPEC or soak evidence,** and are never pooled with any other run.
- **Content safety:** the child-safety core stays on and unmodified. Metadata only: nothing is downloaded or seeded. uTP stays off unless a step runs it as a separate, labelled leg.
- **Nothing identifying leaves the VM:** no infohashes, torrent names, peer addresses, VM or PIA exit IPs, or logs. Browser artifacts from a crawl-grown store (Playwright traces, screenshots, Lighthouse reports) stay on the VM; run with `--trace off`. Only numbers leave.
- **The crawler goes out only through PIA,** via gluetun's kill switch. **No crawl starts until the leak test passes.**
- **Never** touch the owner's soak or Windows hosts. Never run `terraform` of any kind. Never put a credential in a file outside `/home/lodestar/lodestar/secrets` or `/dev/shm`.
- **No fixed sleeps** in anything you add: wait for a condition with a deadline.
- **Report findings before writing any fix.** Write each result to `~/results/<step>.md` and summarise it in this chat. The coordinator reads it and tells the runner. Don't open PRs and don't push to others' branches.
- **Test `main`:** work in `~/lodestar-v2` on `main`, freshly pulled (`git pull --ff-only`). Record the commit in each result.
- **This runbook and the VM tools live on the X-test-vm branch until it merges.** Read them from a separate worktree, so the code under test stays `main`:
  ```bash
  git -C ~/lodestar-v2 fetch origin claude/dreamy-edison-xlmtoy && git -C ~/lodestar-v2 worktree add ~/testkit FETCH_HEAD
  ```

## 0. Check the machine
```bash
cat /var/lib/devvm-bootstrap.status
```
It should say `ok …`. Record these in `~/results/00-machine.md`:
```bash
nproc; free -g; df -h /; docker version --format '{{.Server.Version}}'; docker compose version; rustup --version; node -v; uname -r
```

## 1. Linux baseline (runner priority 1)
In this order, each with its wall time, pass/fail counts and the names of failing tests:
1. `cargo fmt --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace`. Then re-run each failing test 10 times on its own, to tell a flake from a real failure.
4. In `web/`: `npm ci`, `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`.
5. Playwright: run `npx playwright install --with-deps chromium` once. On this VM that's allowed; the "never playwright install" rule is for the cloud containers. Then run `npx playwright test --trace off`.
6. The Docker image: build it with the repo's Dockerfile (find it under `packaging/`), start it on a fresh data directory, and check `/healthz`. Then stop it. No crawl yet: it isn't behind the tunnel.

Compare against CI's flake list. The runner wants "a non-CI reference".

## 2. Tunnel, leak test, then a bounded crawl (runner priority 2)
1. **The PIA login is the owner's to type.** Ask them, in this chat, to run this once in a terminal on the VM:
   ```bash
   sudo ~/testkit/infra/dev-vm/pia-login.sh
   ```
   It never shows you the login. It writes `/home/lodestar/lodestar/secrets/wg0.conf`, root-only. Never read that file's contents.
2. **The stack:** use `infra/fleet/steps/vm/compose.yaml` and its scripts (`leak-test.sh`, `block-lib.sh`, `start-block.sh`, `watcher.sh`, `outage-watcher.sh`) as their headers describe. They're the reviewed fleet versions, with gluetun pinned by digest and Lodestar in gluetun's namespace. Copy them to `/home/lodestar/lodestar`, and run them as root the way the fleet's step bootstrap does (read `infra/fleet/steps/remote.py` and `run.py` for the exact invocation). Don't edit them. If one doesn't fit this VM, report it.
3. **The leak test** (`leak-test.sh`, plan 0d) must exit 0, with every `ok` line recorded. A failure is a stop condition.
4. **A bounded crawl:**
   - Lodestar's image is the one built in step 1.6 (or the current release candidate), pinned by digest.
   - Use a fresh data directory and the Balanced preset, with every switch at its shipped default and `LODESTAR_DHT_SAFE_RESTART=true`.
   - Accept the Legal Notice and set the admin password through the UI over the loopback port, from this VM. The password goes in a `chmod 600` file.
   - Then run the collector (`docs/soak/tools/linux/collect.py`, with `--rehearsal --baseline-hours 2`, which is never evidence) in gluetun's network namespace, as the plan's Lane L describes.
   - **Duration:** 6 h suppression, 1 h warm-up, then 6 h of observation, about 13 h in all, unless the coordinator says otherwise.
   - **Capture, per minute and as totals:** stored/min (`searchable_new_total{origin="network"}`), fetch success, capacity rejections, peer-command-full, unaccepted evictions, admission grants and refusals, RSS and cgroup `anon`, routing-table size, and `discovery_to_searchable_seconds` p95. These are the numbers CR-ADM's hypothesis turns on.
   - Don't open the UI, Insights or `/metrics` by hand during the window (rule 4).
5. **Failure drills after the window,** outside any measurement:
   - drop the tunnel (the leak test's DOCKER-USER block);
   - restart the Lodestar container;
   - stop gluetun.
   After each, confirm nothing leaves outside the tunnel, and that a restart is `resumed` with its data intact.

## 3. Benchmarks (runner priority 4)
**Only when no crawl is running.** Run `bench/`'s search p95 and commit-latency figures, three repeats each, median. Label each result "g6-dedicated-4, 4 dedicated vCPU, 8 GiB, Linode us-east". Compare with CI's numbers.

## 4. When done
Write `~/results/summary.md` with numbers only. Delete Playwright and Lighthouse reports, and any store snapshot. Tell the coordinator, who deletes the VM and verifies that it's gone.
