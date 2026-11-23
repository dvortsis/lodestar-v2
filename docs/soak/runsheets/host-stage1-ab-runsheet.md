# Experiment host, stage 1: the A/B run sheet (CR-AB-ADM, then CR-AB-4)

Developer and operator doc. Follow it top to bottom, one step at a time. The operator may be the owner, working alone.

**Rule 0 (owner-decisions §2.68). If a restore step fails, or you can't confirm it worked, stop all further tests and tell the owner at once.** The restore puts back what you paused. Nothing else on the box matters more than that.

## 0. Before you start

- **Use this sheet only after the owner says the experiment host is back** (owner-decisions §2.82). Until then it is a plan, and the cloud's runs are provisional. Only the host's runs count as the gain.
- **Nothing here is booked per run** (§2.68): the owner's standing permission covers it. The permission covers pausing **Appdata Backup** and a **scheduled parity check**, and nothing else.
- **Never** cancel a parity check that is already running. **Never** pause a parity sync, a rebuild or a disk recovery: ask the owner.
- **Never** change a schedule, a setting or any data on the box other than the steps below.
- **This sheet never changes a number.** The design and thresholds are the owner's CR-SPEC (`docs/decisions/2026-11-02-CR-SPEC.md`, "the spec"), amended by r5.1 (`docs/decisions/2026-11-20-path-to-100-cr-spec.md`). If this sheet and the spec disagree, the spec wins; tell the runner.
- Write every time in UTC with a `Z`. Never write a local time, a place or an offset.
- Write no address, share name, user name or credential in anything you send or commit. Use `<host>`, `<appdata>` and similar.

### Words used

- **Segment:** one process run with one arm on, 6 measured hours after its warm-up.
- **Block:** four segments in one order on one fresh data directory. **ABBA** is control, treatment, treatment, control. **BAAB** is the reverse.
- **A** is the control (the treatment's switch off). **B** is the treatment (on).
- **T_open:** the first scrape at which sampling is no longer suppressed. The collector records it.

## 1. What runs, in order

| Step | Run | A (control) | B (treatment) |
|---|---|---|---|
| 1 | **CR-AB-ADM**, block 1, ABBA | shipped defaults | `dht.coordinated_admission` on |
| 2 | CR-AB-ADM, block 2, BAAB, about 12 h out of phase with block 1 | same | same |
| 3 | **CR-AB-4, the uTP arm**, block 1, ABBA | shipped defaults | `dht.utp` on |
| 4 | CR-AB-4 uTP, block 2, BAAB | same | same |
| 5 | **Route backoff arm: only if** the baseline shows `fetch_no_route_total{family="ipv6"}` above 0 | | `dht.route_backoff` on |

- The two treatments run **back to back** (r5.1 A3), apart from the wait in the next bullet when a backup or parity check is due. Start step 3 as soon as step 2's last segment closes. Block 2 of each treatment should start about 12 h out of phase with block 1 (CR-SPEC rule 6.1): if the next block would start about 10 h out of phase, shift it by waiting, and write the wait in the run record.
- **The pause (section 4.1) is per block:** pause at a block's start, restore at its end, or at once on any stop. Between blocks, a backup or scheduled parity check that is due or overdue runs and finishes before the next block starts. See section 4.1.
- **Stage 1 keeps the shipped-default control** for both treatments. There is no `--winners` file in stage 1 (r5.1 A2).
- **Every other crawler switch stays at its shipped default (off)** in every block. The one exception is `dht.safe_restart`, which is `true` in every segment of a block, in both arms (spec rule 2.3).
- **Step 5 is decided from data, not from this sheet.** After both treatments' blocks are in, run `analyze_cr.py route-baseline` on **every stage-1 segment report** (all CR-AB-ADM segments, both arms, and all CR-AB-4 segments). The analyzer picks the segments with `dht.route_backoff` and `dht.utp` both off (the spec's rule 5.3); leaving any report out can only lower the sum. If the ipv6 no-route sum is 0, route backoff is a **recorded skip** (the runner writes a decisions file). On the default IPv4-only bind, 0 is the expected result. Don't run step 5 unless the runner says so.
- **Two blocks are the minimum before any outcome.** If the outcome is inconclusive, the runner asks for two more blocks (ABBA then BAAB again). Never run one extra block on its own: extras come in pairs.
- **An outcome is the runner's.** You collect and analyze; you don't decide whether a switch wins.

