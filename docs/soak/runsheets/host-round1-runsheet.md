# Experiment host, round 1: three arms at once (the run sheet)

Developer and operator doc. Follow it top to bottom, one step at a time. The operator may be the owner, working alone.

**Rule 0 (owner-decisions §2.68). If a restore step fails, or you can't confirm it worked, stop all further tests and tell the owner at once.** The restore puts back what you paused. Nothing else on the box matters more than that. Section 4 of the stage-1 sheet (`host-stage1-ab-runsheet.md`) is the host-safety procedure: pausing, restoring and the start gates. It applies here unchanged, **per day**: pause at the day's start, restore at its end, or at once on any stop.

## 0. Before you start

- **The spec** is the plan `docs/plan/2026-11-27-revised-path-to-100.md` (§5.2, §6.3, §6.4), with the CR-SPEC round-1 decisions file the runner names. If this sheet and the spec disagree, the spec wins; tell the runner.
- **The declaration is the runner's.** It names every arm, its preset, its 12 switches, its exit, and the image digest, and it's frozen before the rehearsal. You never edit it.
- **This sheet changes no number.** Every threshold is in the spec and in `analyze_cr.py`.
- Write every time in UTC with a `Z`. Never write a local time, a place or an offset.
- **Never write down, send or commit** an address, a server name, a share name, a user name, a credential, or the salt. Use `<round>`, `<stacks>` and similar. The tools record only salted hashes; keep it that way.
- `effect` isn't used in round 1, and nothing here is booked per run (§2.68).

### Words used

- **Arm:** one Lodestar with its own VPN tunnel (gluetun), stack directory and port. Round 1 has up to three: **B** (all-on, Balanced, the first candidate), **A** (all-on, Aggressive) and **C** (all-off control).
- **Exit:** one of three PIA WireGuard servers, **X**, **Y** and **Z**. Each arm uses its own on a given day, and the arms swap exits on day 2.
- **Window:** an arm's measured 24 h, from `ws = T_open + 60 min`. `T_open` is the scrape at which the collector first sees sampling unsuppressed (on this build, within minutes of the start).
- **Final boundary:** the scrape that closes an arm's window. Arms start minutes apart, so they finish minutes apart.
- **Overhang:** the minutes from the first arm's final boundary to the last's. Every healthy arm keeps crawling through it.
- **Attempt:** a two-day block with one layout, named in the declaration (for example `r1-a1`).
- **Coordinator:** `infra/fleet/steps/vm/round1-coordinator.py`. It checks the start gates, records the exits and leak tests, starts the collectors and the host sampler, keeps every arm running through the overhang and stops them all together, and writes each day's archive and the round's manifest.
- **Salt:** a random secret that turns addresses into hashes. It lives only in `/root/round1.salt`, never in the round's folder.

## 1. What runs, in order

| Step | What | Where in this sheet |
|---|---|---|
| 1 | One-time setup: salt, stacks, the coordinator file, identities for the runner | §3 |
| 2 | **Rehearsal** (2 h of each arm on its day-1 exit, inside the 12 h after the comparator) | §5 |
| 3 | **Day 1**: three arms, 24 h windows, staggered starts | §6 |
| 4 | Copy-off, `segment`, read-back; then the exit swap | §7 |
| 5 | **Day 2**: the same, on the day-2 exits | §6 |
| 6 | `gate-round` | §8 |
| 7 | Restart observation, then teardown | §9 |

The layouts (the runner picks one at the rehearsal, and it holds for both days):

| Layout | Day 1 | Day 2 |
|---|---|---|
| L3 | B–X, A–Y, C–Z | B–Y, A–Z, C–X |
| L2 | B–X, A–Y | B–Y, A–X |
| L2b | B–X, C–Y | B–Y, C–X |
| S | the candidate alone, one exit | the candidate alone, another exit |

## 2. The rules that decide what you do

