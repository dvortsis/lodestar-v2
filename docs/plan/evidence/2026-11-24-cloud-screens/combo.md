<!-- Reported, not verified. Copied on 2026-11-27 from the X-test-vm lane session's own report text (its transcript), because the instance and its results file were terminated with it and the report never reached the runner. Not a CR-SPEC run: own 1-min scraper, hours counted from suppression end with no 60 min warmup, gross stored counter, no carry-in bound, no control arm. Never evidence of a gain or of the gate. -->

# combo lane: four likely-shipping switches together (works-as-designed screen)

Works-as-designed, interaction and regression screen only. On-only block, no control arm: every rate here is a lead, never evidence of a stored/min gain and never the G-v06 gain.

## Setup
- c6a.xlarge us-east-1, launch 2026-11-23T22:00Z, ExpiresAt 2026-11-24T14:00Z.
- Commit 6bec9388279ed1ea6b3c07c8b7fc788f7ee22b18 (contains 3288d133d), image built locally as `lodestar` (ID ce2f80881948…).
- Switches (from /proc/<exe>/environ, before and after the drill): COORDINATED_ADMISSION, LOOKUP_END, PRESET_SLOTS, SAMPLER_COOLDOWN_SPLIT = true; SAFE_RESTART = true (start-block). No other LODESTAR_DHT_* switch set.
- Local change (work copy of block-lib.sh only, never the repository), diff sha256 9d7c401dcb55c9023441f363b5cb7aa798c810fb6bcb3d72b9759cf97e1d544b:
  ```
  -    lc) echo LODESTAR_DHT_LOOKUP_END LODESTAR_DHT_PRESET_SLOTS ;;
  +    lc) echo LODESTAR_DHT_LOOKUP_END LODESTAR_DHT_PRESET_SLOTS LODESTAR_DHT_COORDINATED_ADMISSION LODESTAR_DHT_SAMPLER_COOLDOWN_SPLIT ;;
  ```
  start-block args: TREATMENT=lc MODE=on PURPOSE=screen SEGMENT_HOURS=3 ROLE=main NOTICE_VERSION=5 LOCAL_IMAGE_ID. Collector 1 SIGKILLed after setup so its window couldn't stop Lodestar; a 1-min /metrics scraper ran instead.
- Leak test 1: 49 ok, failed only `vpn_interface_held_down` (known flake), 0 leaked packets. Leak test 2: pass (55 ok). start-block's own leak test: pass (67 ok total).
- Lodestar started 22:23Z; suppression ended 04:24Z (21,600 s, `first_start`); crawl to 10:31Z.
- Known gap: the AWS connector's sign-in lapsed 07:31Z–10:30Z; the crawl and scraper ran on, so no data was lost.

## Hourly, after suppression (leads only)
Single-switch leads: ADM ~20.4, split ~19.8, lookup_end mean 31.4 (11-22), shipped defaults ~10–16 stored/min.

| h | stored/min | samples/min | fetch % | q/s mean (1-min max) | outst max | grants | refusals | spill end (+growth) | sampler-full s | enough_values | occ max (>32 scrapes) | W s | q | y | RSS / anon MiB |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | 66.2 | 4,300 | 5.30 | 200.0 (204.2) | 519 | 14,880 | 0 | 60,654 (+60,654) | 0 | 4,344 | 80 (61/61) | 19.4 | 29.0 | 0.267 | 421 / 389 |
| 2 | 63.4 | 4,246 | 5.99 | 199.9 (203.6) | 520 | 14,064 | 0 | 314,089 (+253,435) | 0 | 3,620 | 80 (61/61) | 20.5 | 30.8 | 0.270 | 469 / 437 |
| 3 | 63.6 | 4,176 | 5.92 | 200.0 (203.7) | 490 | 14,182 | 0 | 563,462 (+249,373) | 0 | 3,687 | 80 (61/61) | 20.3 | 30.9 | 0.269 | 469 / 436 |
| 4 | 64.4 | 4,174 | 6.07 | 200.0 (204.5) | 522 | 14,179 | 0 | 812,611 (+249,149) | 0 | 3,747 | 80 (61/61) | 20.3 | 30.9 | 0.273 | 513 / 480 |
| 5 | 65.2 | 4,130 | 5.94 | 200.0 (204.2) | 529 | 14,261 | 0 | 1,059,106 (+246,495) | 0 | 3,855 | 80 (61/61) | 20.2 | 30.9 | 0.274 | 496 / 463 |
| 6 | 61.7 | 4,144 | 5.72 | 200.0 (204.1) | 537 | 14,164 | 0 | 1,306,294 (+247,188) | 0 | 3,773 | 80 (61/61) | 20.3 | 31.0 | 0.262 | 554 / 519 |

