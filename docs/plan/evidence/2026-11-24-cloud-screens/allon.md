<!-- Reported, not verified. Copied on 2026-11-27 from the X-test-vm lane session's own report text (its transcript), because the instance and its results file were terminated with it and the report never reached the runner. Not a CR-SPEC run: own 1-min scraper, hours counted from suppression end with no 60 min warmup, gross stored counter, no carry-in bound, no control arm. Never evidence of a gain or of the gate. -->

# allon: all-switches interaction and stress screen (numbers only)

Screen, never evidence of a stored/min gain. One continuous on-only block (MODE=on, PURPOSE=screen), no control.

## Setup
- Instance c6a.xlarge us-east-1, launch 2026-11-23T21:59:58Z, ExpiresAt 2026-11-24T13:59:57Z.
- Lodestar commit 6bec9388279ed1ea6b3c07c8b7fc788f7ee22b18 (contains 3288d133d); image built from main as `lodestar`, sha256:995121405f88c7b2b4b81df2710026c0a3b17b8bc2907add5fa7be1f9439661f; Balanced preset (slots capacity 80).
- Local change (work folder copy only, never the repository): block-lib.sh `switches_of adm` echoes all 12 switches:
  `adm) echo LODESTAR_DHT_COORDINATED_ADMISSION LODESTAR_DHT_RESPONDER_HINTS LODESTAR_DHT_STREAMING_PEERS LODESTAR_DHT_TARGET_PREFIXES LODESTAR_DHT_UTP LODESTAR_DHT_ROUTE_BACKOFF LODESTAR_DHT_LOOKUP_END LODESTAR_DHT_PRESET_SLOTS LODESTAR_DHT_HINT_FIRST LODESTAR_DHT_SAMPLER_COOLDOWN_SPLIT LODESTAR_DHT_SAMPLE_BACKOFF_ESCALATE LODESTAR_DHT_SOFT_TIMEOUT ;;`
  unified diff 11 lines, diff sha256 60b568fa215390e041f57d1444b03882a3eca469f6a3939f87bb11501890f05d; patched block-lib.sh sha256 b9420a0f93b8893415bd9b561318e19446a89899beedf890251326544e18d65b.
- args: TREATMENT=adm, LOCAL_IMAGE_ID, SEGMENT_HOURS=3, ROLE=main, NOTICE_VERSION=5, MODE=on, PURPOSE=screen.
- /proc/<exe>/environ: the 12 switches =true plus LODESTAR_DHT_SAFE_RESTART=true (as start-block sets it); only other LODESTAR_DHT_ var is WITNESS_DIR (a setting). No config error: Lodestar accepts the full combination.
- Leak test: passed first run (55 ok, captured_*_ifdown 0, host_egress 0). start-block rc 0, ok mode_on, Lodestar start 2026-11-23T22:22:51Z.
- Segment 1 collector SIGKILLed after setup so it could not stop Lodestar at window end; own 1-min /metrics scraper (724 scrapes, 0 missing).

## Hourly table (h from suppression end 2026-11-24T04:23Z; counters are the hour's increase)
Suppressed baseline h-6..h-1 (22:27-04:23Z): stored 0, samples 0, fetch success 0-1.26 % (few attempts), q/s max 25, outstanding max 60, anon 33-35 MiB.

| h | stored/min | samples/min | fetch % | q_out (≈q/s) | outst max | ADM grants | HINTS capt/q | STREAM | PREFIX | UTP att/ok/in | ROUTE skip/down | LEND ev | HFIRST mark/T1 | SOFT to/late | spill max | anon max/last MiB | RSS max |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 0 | 100.5 | 2385 | 3.79 | 714k (198) | 521 | 18.6k | 778k/6.5k | 36.5k | 143k | 85.6k/4.8k/555k | 32/10 | 6.5k | 43.6k/14.8k | 166k/589 | 0 | 352/339 | 383 |
| 1 | 91.3 | 2398 | 4.01 | 721k (200) | 465 | 17.9k | 646k/5.8k | 33.9k | 144k | 74.3k/4.3k/496k | 14/6 | 5.3k | 40.2k/14.3k | 168k/659 | 72k | 434/422 | 465 |
| 2 | 91.1 | 2205 | 4.31 | 721k (200) | 475 | 17.6k | 538k/5.4k | 33.0k | 133k | 69.6k/4.4k/511k | 14/9 | 4.8k | 35.6k/14.0k | 168k/644 | 198k | 456/413 | 493 |
| 3 | 105.0 | 2005 | 4.00 | 721k (200) | 462 | 19.1k | 519k/6.5k | 37.3k | 121k | 85.2k/5.1k/625k | 15/7 | 6.4k | 31.7k/15.2k | 167k/918 | 312k | 477/404 | 508 |
| 4 | 97.5 | 2149 | 4.22 | 721k (200) | 481 | 18.2k | 521k/5.8k | 34.4k | 129k | 75.6k/4.8k/527k | 18/5 | 5.5k | 33.5k/14.5k | 168k/817 | 436k | 462/456 | 506 |
| 5 | 98.7 | 2147 | 4.24 | 721k (200) | 473 | 18.1k | 527k/5.9k | 34.5k | 129k | 76.2k/4.9k/551k | 17/6 | 5.5k | 32.7k/14.5k | 168k/751 | 560k | 471/463 | 502 |
| 6 (9 min) | 99.7 | 2243 | 4.50 | 108k (200) | 456 | 2.7k | 81k/0.9k | 5.1k | 20k | 11.0k/0.7k/75k | 1/1 | 0.8k | 5.2k/2.2k | 25k/81 | 574k | 460/450 | 491 |