| Rule | Value | Source |
|---|---|---|
| Earliest start of any arm (process and window) | the comparator's window end + 12 h. The coordinator refuses earlier | rule 2.2, §6.3 |
| Every day | fresh data, witness and state directories, new node identities. No restart inside a window | §6.3 |
| Disruptive steps (leak tests, restart drills, the exit swap) | only while **no** arm has a window open | §6.3 |
| Leak tests | each arm's own, before the day's starts and after the coordinator has stopped everything | §5.2 item 3 |
| A failed leak test | stops crawling on that arm (all arms if the cause is on the host) until it's repaired and a re-test passes. Before the day's start it's a note, and the day runs after the passing re-test; after the start, the day is repeated | §6.3; the runner's ruling |
| Shutdown | the coordinator's, at the latest final boundary + 2 min. Never stop an arm yourself | §5.2 item 11 |
| Retry budget | at most two repeated days in the round | §6.3 |
| Never stitch | one process, one window | rule 6.7 |

**Never stop because of what the data shows.** The collector shows no rates while a window is open; don't go looking.

## 3. One-time setup

Do this once, before the rehearsal. Run everything as root on the experiment VM, from a checkout of this repository at the commit the runner names, with Python 3.10 or later.

1. **Pin the build.** The runner gives you the image tag, the source commit and the spec commit. Pull the image by its tag **once**, and read the digest Docker gives it:

   ```bash
   docker pull <image>:<tag>
   docker inspect --format '{{index .RepoDigests 0}}' <image>:<tag>
   ```

   Send the runner that line (`<image>@sha256:<64 hex>`): it goes into the declaration as `image_digest`, and into the coordinator file as `image`. From then on everything runs by that digest, never the tag. Every arm and both days use it.