## 2. The design in numbers

"CR-SPEC" is `docs/decisions/2026-11-02-CR-SPEC.md`; "r5.1" is `docs/decisions/2026-11-20-path-to-100-cr-spec.md`.

| What | Value | Source |
|---|---|---|
| Measured window per A/B segment | 6 h | CR-SPEC rule 1.3 |
| Warm-up after `T_open` | 60 min (`ws = T_open + 60 min`) | CR-SPEC rule 1.1 |
| First segment of a block (fresh directory) | sampling held 6 h after start, so `ws` is about start + 7 h | CR-SPEC rule 1.1 |
| Later segments (resumed restart) | `ws` is about start + 60 min | CR-SPEC rule 1.1 |
| No `T_open` within 7 h of the process start | the segment is invalid | CR-SPEC rule 1.1 |
| Boundary scrape | first scrape at or after the hour, at most 90 s late | CR-SPEC rule 1.4 |
| Scrape gap | two scrapes more than 180 s apart is missing evidence | CR-SPEC rule 6.6 |
| Arm switch | a clean stop, then a start with only the treatment's switch changed, within 10 min. The analyzer counts from the previous segment's **last scrape** to the next process start | CR-SPEC rule 2.3; `analyze_cr.py` |
| Data directory | fresh per block, never per segment | CR-SPEC rule 2.1 |
| `dht.safe_restart` | `true` in every segment, both arms | CR-SPEC rule 2.3 |
| Witness directory | on the host, outside the data directory, fresh per block (`LODESTAR_DHT_WITNESS_DIR`) | CR-SPEC rule 2.3 |
| Preset | Balanced | CR-SPEC rule 5.5 |
| Blocks per treatment | at least two: ABBA, then BAAB about 12 h out of phase | CR-SPEC rule 6.1 |
| Block length | about 34 h (6 h hold, warm-ups, four 6 h windows) | r5.1, "Calendar" |
| Outcome | win: CI lower bound above 0 and at least +5 %; no effect: CI within ±5 %; loss: CI upper bound below 0; else inconclusive | CR-SPEC rule 6.3 |
| Re-runs | an invalid or void segment invalidates its block; re-run the block from a fresh directory, same order. Never stitch | CR-SPEC rule 6.7 |
| Retry budget | about 30 % | build-plan §0.7 |