First hour in 10-min bins: stored/min 72.9, 64.7, 71.5, 59.6, 64.0, 64.5; fetch % 4.92, 4.80, 5.61, 5.18, 5.41, 6.10; spill 0, 0, 0, 0, 17,624, 60,654; grants 2,688, 2,561, 2,463, 2,357, 2,417, 2,394; enough_values 881, 752, 721, 648, 701, 641.

Suppressed hours 1–6 (baseline): stored 0, samples 0, q/s 0.1–0.4, outstanding ≤25, grants ≤19/h, RSS ~59 / anon ~31 MiB.

Other: capacity 80 throughout; `dht_lookup_queued` 0; `fetch_admission_waiters` flat at about 199,500 from the first minute (a cap, not growth); `table_filled_after_seconds` 2,760; stall minutes 0; no counter ran backwards before the drill; 0 panics, 0 ERROR lines.

Spill projection: last-2-h slope 242,282 slots/h; linear reach of 7,864,320 about 33.1 h after suppression ends. No plateau seen within the 6.1 h window (6 h cooldowns would begin ending about 6 h after the fill at 05:10Z, i.e. just at the window's end).

## Restart drill (10:31:48Z, `docker compose --profile block restart lodestar`, root, /opt/lodestar-run, LODESTAR_IMAGE=sha256:<local ID>, DOCKER_GID=unused)
- rc 0; healthy after 10 s; restart check `mode=resume reason=resumed previous=clean generation=2`.
- Switches identical before and after.
- Data intact: torrents 23,612 → 23,618, search docs 23,611 → 23,618 (both still rising).
- Counters restarted (14 min after): grants +3,501, refusals 0, enough_values +922, lookups ended +3,504, ~130 q/s, occupied 80 of 80, torrents +903.
- Sampling gated after the restart: suppression 18,697 s (`resumed`), samples 0, spill gauge 0 — the split's documented post-fill restart gate (up to about 6 h).

## Verdicts
- a. Every mechanism runs: yes. Grants 14,064–14,880/h with 0 refusals; spill slots used from 46 min after suppression, sampler-full 0 s; enough_values 3,620–4,344/h; capacity 80, occupied 80 and >32 in every scrape.
- b. Stacking and interaction: yes, no bad interaction seen. Stored 61.7–66.2/min (lead) beside the single-switch leads of 19.8–31.4; W ~20 s, q ~31, y ~0.27 steady. No counter backwards, no stuck lookups (lookups queued 0, ended steady), waiters capped flat. Finding: spill slots grow ~247 k/h, reaching the 7,864,320 fallback at ~33 h if linear.
- c. No collapse: yes. Stored 61.7–66.2/min every hour vs 16.0 (base, before stall) and 10.0 (in stall); fetch 5.3–6.1 % vs 4.3–4.8 %; no table-full stall.
- d. Within the preset: yes. q/s mean 199.9–200.0 (1-min max 204.5, scrape-timing jitter); outstanding max 537 of 1,024.
- e. Memory holds: yes. Anon 389 → 519 MiB over 6 h; last-4-h slope 8.6 MiB/h.
- f. No panics, debug-asserts or error bursts: yes (0 / 0 / 0).
- Drill: pass.