2. **Make the salt** (once for the round; never copy it into the round's folder):

   ```bash
   umask 077; head -c 32 /dev/urandom | base64 > /root/round1.salt; chmod 600 /root/round1.salt
   ```

3. **Make three stack directories**, one per arm, each with its own WireGuard file from a **different** PIA server:

   ```bash
   for arm in b a c; do
     mkdir -p <stacks>/$arm/secrets
     install -m 0644 infra/fleet/steps/vm/compose.yaml <stacks>/$arm/compose.yaml
   done
   # then put each arm's own wg0.conf in <stacks>/<arm>/secrets/wg0.conf (mode 0600)
   ```

4. **Make the round's folder** `<round>` on the data volume, and the coordinator file `/root/round1-coordinator.json`. The coordinator file stays **outside** `<round>`: it names the stacks, the salt and the password files, and `<round>` is copied off (the coordinator refuses a file inside it). Fill in the `<...>` parts; ask the runner for any you don't know:

   ```json
   {
     "schema": 1, "kind": "cr_round1_coordinator",
     "declaration": "declaration.json", "comparator_report": "comparator.json",
     "salt_file": "/root/round1.salt", "round_dir": "<round>", "data_volume": "<stacks>",
     "image": "<image>@<digest>", "commit": "<source commit>", "uplink_interface": "<uplink interface>",
     "ip_echo_url": "https://api.ipify.org",
     "expected_growth_bytes": {"default": <bytes per arm per 24 h, from the runner>},
     "checklist": ["auto_update=excluded", "backup=excluded"],
     "next_run": ["auto_update=<UTC>", "backup=<UTC>"],
     "arms": {
       "B": {"stack": "<stacks>/b", "suffix": "-b", "port": 8091, "tunnel_target": "<from the runner>", "password_file": "/root/round1-b.password"},
       "A": {"stack": "<stacks>/a", "suffix": "-a", "port": 8092, "tunnel_target": "<from the runner>", "password_file": "/root/round1-a.password"},
       "C": {"stack": "<stacks>/c", "suffix": "-c", "port": 8093, "tunnel_target": "<from the runner>", "password_file": "/root/round1-c.password"}
     }
   }
   ```

   - `declaration` and `comparator_report` are read from `<round>`; the other paths are full paths.
   - **`tunnel_target`** is the address each arm's 10 s tunnel probe pings inside its tunnel. The runner declares it before the rehearsal; the rehearsal checks it answers (§5, step 4).
   - Each **password file** holds the admin password you'll set when you claim that arm's fresh instance each day (§6, step 7). `chmod 600`, outside `<round>` and `<stacks>`.
   - The `next_run` times are the next scheduled runs of the items you excluded (stage-1 sheet, 3.4c).
4a. **Check that every arm has its own VPN server** (the coordinator compares them and prints only ok or fail):

   ```bash
   C="python3 infra/fleet/steps/vm/round1-coordinator.py"
   $C check-endpoints /root/round1-coordinator.json
   ```

5. **Read the identities for the runner** (salted hashes and kernel names only; safe to send):

   ```bash
   $C identity /root/round1-coordinator.json
   ```

   Send the runner the four lines, and the box's vCPU count (`nproc`, which must be 8). It writes `host_identity`, `network_identity.uplink_hash`, `backing_device` and `vcpus` into the declaration, and you put `uplink_interface` in the coordinator file. The declaration is append-only: these are filled once, before the rehearsal, and never change.
5a. **Bind the comparator** once it has stopped (its collector wrote `result.json`), before the rehearsal. Its collector predates round 1, so its report carries no binding; the coordinator binds it now, with the round's salt. First its report, then the binding:

   ```bash
   python3 docs/soak/tools/analyze_cr.py segment <the comparator's evidence folder> --output <round>/comparator.json
   $C bind-comparator /root/round1-coordinator.json --comparator-run <the comparator's evidence folder>
   ```

   It writes `<round>/comparator-binding.json` and prints the comparator's run id (`comparator-<folder name>`). Send the runner that line: the declaration's `comparator.run_id` names it. It refuses a comparator that hasn't finished, or that ended before its window's end, and it never rewrites a binding. Then the runner gives you `declaration.json` for `<round>`.
6. **Disk.** The coordinator checks free space each day (every arm's expected growth plus the round's archives, plus 20 %). If it refuses, ask the runner; never delete an archive to make room.

## 4. Each arm's leak test

The coordinator runs the leak test itself and records the result in the same step. It's the cloud lane's test, run for one arm: it recreates **only that arm's** gluetun and touches **only that arm's** rules, so the other arms' tunnels keep running. It runs from a fresh private folder that is deleted afterwards (its packet captures hold raw addresses). **Test the arms one at a time**: the coordinator refuses a second leak test while one is running.

```bash
$C leak /root/round1-coordinator.json --day-dir <day dir> --attempt <attempt> --day <1|2> --arm B --phase before
```

It prints `ok leak_test` or `fail leak_test` with the arm and phase, and keeps the test's own lines (fixed names and counts, no addresses) as `leak-<arm>-<phase>-<n>.log` in the day folder. It takes a few minutes per arm.

- `--phase before` before the day's starts; `--phase after` after the coordinator has stopped the day; `--phase retest` after a repair.
- **The coordinator refuses a leak test while any arm is still measuring.** Leak tests never run inside a window.
- **If the test refused before testing anything** (its failures are only `args_valid`, `block_running`, `wg_conf_present`, `endpoint_valid` or a missing tool), the coordinator prints `skip leak_test` and records nothing. Fix what the log names and run it again.
- **On a fail,** the coordinator stops that arm's Lodestar and blocks it. If the cause is on the host (the VM's network, the firewall, Docker), add `--host-level` and it blocks every arm. Tell the runner. Nothing starts that arm again until a `--phase retest` passes.
  - **Before the day's collectors have started,** nothing was measured: after the repair and the passing re-test, carry on with the same day folder. If the repair changed a VPN server, run `bring-up` again: the archive keeps both exit checks.
  - **After they started** (an `after` test), it's an infrastructure fault: the whole day is repeated from fresh stores in a new `day<n>-repeat<k>` folder (§6.3).
- The log stays in the day folder; it holds no address (the test prints fixed names only).

