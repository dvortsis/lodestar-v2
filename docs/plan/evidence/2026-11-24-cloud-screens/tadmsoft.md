<!-- Reported, not verified. Copied on 2026-11-27 from the X-test-vm lane session's own report text (its transcript); the instance and its results file were terminated with it. Not a CR-SPEC run unless the text says otherwise. Never evidence of a gain or of the gate. -->

# tadmsoft: COORDINATED_ADMISSION + SOFT_TIMEOUT (concurrent trio lane)

Works-as-designed and interaction screen. **Every rate here is a lead, never evidence of a stored/min gain**, and the lane is a concurrent lead beside tbase and tsoft, not an A/B. Numbers only.

## Setup
- Instance c6a.xlarge us-east-1, launched 2026-11-24T05:08:11Z, ExpiresAt 21:08:10Z.
- Commit a91ab090fdbfc985c2cd223466ce2674f5882e9b (contains 3288d133d and #1091); image built locally, ID prefix 257d4a1629c0.
- Leak test: run 1 failed on `vpn_interface_held_down` alone (48 ok; captured/host-egress packets 0/0/0/0); run 2 passed (55 ok, 0 leaked). start-block's own leak test passed; start-block 67 ok, 0 fail.
- Block: MODE=on, PURPOSE=screen, TREATMENT=admsoft (local key), SEGMENT_HOURS=3, ROLE=main, NOTICE_VERSION=5.
- Lodestar start 05:30:36Z. `/proc/<exe>/environ`: `LODESTAR_DHT_COORDINATED_ADMISSION=true`, `LODESTAR_DHT_SOFT_TIMEOUT=true`, `LODESTAR_DHT_SAFE_RESTART=true` (plus `WITNESS_DIR`, a path). No other switch.
- One-minute `/metrics` scraper (nsenter into gluetun's netns, loopback :8090), RSS from the `exe` process, anon from the container cgroup.

## Local diff (never in the repository)
`/root/local.diff` sha256 `d54a3727ced373d47cc643130a95bced1d79b800b9933d26bb85b94d7831f5d5`:
```
block-lib.sh  -TREATMENT_KEYS='adm|utp|route|lc|hint|split|soft'
              +TREATMENT_KEYS='adm|utp|route|lc|hint|split|soft|admsoft'
              +    admsoft) echo LODESTAR_DHT_COORDINATED_ADMISSION LODESTAR_DHT_SOFT_TIMEOUT ;;  # local tadmsoft lane only
              -    --run-kind ab --treatment "$TREATMENT" --intensity balanced --stop --rehearsal \
              +    --run-kind ab --treatment "$TREATMENT" --intensity balanced --rehearsal \
collect.py    -                      "soft": ("soft_timeout",)}
              +                      "soft": ("soft_timeout",),
              +                      "admsoft": ("coordinated_admission", "soft_timeout")}  # local tadmsoft lane only
```
Dropping `--stop` keeps the segment-1 collector from stopping Lodestar at its window end; the collector exited on its own and Lodestar kept running.

## Timeline
- Suppression end 11:31:00Z. `table_filled_after_seconds` 2,100 (cooling ≥ 200,000; 2,040 counting in-flight).
- Stall (samples < 100/min) from about 12:06Z to about 17:30Z: 325 stalled minutes after suppression; sampling resumed for the last ~9 minutes (5,451 samples/min).
- Crawl window end 17:39Z (launch + 12.5 h). AWS connector needed re-auth 07:41Z–12:36Z; crawl unaffected.

## First hour after suppression, per 10 min
| from | st/min | smp/min | fetch % | q/s | out max | W s | q | y | grants | refusals | cap rej | soft | late | soft/pl | late/soft | stall min |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 11:31 | 21.9 | 5,997 | 4.51 | 153.7 | 461 | 23.50 | 34.9 | 0.278 | 788 | 0 | 0 | 11,356 | 44 | 0.413 | 0.0039 | 0 |
| 11:41 | 20.8 | 5,997 | 4.10 | 165.7 | 402 | 23.65 | 36.9 | 0.266 | 781 | 0 | 0 | 11,284 | 60 | 0.391 | 0.0053 | 0 |
| 11:51 | 20.7 | 5,998 | 5.07 | 173.5 | 427 | 23.68 | 36.2 | 0.265 | 784 | 0 | 0 | 11,297 | 59 | 0.399 | 0.0052 | 0 |
| 12:01 | 21.8 | 1,959 | 4.87 | 105.4 | 427 | 23.58 | 36.5 | 0.279 | 778 | 0 | 0 | 11,372 | 42 | 0.400 | 0.0037 | 6 |
| 12:11 | 20.7 | 0 | 5.05 | 74.0 | 163 | 23.32 | 37.0 | 0.263 | 786 | 0 | 0 | 11,325 | 44 | 0.390 | 0.0039 | 10 |
| 12:21 | 21.4 | 0 | 5.59 | 75.2 | 168 | 24.10 | 37.2 | 0.278 | 771 | 0 | 0 | 11,351 | 69 | 0.397 | 0.0061 | 10 |

## Per UTC clock hour
`cooling` is `dht_sample_cooling` at the hour's end. Hours 05–10 are suppressed (baseline). U/S = unstalled/stalled part (minutes, stored/min, fetch %).

| hour | st/min | smp/min | fetch % | q/s | out max | W s | q | y | cooling | stall min | grants | refusals | cap rej | soft | late | soft/pl | late/soft | RSS MiB | anon MiB | U | S |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 05 (05:34–) | 0 | 0 | – | 0.2 | 2 | 25.03 | 46.7 | 0 | 0 | 0 | 3 | 0 | 0 | 49 | 0 | 0.350 | 0 | 61 | 33 | supp | – |
| 06 | 0 | 0 | 0 | 0.3 | 6 | 24.81 | 29.9 | 0 | 0 | 0 | 19 | 0 | 0 | 292 | 0 | 0.513 | 0 | 61 | 33 | supp | – |
| 07 | 0 | 0 | – | 0.2 | 3 | 25.50 | 31.5 | 0 | 0 | 0 | 2 | 0 | 0 | 32 | 0 | 0.508 | 0 | 61 | 33 | supp | – |
| 08 | 0 | 0 | 0 | 0.2 | 3 | 11.73 | 28.0 | 0 | 0 | 0 | 3 | 0 | 0 | 23 | 2 | 0.274 | 0.087 | 60 | 32 | supp | – |
| 09 | 0 | 0 | 0 | 0.3 | 8 | 20.30 | 26.3 | 0 | 0 | 0 | 16 | 0 | 0 | 199 | 1 | 0.504 | 0.005 | 61 | 32 | supp | – |
| 10 | 0 | 0 | 0 | 0.5 | 8 | 22.38 | 27.9 | 0 | 0 | 0 | 48 | 0 | 0 | 697 | 5 | 0.510 | 0.007 | 60 | 32 | supp | – |
| 11 (29 min supp) | 10.28 | 2,926 | 4.56 | 80.1 | 461 | 23.59 | 36.3 | 0.270 | 174,396 | 0 | 2,320 | 0 | 0 | 33,306 | 161 | 0.401 | 0.0048 | 309 | 278 | 60 / 10.28 / 4.56 (29 min post-supp ≈ 21.3) | – |
| 12 | 21.08 | 426 | 5.03 | 81.6 | 427 | 23.66 | 36.6 | 0.271 | 200,000 | 55 | 4,675 | 0 | 0 | 68,185 | 362 | 0.399 | 0.0053 | 357 | 325 | 5 / 22.60 / 4.94 | 55 / 20.95 / 5.04 |
| 13 | 20.87 | 0 | 4.85 | 76.1 | 168 | 22.97 | 35.9 | 0.259 | 200,000 | 60 | 4,821 | 0 | 0 | 68,328 | 338 | 0.395 | 0.0049 | 379 | 347 | – | 60 / 20.87 / 4.85 |
| 14 | 21.90 | 0 | 4.96 | 68.2 | 169 | 21.12 | 34.3 | 0.252 | 200,000 | 60 | 5,227 | 0 | 0 | 68,226 | 400 | 0.380 | 0.0059 | 382 | 349 | – | 60 / 21.90 / 4.96 |
| 15 | 21.33 | 0 | 5.25 | 73.9 | 171 | 23.33 | 36.1 | 0.270 | 200,000 | 60 | 4,740 | 0 | 0 | 68,262 | 400 | 0.399 | 0.0059 | 371 | 339 | – | 60 / 21.33 / 5.25 |
| 16 | 20.77 | 0 | 5.05 | 56.5 | 126 | 23.77 | 36.8 | 0.268 | 200,000 | 60 | 4,654 | 0 | 0 | 68,704 | 301 | 0.401 | 0.0044 | 378 | 337 | – | 60 / 20.77 / 5.05 |
| 17 (–17:39) | 22.69 | 1,258 | 5.40 | 77.9 | 425 | 23.66 | 37.1 | 0.291 | 199,594 | 30 | 3,041 | 0 | 0 | 44,477 | 247 | 0.395 | 0.0056 | 377 | 332 | 9 / 23.11 / 5.29 | 30 / 22.57 / 5.43 |

Lookup ends after suppression: values 23–32 %, no_values 2–5 %, deadline 63–73 %, query_cap 0.

## Split since suppression end
- Unstalled (11:31–12:06 and ~17:30–17:39, 43 min): 21.8 stored/min, about 5,800 samples/min, fetch 4.6–5.3 %, q/s ≈ 160.
- Stalled (12:06–~17:30, 325 min): 21.3 stored/min, 0 samples/min, fetch 5.07 %, q/s ≈ 71.
- Whole post-suppression window: 21.4 stored/min.

## Verdicts
- **a. Both mechanisms run: yes.**
  - Admission: about 4,650–5,230 grants/h after suppression (2–48/h suppressed), refusals 0 throughout.
  - `dht_peer_rejection_reasons_total{reason="capacity"}` 0 for the whole run (yesterday's ADM-off: about 4.3M/h).
  - Soft: 68,185–68,704 soft timeouts/h, 0.38–0.40 of peer-lookup queries, late ÷ soft 0.0044–0.0059. Same as soft alone (about 68.4k/h, 0.39, 0.003–0.005).
  - Every mechanism counter moved.
- **b. Interaction: no bad interaction seen.**
  - Both mechanisms' rates are as each showed alone.
  - No counter ran backwards (checked on every one-minute pair).
  - Lookups all end: W 21–24 s against the 25 s deadline; most end at the deadline (63–73 %), but none run past it.
  - `fetch_admission_waiters` reached 199.9k within 10 min of suppression end and then stayed flat or fell (195.7k–199.9k): bounded, not growing.
  - Permit identity at 17:39Z: grants 29,569 = actor returns 29,536 + finder returns 0 + in use 32, gap 1 (one in transit).
  - Trio comparison is the coordinator's (tbase, tsoft).
- **c. No collapse: yes.** Stored/min about 21.8 unstalled and 21.3 stalled, against the leads (base 16.0 before and 10.0 in the stall; stage-1 control 9.6–10.5). Fetch success 4.1–5.6 % against 4.3–4.8 %. A lead only, not a gain.
- **d. Within the preset: yes.** q/s max per 10 min 173.5 (≤ 200); outstanding max 461 (≤ 1,024).
- **e. Memory holds: yes.** Anon 277–349 MiB through the post-suppression hours; slope over the last 4 h about 6 MiB/h (earlier lanes about 320–400 MiB levels). RSS 309–382 MiB.
- **f. No panics, asserts or error bursts: yes.** 843 log lines at 17:39Z: 0 panic, 0 assert, 0 ERROR, 1 WARN. 0 container restarts before the drill.

## Restart drill
- After 12.2 h on the fresh database: `docker compose restart lodestar` as root from `/opt/lodestar-run` (`LODESTAR_IMAGE=sha256:<LOCAL_IMAGE_ID>`, `DOCKER_GID=unused`), rc 0, 17:40:08Z → 17:40:09Z.
- Before: 7,859 torrents, 7,859 search documents. After 15 min: 8,216 and 8,216 (never below the before counts).
- New `exe` process; environ still exactly `COORDINATED_ADMISSION=true`, `SOFT_TIMEOUT=true`, `SAFE_RESTART=true`. Log: `mode=resume reason=resumed` (1), 0 panic, 0 ERROR in 52 lines. Container restart count 0, status running.
- Suppression 0 after the resume (no new 6 h hold). Mechanisms restarted, 17:42–17:56Z: 1,086 grants, 0 refusals, 0 capacity rejections, 15,879 soft (0.383 of peer-lookup queries), 93 late (0.0059), 22.2 stored/min, 6,000 samples/min, q/s 158, outstanding max 373, W 23.7 s; RSS 396 MiB, anon 363 MiB.
- **Drill: pass.**

## Notes
- The segment-1 collector ended at its 3 h window (it carried no `--stop`), leaving Lodestar running, as intended.
- The AWS connector needed a re-sign-in 07:41Z–12:36Z; the crawl and scraper ran on the instance throughout, and no data was lost.
- Coordinator 07:30Z: no 2-hour chat lines, minimal wakes.