ESC (`dht_sample_cooling_reason{reason="first_failure"}` gauge): 5.9k-7.8k every hour after suppression. ADM waiters gauge: pinned at 199.4-199.5k from 04:42Z to the end; permits in use 80/80; HFIRST T1 waiting gauge 256 throughout.

## Verdicts
- a. Every mechanism's counter moves: YES, all 12 every post-suppression hour (table). None masked. PREFIX queries equal samples sent, as designed. ROUTE skipped is small (5-32/h) but non-zero. SPLIT spill_slots first > 0 at 05:51Z and grows to 574k of 8,388,608.
- b. No interaction harm: YES with two watch items.
  - 0 panics, 0 debug-assert lines, 0 ERROR lines, error counters (writer, search, storage_sample, restart_persist) 0, no counter ran backwards across 724 scrapes, 0 restarts, no stall.
  - Watch: ADM waiter queue saturated at about 199.5k (bounded by the pool's max_pending with eviction, flat for 5.8 h: not unbounded, but fetch admission is the bottleneck: 80/80 permits, about 97 % of arrivals wait).
  - Watch: spill table grows about 115-125k slots/h (bounded at 8.39M by design; would not reach it within this run).
- Table-full stall: table_filled_after_seconds 5288 (05:51Z); stall minutes 0; samples stayed 2.0-2.4k/min after the fill; cooling plateaued about 98-113k instead of reaching 200k. The split prevented the stall.
- c. No collapse: YES. stored/min 91-105 every hour (vs shipped defaults' 16.0 before and 10.0 in the stall); fetch success 3.79-4.50 % (vs 4.3-4.8 %), lower in h0-h1, in range after. The combo lane's numbers were not available to this lane.
- d. Caps hold: YES. Whole-hour query rate 198-200.3 q/s (721k per hour; per-minute max 203.6 is 1 s scrape-timestamp jitter on 60 s gaps); outstanding max 521 (cap 1,024); throttled 45-59k/h shows the limiter engaged.
- e. Memory holds: YES with a note. anon (cgroup) per hour max/last: 352/339, 434/422, 456/413, 477/404, 462/456, 471/463, 460/450 MiB; slope over the last 4 h +7.4 MiB/h (flat). Peak 477 MiB is above the single-switch lanes' 300-415 MiB, with the rise in h1-h2 coinciding with the spill table (32 MiB on first spill plus growth). RSS max 508 MiB.
- f. No panics, debug-assert messages or error bursts: YES (0 / 0 / 0 ERROR; 1 WARN line in 12 h).

## Restart drill (after the window)
- 10:32:51Z `docker compose restart lodestar` as root from /opt/lodestar-run (LODESTAR_IMAGE=sha256:<image id>, DOCKER_GID=unused): rc 0, /healthz 200 after 2 s, RestartCount 0 (compose restart), process started 10:32:51Z.
- Switches: environ before and after identical (13 =true: 12 + SAFE_RESTART).
- Data intact: torrents 36,085 -> 36,086, search docs 36,083 -> 36,086 at the first post-restart scrape (no loss); 37,265 / 37,263 at 10:49Z.
- Counters start again (10:33:56Z -> 10:49:57Z): stored +1,166, ADM grants +4,448, STREAM +7,812, UTP in +102,720, ROUTE probe_down +2 / skipped +4, LEND +988, SOFT +47,330.
- Sampling suppressed again after the restart: suppression reason `locked` at first, then 15,907 s remaining at 10:33:56Z (14,945 s at 10:49Z), so samples, HINTS, PREFIX, HFIRST and SPLIT stay 0 in the 15 min drill window. Observed, not judged; the coordinator to check against SAFE_RESTART's expected resume behaviour.
- anon after restart 241-285 MiB; 0 panics, 0 ERROR lines.