## 5. The rehearsal (inside the 12 h after the comparator; never a measurement)

The rehearsal checks the setup and the timing. Its stores are deleted afterwards, and it never enters the manifest.

1. Day folder `<round>/rehearsal`. Make each arm's `data` and `witness` folders empty.
2. Leak tests for every arm on its **day-1** exit, recorded with `--rehearsal`:

   ```bash
   D="/root/round1-coordinator.json --day-dir <round>/rehearsal"
   $C leak $D --attempt <attempt> --day 1 --rehearsal --arm B --phase before
   $C leak $D --arm A --phase before
   $C leak $D --arm C --phase before
   ```

3. Exits and start:

   ```bash
   $C bring-up $D --attempt <attempt> --day 1 --rehearsal
   $C up $D
   ```

4. **The tunnel probe answers.** For each arm, ping its `tunnel_target` from inside its tunnel and check you get replies (`<probe image>` is the `PROBE_IMAGE` digest in `infra/fleet/steps/vm/block-lib.sh`):

   ```bash
   docker run --rm --network container:gluetun-b --cap-drop ALL <probe image> ping -c 3 <B's tunnel_target>
   ```

   No replies: stop, and tell the runner before anything else. Without them the contention check can't be judged.
5. Start the collectors and sampler, start the watch in `tmux` straight away, then **claim each fresh instance** (step 8 of §6):

   ```bash
   $C collect $D
   tmux new -d -s round1-rehearsal "$C watch $D"
   ```

6. **It ends** when every arm has had 60 active minutes in the rehearsal's last hour (the runner reads it), or at 4 h. The collectors close after 2 h windows and the coordinator stops everything at the last one + 2 min. At 4 h, if it's still running, stop the watch's `tmux` session, then stop every arm with the coordinator, and tell the runner:

   ```bash
   tmux kill-session -t round1-rehearsal; $C stop $D
   ```
7. **Leak tests after**, for every arm, recorded with `--phase after`.
8. **Read the rehearsal's tunnel medians** (the base for the network threshold), and keep its samples file:

   ```bash
   python3 docs/soak/tools/linux/host_samples.py median --samples <round>/rehearsal/host-samples.jsonl
   ```

   Send the runner the medians line.
8a. **Check the uplink's drop counters stayed flat** over the rehearsal (any rise breaches the "no uplink drops" rule every hour):

   ```bash
   python3 docs/soak/tools/linux/host_samples.py drops --samples <round>/rehearsal/host-samples.jsonl
   ```

   It prints one line per hour with each series' rise (`uplink_rx_dropped`, `uplink_tx_dropped`, and the card's own `rx_missed_errors` and `rx_fifo_errors` where it has them). Send the runner the lines. If any is above 0, tell the runner before day 1: some network cards count harmless frames as drops, and the series may need changing.
8b. **Each arm's settings, as the declaration says.** Judge each rehearsal arm against the declaration (it reads the switch gauge the arm reported):

   ```bash
   for arm in B A C; do
     python3 docs/soak/tools/analyze_cr.py segment <round>/rehearsal/$arm --declaration <round>/declaration.json \
       --output <round>/rehearsal/$arm.report.json
   done
   ```

   Every check named `declared_*`, `effective_switches_recorded` and `safe_restart_on` must read ok (a rehearsal is never a measurement, whatever its verdict). If one doesn't, stop and tell the runner.
8c. **The drop-C rule.** Run it on the rehearsal and send the runner its output:

   ```bash
   python3 docs/soak/tools/analyze_cr.py drop-c <round>/rehearsal/host-samples.jsonl \
     --arm B=<round>/rehearsal/B --arm A=<round>/rehearsal/A --arm C=<round>/rehearsal/C
   ```

   It prints `keep` or `drop` with its numbers (the last 60 min once every arm crawled; host CPU p95 over 70 % or steal p95 over 3 % drops C). **The runner selects the layout** from it and tells you.