**If the pinned build has no start hold** (the "no sampling wait" work of owner-decisions §2.77 had not merged when this sheet was written, so don't assume it has): rule 1.1's formula still holds. `T_open` comes from what the collector records, `ws = T_open + 60 min`, and the block is only shorter. The design doesn't change.

### What the spec doesn't say

**The spec has no early-stop rule.** It says what makes a segment invalid (rule 6.5) and when a block is re-run (rule 6.7), and nothing about ending a block early because of what the numbers look like. So:

- **Finish every segment** unless a collector guard trips or a host-safety rule in section 4 says stop.
- **Never stop because of what the data shows.** The collector shows no rates while a window is open, and you don't go looking (the no-peeking rule, CR-SPEC rule 6.11 and r5.1 A3). Rates are seen only after a window ends.
- A stopped or invalid segment means the block is re-run, not continued.

## 3. Before the day

Do these when the owner says the box is back, **before** the first start. Record each answer in the run record (section 12).

0. **Where things run.** Use a checkout of this repository at the commit the runner names, on the host or a machine that can read the evidence folders, and Python 3.10 or later (stock Unraid may not have it; ask the runner if it is missing). Run the collector **detached** (`tmux`, `screen` or `nohup`): if your shell session drops, the collector dies and the segment is lost. Tool paths below are from the repo root: `docs/soak/tools/linux/collect.py`.
1. **Pin the build.** The runner tells you the commit. Pin the image **by digest**, not by tag, and write the digest down. The runner fills it in from the build on `main` that day:
   - image digest: `<digest>`
   - source commit: `<commit>`
   - spec commit (`--spec-commit`: the commit whose CR-SPEC is in force for this run, named by the runner): `<spec-commit>`
   - Every segment of a block uses **the same digest**. A different digest makes the segment invalid.
2. **Pin the collector.** Use `docs/soak/tools/linux/collect.py` and `analyze_cr.py` from the runner-named commit on `main` (not the commit that last changed the spec; the tools have changed since). Record each file's hash:
   ```bash
   sha256sum docs/soak/tools/linux/collect.py docs/soak/tools/analyze_cr.py
   ```
   Write both hashes down. They must not change during a block.
3. **Preset and switches.** Preset Balanced, which is the shipped default: leave `LODESTAR_INTENSITY` unset (or `balanced`) and don't change the preset in the UI. Every crawler switch at its shipped default, except `LODESTAR_DHT_SAFE_RESTART=true` and the treatment's own switch on the B arm:
   - CR-AB-ADM B arm: `LODESTAR_DHT_COORDINATED_ADMISSION=true`
   - CR-AB-4 B arm: `LODESTAR_DHT_UTP=true`
4. **Network is unchanged** (owner-decisions §2.51). The DHT UDP port is **not** forwarded, UPnP stays off, and the router, the LAN and the public IP are the same as for the last comparator run. If any of them changed, stop and tell the runner: the comparator is re-run before the next run.
4a. **Clock.** The host clock must be synchronised (NTP) to within 1 s, or every segment fails (CR-SPEC D1a). Check it in the Unraid UI and note "synced".
4b. **Mover.** A mover run counts as an `array_operation` and invalidates a segment, and §2.68 doesn't let you pause it. Look at the mover schedule: if it falls inside a block, **don't start; ask the runner**, who asks the owner.
4c. **Both the auto-update plugin and Appdata Backup must already exclude the Lodestar container,** and you note each one's next scheduled run. Excluding a container is a settings change, which §2.68 doesn't cover: a test only pauses and restores. If the container isn't excluded from either, **stop and ask the owner** (through the runner).
4d. **The admin password file.** The collector needs one (`--password-file`). Create it with the container's admin password, `chmod 600`, outside the data and evidence directories. Never send or commit it (section 9.3).
5. **Nothing else crawls from this box or this public IP.** No bitmagnet, no other Lodestar, no import. Runs never overlap a soak on the same box.
6. **Disk and memory room** (section 4.3). Check them now.
7. **Rehearse once** if the runner asks. The runner supplies the exact rehearsal command (it needs the password file and the checklist too). A rehearsal never counts and is marked in `run.json`.

## 4. Host safety

### 4.1 What you pause, and what you restore

Pause only these two, and only for the run:

| Pause | How to restore |
|---|---|
| Appdata Backup: pause the schedule. The Lodestar container must **already** be excluded from it (4c) | put the schedule back |
| Scheduled parity check (correcting or not) | put the schedule back |

For each, write down **before** the pause: the setting as it was, and its next scheduled run in UTC. The collector takes the **backup** and **auto-update** next-run times (`--next-run`); the parity time is for your own restore record only.

**The pause is per block, not for the whole stage** (six days without backups is too long for the owner's data):

- Pause at the start of each block and restore at its end, or at once on any stop.
- **Between blocks**, if a backup or a scheduled parity check is due or overdue, let it run and finish **before** the next block starts. Never start a block while one runs (4.2). Record what ran and when.
- Log every pause and restore per block (section 12).
- This means the CR-AB-4 block may start later than the ADM block's end; the no-idle aim (r5.1 A3) gives way to this rule.

**The restore always runs**: when a block finishes (not only when stage 1 ends), when it fails, when you stop it, when you're out of time. It is not optional and doesn't wait for analysis.

### 4.2 Start gates

Do not start a block if any of these holds:

- A **parity check, sync, rebuild, recovery or mover run is in progress.** Wait for it to finish. Never cancel it (§2.68). Once it has finished, pause the schedule and start.
- A scheduled parity check or backup will start inside the block's window and you can't pause it.
- Free disk is under 50 GiB on the evidence volume or the block's data volume. The collector guards only the evidence volume and the paths you give with `--storage-path`, so always pass `--storage-path <block directory on appdata>`.
- A mover run is scheduled inside the block (4b).
- Another crawler is running on the host or the public IP.

**A parity, sync or rebuild inside a segment invalidates that segment** (CR-SPEC rule 6.5). Write a host-activity entry when it happens (section 6).

### 4.3 Bounds (the collector enforces them; you watch them)

| Bound | Value | What happens |
|---|---|---|
| Free disk, start | at least 50 GiB | the collector refuses to start |
| Free disk, warning | 20 GiB | warning in the log |
| Free disk, stop | 10 GiB | the run stops (protected shutdown) |
| Memory | container `anon` memory above 1 GiB for five samples in a row | the run stops (decisions/2026-10-31-X-soak-docker-memrule.md) |
| Any other collector guard (stall, bounds) | | the run stops |
| Identity lost | container ID or start time changes, or unreadable for 3 min | the run ends |

A stop by any bound makes the segment invalid and the block is re-run.

### 4.4 How to stop at a safe point

Use the collector's own stop, never a kill:

1. Create an empty `stop.request` file in the run's evidence directory.
2. Wait for the collector to finish. It runs its shutdown and writes `result.json` with reason `operator_stop`.
3. Read `result.json`. Don't kill a process id.
4. **Run the restore (4.1).** Then tell the runner (section 11).

An early stop is an interrupted run. The directory is never reused. The block is re-run from a fresh directory.

If the box itself is in trouble (disk almost full, a disk fault, the array degraded): stop as above, run the restore, and tell the owner.

## 5. A block, step by step

Do this once per block. Steps 5.1 and 5.2 happen once; 5.3 to 5.5 repeat for each of the four segments.

### 5.1 Set up the block

1. Do section 3 and the start gates (4.2).
2. **Pause** the backup and parity schedules as in 4.1 (if a backup or parity check is due or overdue, let it finish first). Note the time and the next-run times.
3. Make a **new, empty** data directory: `<appdata>/lodestar-<treatment>-b<n>-a<attempt>/data`. `<attempt>` is 1 for the first try and rises for each re-run of that block, so a re-run never resumes an old directory. Never reuse one.
4. Make a **new, empty** witness directory outside it: `<appdata>/lodestar-<treatment>-b<n>-a<attempt>/witness`.
5. Decide the order: block 1 is **ABBA**, block 2 is **BAAB**. Write it in the run record. The recorded order is passed to the analyzer later.
6. Make a new, empty evidence directory per segment, outside the data directory: `<evidence>/<treatment>-b<n>-a<attempt>-s<k>`. Old data directories of an invalid block are kept until the runner says to delete them.

### 5.2 Container settings for every segment

| Setting | Value |
|---|---|
| Image | the pinned digest, not a tag |
| Data | the block's data directory |
| Witness | the block's witness directory, bind-mounted from the host at a container path **outside `/data`**, e.g. `/witness`, with `LODESTAR_DHT_WITNESS_DIR=/witness`. A path under `/data` makes every segment invalid |
| Preset | Balanced (unset `LODESTAR_INTENSITY`) |
| Switches | `LODESTAR_DHT_SAFE_RESTART=true`; the treatment's switch `true` on B and unset or `false` on A; nothing else |
| Network | as for every earlier run: DHT port not forwarded, UPnP off |

Changing an environment variable on Docker recreates the container. That is expected: the witness directory is mounted from the host so it survives.

### 5.3 Start a segment

1. Start the container (A or B per the order).
2. **Start the collector within minutes.** It must see the start-up. Example for the first segment of a CR-AB-ADM block (fill in the values; the collector checks them):

   ```bash
   python3 docs/soak/tools/linux/collect.py --mode docker --container <container> --root <evidence>/adm-b1-s1 \
     --run-kind ab --treatment adm --commit <commit> --spec-commit <spec-commit> \
     --url <http://127.0.0.1:<host port>> --password-file <password file> \
     --storage-path <block directory on appdata> \
     --checklist auto_update=excluded --checklist backup=excluded \
     --next-run auto_update=<UTC time of its next run> --next-run backup=<UTC time of its next run> \
     --intensity balanced --stop
   ```

   - Use `--treatment utp` for CR-AB-4.
   - Use `python3 docs/soak/tools/linux/collect.py --help` for the current flags. The checklist and `--next-run` must name both items, or the collector refuses.
   - `--url` must match the container's host port mapping (the default is `http://127.0.0.1:8484`).
   - `--password-file` is required (3, step 4d).
   - Leave the command running in `tmux` or similar. Progress is in `status.json`; don't open `samples.jsonl` until the run ends.
   - `--stop` makes the collector stop the verified container when collection ends.
3. Check the collector's first lines: the mode, the image digest and the arm. Check once. Don't look at rates.
4. Note the start time in the run record, and work out the expected end of this segment (first segment: start + about 13 h; later ones: start + about 7 h) so you're there for the next arm switch.

### 5.4 During a segment

See section 6. In short: touch nothing, write a host-activity entry for any event, and don't read rates.

### 5.5 Switch arms (between segments)

The arm switch must be **a clean stop, then a start, within 10 minutes** (CR-SPEC rule 2.3). Plan the minutes before you begin; don't do it while distracted.

1. The clock for the 10 minutes starts at the collector's **last scrape**, in practice when it reports done. Watch for `result.json` appearing in the evidence directory: that means it has finished. Check it is complete.
2. The collector stops the container (because of `--stop`). Confirm it stopped cleanly. A kill is not a clean stop.
3. **Start the next segment's container at once** with only the treatment's switch changed. Keep the same data directory, witness directory, image digest and every other setting.
4. Start a new collector with a **new, empty** evidence directory.
5. If more than 10 minutes pass between the stop and the start, the segment is invalid (the block is re-run). Write down the times anyway.

After segment 4, don't start another. Go to 5.6.

### 5.6 End of the block

1. Check that the collector for segment 4 finished and wrote `result.json`.
2. Run the analysis (section 8) for each segment, one by one, then the block's effect once all four exist.
3. **Run the restore (4.1) now**, at the block's end, before anything else.
4. Check the restore worked: look at the schedules in the Unraid UI and confirm they are back. If you can't confirm, that is a failed restore: **Rule 0 applies.**

## 6. During a segment

### 6.1 Don't

- Don't restart, stop or recreate the container, or let a plugin do it (backup, auto-update). Write an entry if it happens.
- Don't look at rates, the dashboard's numbers or the metrics. The collector shows none while a window is open, on purpose.
- Don't run an import, a second crawler, a parity operation, or a backup.
- Don't change network settings, ports, UPnP or the router.
- Don't update the collector, the image or the config.
- Don't run builds, test suites or large jobs on the box. Keep the box quiet.

### 6.2 Do

- **Write a host-activity entry the moment anything on the list happens**, through the collector's note command. It stamps UTC and elapsed time:

  ```bash
  python3 docs/soak/tools/linux/collect.py note --root <evidence>/<segment> --category <category> --text "<short private detail>"
  ```

  The fixed categories: `import`, `other_crawler`, `array_operation` (parity check, rebuild, mover), `host_reboot`, `plugin_container_action`, `link_outage`, `router_outage`, `public_ip_change`, `power_event`. (`other` is also accepted but invalidates nothing.)
- An entry **written late, written after the window ended, or written after the data was viewed** doesn't invalidate the segment. It is published as a note. So write it at the time.
- Events the collector sees itself (a restart, a counter reset, a public-IP change, a scrape gap) need no entry.
- Keep the box awake and powered. A power cut or reboot invalidates the segment.

### 6.3 What the segment checks

At the end the analyzer judges each segment against the spec. In plain terms:

- **ADM off:** every `fetch_admission_*_total` delta is 0 and `fetch_admission_permits_in_use` reads 0 at every scrape.
- **ADM on:** `fetch_admission_grants_total` grew, and the permit identity returns to 0 at some scrape within every 60 minutes. Off: every `fetch_admission_*_total` delta is 0.
- **uTP on:** `dht_utp_in_total` and `fetch_attempts_total{transport="utp"}` both grew. Off: both are 0 and no `utp_*` series appears.
- **A segment whose treatment did not take effect is void, not negative.** A void segment invalidates its block (except route backoff's "no opportunity").
- **Every segment:** Balanced preset, no import job, same image digest across the block, only the expected switches on.

## 7. Stopping and failures

| What happened | What you do |
|---|---|
| A collector guard tripped (disk, memory, identity) | the segment is invalid. Run the restore (4.1). Tell the runner. The block is re-run from a fresh directory. |
| You stopped the run (4.4) | same |
| The host rebooted or lost power | same. Write an entry when you can. |
| The arm switch took more than 10 min | the segment is invalid. Finish nothing more in this block unless the runner says so. Restore. |
| A parity operation or backup ran during a segment | write the entry; the segment is invalid. Restore. |
| The analyzer says a segment is void or invalid | the block is re-run in the same order from a fresh directory. The failed block is published with its reason. |
| **A restore failed or can't be confirmed** | **Stop. No further tests. Tell the owner at once** (§2.68), and the runner. |

**Never stitch.** Don't assemble a window from two processes, two periods or two data directories.

## 8. After each segment and block

Run from the repo checkout at the pinned commit. Replace the paths.

1. **Each segment, as it closes:**

   ```bash
   python3 docs/soak/tools/analyze_cr.py segment <evidence>/<segment> \
     --output <reports>/<segment>.json --publish <publish>/<segment>
   ```

   There is no `--winners` in stage 1. Read the verdict line. Don't read more into one segment's rates than the report says.
2. **After the four segments of a block**, the effect. Give one `--design` per `--block`, in the same order as the blocks:

   ```bash
   python3 docs/soak/tools/analyze_cr.py effect --treatment adm \
     --block <reports>/adm-b1-s1.json <reports>/adm-b1-s2.json <reports>/adm-b1-s3.json <reports>/adm-b1-s4.json \
     --block <reports>/adm-b2-s1.json <reports>/adm-b2-s2.json <reports>/adm-b2-s3.json <reports>/adm-b2-s4.json \
     --design abba --design baab --output <reports>/adm-effect.json
   ```

   - A block whose switches show a different order from its `--design` is invalid (`order_differs_from_design`).
   - Use `--treatment utp` for CR-AB-4.
   - With only one block done, the fit has one degree of freedom and can't decide. Wait for the second block.
3. **The outcome and what follows are the runner's.** Checkpoint 1 is the runner's and happens the day stage 1 is decided (r5.1 A3).
4. Run `python3 docs/soak/tools/analyze_cr.py --help` if a flag here doesn't match; the tool wins over this sheet.

## 9. Evidence

### 9.1 Where it goes

- CR-AB-ADM: `docs/soak/evidence/cr-ab-adm/**`
- CR-AB-4: `docs/soak/evidence/cr-ab-4/**`

The runner commits these. You hand over the publishable bundles from `--publish`.

### 9.2 What is published (CR-SPEC rule 7)

Aggregates and hourly snapshots only: the numerators (gross and net), latency buckets, the mechanism and guardrail series, peak memory and CPU, the identity block (image digest, source commit, every switch, preset, `safe_restart`, whether the witness is host-mounted), the host checklist and activity log as fixed categories with elapsed times, and each segment's verdict with its reason. Invalid blocks are published too, with their reason.

### 9.3 What stays private (never committed, never sent)

- The raw evidence directories (per-minute rows), logs and databases.
- Credentials and password files.
- Executables and the image itself.
- Peer or node IPs; the public IP (say only "unchanged" or "changed"); ports; node IDs; tokens; infohashes; torrent or file names.
- Host addresses, share names, user names, paths on the box.

Check a bundle by eye for any of these before handing it over. If one is there, don't send it; tell the runner.

## 10. Your first hour: checklist

Tick each one. Write the time (UTC) beside it.

- [ ] The owner has said the box is back (date and time, UTC)
- [ ] The runner gave me the commit, the digest and the spec commit
- [ ] No parity check, sync, rebuild or recovery is running
- [ ] No other crawler or import is running on the box
- [ ] Network unchanged: port not forwarded, UPnP off, public IP the same
- [ ] I noted the backup and parity settings and their next runs (UTC)
- [ ] Container already excluded from Appdata Backup (next run noted); backup schedule paused
- [ ] Parity check schedule paused
- [ ] Free disk is at least 50 GiB; clock synced; no mover inside the block
- [ ] Auto-update plugin already excludes the container (next run noted)
- [ ] Admin password file made (`chmod 600`, outside data and evidence directories)
- [ ] New empty data directory and witness directory
- [ ] Image is the pinned digest; collector and analyzer hashes written down
- [ ] Block order written down (block 1 ABBA)
- [ ] Container started with `LODESTAR_DHT_SAFE_RESTART=true`, Balanced, the right arm
- [ ] Collector started within minutes; it shows the right mode, digest and arm
- [ ] I checked the collector's first lines once and then left it alone
- [ ] I know how to stop (4.4) and how to restore (4.1)
- [ ] **I sent the runner the "started" numbers (section 11)**

## 11. What to send the runner

Send **numbers only**. No addresses, no names, no logs, no files unless the runner asks for a bundle.

**When a block starts:**

- the treatment (adm or utp), block number, order (ABBA or BAAB)
- the image digest and the collector and analyzer hashes
- the start time, UTC
- the backup and parity pause times, UTC, and the next runs noted
- free disk, GiB

**When each segment closes:** the segment number, arm, the verdict line (valid, invalid or void) and the reason, and the start and end times, UTC.

**When a block ends:** the four verdicts, the effect result for the treatment once both blocks exist, and the restore time, UTC, with "confirmed" or "NOT confirmed".

**Immediately, any time:** a stop, an invalid or void segment, a host event, a collector guard, or a failed or unconfirmed restore. The last one also goes to the owner.

## 12. Run record (copy this per block)

```
treatment:           adm | utp
block:               1 | 2        order: ABBA | BAAB
image digest:        <digest>
source commit:       <commit>
spec commit:         <spec-commit>
collect.py sha256:   <hash>
analyze_cr.py sha256:<hash>
preset:              Balanced
switches on (A):     safe_restart
switches on (B):     safe_restart + <treatment switch>
witness mounted from host:  yes | no
backup paused at (UTC):     <time>   next run was (UTC): <time>
parity paused at (UTC):     <time>   next run was (UTC): <time>

segment  arm  start (UTC)  T_open (UTC)  end (UTC)  verdict  reason
1
2
3
4

arm-switch gaps (minutes, each must be 10 or less):  s1-s2 __  s2-s3 __  s3-s4 __
host-activity entries (category, time UTC):
restore done at (UTC):      <time>     confirmed: yes | NO
```