9. Delete each arm's rehearsal `data` and `witness` folders. Keep `<round>/rehearsal` itself.

## 6. A day, step by step

Day folder: `<round>/<attempt>/day<1|2>`. A repeated day gets a new folder `day<n>-repeat<k>` (k = 1, then 2), and its **first** command (the first `leak`) carries `--repeat <k>`; later commands read it from the folder. Pause the backup and parity schedules as the stage-1 sheet's 4.1 says, and write the restore time down.

1. **Fresh folders.** Each arm's `data` and `witness` folders are empty (day 2: only after §7 is done).
2. **Leak tests before**, every arm on today's exit (§4), each recorded:

   ```bash
   D="/root/round1-coordinator.json --day-dir <round>/<attempt>/day<n>"
   $C leak $D --attempt <attempt> --day <n> --arm B --phase before   # a repeated day: add --repeat <k> here
   $C leak $D --arm A --phase before
   $C leak $D --arm C --phase before
   ```

3. **Exits.** The coordinator reads each arm's address inside its own tunnel and keeps only the hash, by exit label. It refuses two arms on one exit, or an arm whose traffic leaves outside its tunnel:

   ```bash
   $C bring-up $D
   ```

   Run it after the leak tests: `check-start` refuses an exit check older than any arm's latest leak test.

4. **Start gates** (spacing, disk budget, leak tests, fresh folders). `up` runs them again itself:

   ```bash
   $C check-start $D
   ```

5. **Start the stacks** with the declared settings (every switch set explicitly, `safe_restart` on, Aggressive only for A):

   ```bash
   $C up $D
   ```

6. **Start the collectors and the sampler at once** (they must see the start-up). `collect` waits up to a minute for each collector to start; if one exits, it says so and names its log:

   ```bash
   $C collect $D
   ```

7. **Start the watch straight away** in `tmux`, and leave it running until it ends by itself (about 25 h):

   ```bash
   tmux new -d -s round1-day<n> "$C watch $D"
   ```

   It scrapes each arm every minute, waits for every collector to close its own window, keeps every healthy arm crawling until the latest final boundary + 2 min, then stops every stack and the sampler and writes `archive.json`. A collector that dies without finishing counts as closed, so the day still ends (and fails closed).
8. **Claim each fresh instance,** within the first minutes, for every arm (through `ssh -L <port>:127.0.0.1:<port>` to the VM):
   1. Read that arm's newest setup token: `docker logs lodestar-<arm> 2>&1 | grep 'setup token' | tail -1`. It stays private.
   2. In the browser, accept the Legal Notice and set the admin password to **that arm's password file's contents**.
   3. Save no other settings.
   4. Write the claim time (UTC) in the run record.
9. **During the day:** touch nothing. Write a host-activity note for any event, in each running arm's evidence folder (`python3 docs/soak/tools/linux/collect.py note --root <day dir>/<arm> --category <category>`). Don't read rates.
10. **After the watch ends:** leak tests after, every arm, `--phase after`. Then rewrite the archive with them in it:

    ```bash
    $C archive $D
    ```

11. **Restore** the schedules (stage-1 sheet, 4.1). Check it worked. Rule 0 applies.

### If something goes wrong during a day

| What happened | What you do |
|---|---|
| A leak test failed | §4: the coordinator stopped the arm. Tell the runner. Before the day's start: it's a note; repair, pass a `--phase retest`, and the day runs. After the start (an `after` test): the day is repeated from fresh stores |
| An arm's collector stopped early (its `result.json` names a reason other than `baseline_window_complete`) | Leave the others running: the watch carries on and records the event. Tell the runner, who diagnoses it with a fresh reviewer |
| The watch died, or your session dropped | Start the watch again at once with the same command. Inside a window, the collectors' own scrapes cover the gap. In the overhang, a gap of over 3 min ends the arms' recorded activity, so later candidates may not be judged: tell the runner the times. If every window has closed and the overhang has passed, it stops everything at once |
| The host rebooted or lost power | The day is invalid. Round 1's stacks never restart by themselves, so no arm crawls untested after a reboot. Leave them down. Write a note when you can, restore, and tell the runner. The repeated day starts as any day does: leak tests, `bring-up`, then `up` |
| A parity operation, sync, rebuild or mover run | Write the note in each running arm's evidence folder, saying what ran and on which pool: `collect.py note --root <day dir>/<arm> --category array_operation --kind <mover, parity or other> --pool <array, or the pool's name>`. A mover run or parity check on the array doesn't void a window whose `/data` is on `ssd_apps` (§8 item 6, D3); a sync or rebuild is `--kind other` and does. Tell the runner either way |

## 7. After each day, before the exit swap

1. **Each arm's segment report,** against the declaration. List the layout's arms for today (L3: `B A C`; L2: `B A`; L2b: `B C`; S: the candidate):

   ```bash
   for arm in <the layout's arms>; do
     python3 docs/soak/tools/analyze_cr.py segment <day dir>/$arm --declaration <round>/declaration.json \
       --output <day dir>/$arm.report.json
   done
   ```

2. **The day's host samples,** with the rehearsal's medians:

   ```bash
   python3 docs/soak/tools/linux/host_samples.py assemble --samples <day dir>/host-samples.jsonl \
     --device <the declaration's backing_device> --rehearsal <round>/rehearsal/host-samples.jsonl \
     --archive <day dir>/archive.json --out <day dir>/host-samples.json
   ```

   It refuses samples taken on another device than the declaration's. Every hour from the sampler's start to the shutdown is listed with its coverage; tell the runner about any hour under 98 %.

3. **Copy off** the whole `<round>` folder (never the salt, a password file, `<stacks>` or the coordinator file, all of which live outside it) to the place the runner names, then **read it back**: on the copy, run step 1's `segment` command for each arm with `--output <a new path outside the copy>` (it never overwrites a file) and compare the verdict line with the original's. Only then may the day's `data` folders be emptied.
4. **Then the exit swap:** put each arm's day-2 `wg0.conf` in place (the layout's day-2 column), and run `check-endpoints`.

## 8. After day 2: the verdict

```bash
python3 infra/fleet/steps/vm/round1-coordinator.py manifest /root/round1-coordinator.json
python3 docs/soak/tools/analyze_cr.py gate-round <round>/manifest.json --output <round>/verdict.json
```

- `manifest` refuses a report on another image, a repeated run ID or a gap in repeats: tell the runner, don't edit a file.
- If an attempt ended short (the runner says so), record it first: `$C end-attempt /root/round1-coordinator.json --attempt <attempt> --reason <reason>`.
- Send the runner `verdict.json` and the manifest's SHA-256 line. **The verdict and what follows are the runner's.**
- **Keep every raw folder** (both days, the rehearsal's samples) until the runner has reproduced `gate-round` from the copy.

## 9. After day 2's windows: restart observation and teardown

1. Only after day 2's windows have closed, and only if the runner asks: the restart drill on each candidate (§6.6). The runner gives the steps.
2. **Teardown** only when the runner confirms `gate-round` reproduced from the archive: then delete the stores, the stack folders' data and the salt.

## 10. Run record (one per day)

| Item | Value |
|---|---|
| Day, attempt, repeat | |
| Layout | |
| Image digest, source commit, spec commit | |
| Tool hashes (`sha256sum docs/soak/tools/linux/collect.py docs/soak/tools/linux/host_samples.py docs/soak/tools/analyze_cr.py infra/fleet/steps/vm/round1-coordinator.py`) | |
| Pause time, restore due, restore done (UTC) | |
| Leak tests before (per arm, pass or fail, UTC) | |
| Bring-up (UTC), each arm's exit label | |
| Claims (per arm, UTC) | |
| Watch started (UTC); stacks stopped (UTC, from `archive.json` `shutdown_utc`) | |
| Leak tests after | |
| Events and notes | |
| Copy-off done and read back (UTC) | |
