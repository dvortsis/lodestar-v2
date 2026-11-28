<!-- Reported, not verified. Copied on 2026-11-27 from the X-test-vm lane session's own report text (its transcript); the instance and its results file were terminated with it. Not a CR-SPEC run unless the text says otherwise. Never evidence of a gain or of the gate. -->

# admhint: hint-first stacked on coordinated admission (X-test-vm screen)

Works-as-designed and interaction screen only. No rate here is evidence of a stored/min gain or of the G-v06 gain. Numbers only.

- Commit `9ea2e8557` (main; contains 3288d133d and #1091). Image built from it as `lodestar`, image id prefix `ecbf963a322c`.
- Instance `claude-dev-20261123-admhint`, c6a.xlarge, us-east-1. Launched 2026-11-23T23:27:17Z. Lodestar started 23:46:55Z on a fresh database, Balanced preset. Start suppression (`first_start`) ended 05:48Z (S). Crawl ran until 12:00Z.
- Switches, read from `/proc/<exe>/environ`: `LODESTAR_DHT_COORDINATED_ADMISSION=true`, `LODESTAR_DHT_HINT_FIRST=true`, `LODESTAR_DHT_SAFE_RESTART=true`. No other `LODESTAR_DHT_*` switch was set. `RESPONDER_HINTS` was unset.
- Local diff, to the start-block work copy of `block-lib.sh` only; nothing changed in the repository. The diff file's sha256 is `34b4917dba58aafc1da1405bd81daf0c86e767e5cdc9bf7765c94b5f485f90ca`.
  - `hint) echo LODESTAR_DHT_HINT_FIRST ;;` became `hint) echo LODESTAR_DHT_HINT_FIRST LODESTAR_DHT_COORDINATED_ADMISSION ;;`. This lets an on-only block (`MODE=on PURPOSE=screen TREATMENT=hint`) run both switches.
  - `--stop` was removed from `start_collector`. With it, segment 1's collector would have stopped Lodestar at its window end, about 2 h before the required crawl end.
- Leak test 1 failed on `vpn_interface_held_down` alone: 49 ok, every captured and egress count 0. Leak test 2 passed: 55 ok, `ok leak_test`. start-block passed with 67 ok and 0 fail, including its own leak test.
- Scraper: `/metrics` every minute, 730 scrapes. No counter ran backwards.

## Hourly
Columns:
- `samp`: samples/min.
- `fs%`: fetch success.
- `grants (h)`: admission grants, with `class="hinted"` in brackets.
- `marked`, `hq`, `useful`, `expired`: hint counters.
- `held/guard`: maximum `dht_hints_held` / `dht_hint_guard_entries`.
- `caprej`: actor `dht_peer_rejection_reasons_total{reason="capacity"}`.
- `W/q/y`: ledger figures.
- `anon/RSS`: memory, in MiB.

Admission refusals and every `dht_hint_fallbacks_total` reason were 0 in every hour.

| window (UTC) | stored/min | samp | fs% | q/s | outst max | grants (h) | marked | captured | hq | useful | expired | guard skips | held/guard | caprej | W | q | y | anon/RSS |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 23:50–03:50 suppressed (4 h) | 0 | 0 | – | ≤1 | 6 | 24–40/h (0) | 0 | 0 | 0 | 0 | 0 | 0 | 0/0 | 0 | 17.7–23.0 | 31.8–34.3 | – | 32–34/60–62 |
| 03:50–04:50 suppressed | 0 | 0 | – | 12 | 51 | 39 (0) | 0 | 0 | 0 | 0 | 0 | 0 | 0/0 | 0 | 21.6 | 32.0 | – | 32/60 |
| S+0–60 | 22.1 | 3,257 | 4.6 | 122 | 424 | 4,644 (2,117) | 64,856 | 64,856 | 2,116 | 654 | 63,995 | 396 | 2,302/70 | 0 | 23.8 | 34.8 | 0.286 | 319/351 |
| S+60–120 (stalled) | 19.8 | 0 | 5.0 | 54 | 153 | 4,501 (0) | 0 | 0 | 0 | 0 | 0 | 0 | 0/0 | 0 | 24.6 | 35.8 | 0.264 | 349/385 |
| S+120–180 (stalled) | 21.8 | 0 | 5.5 | 54 | 117 | 4,513 (0) | 0 | 0 | 0 | 0 | 0 | 0 | 0/0 | 0 | 24.6 | 36.1 | 0.290 | 359/392 |
| S+180–240 (stalled) | 21.4 | 0 | 5.3 | 53 | 119 | 4,480 (0) | 0 | 0 | 0 | 0 | 0 | 0 | 0/0 | 0 | 24.7 | 35.9 | 0.288 | 349/382 |
| S+240–300 (stalled) | 20.9 | 0 | 5.2 | 54 | 119 | 4,481 (0) | 0 | 0 | 0 | 0 | 0 | 0 | 0/0 | 0 | 24.8 | 36.5 | 0.280 | 369/402 |
| S+300–360 (stall ends) | 20.5 | 89 | 5.0 | 55 | 363 | 4,466 (57) | 1,305 | 1,305 | 56 | 11 | 0 | 1 | 1,223/54 | 0 | 24.8 | 36.4 | 0.274 | 335/369 |
| S+360–372 (partial) | 22.4 | 5,983 | 4.9 | 152 | 408 | 815 (652) | 19,699 | 19,699 | 652 | 208 | 18,282 | 104 | 1,981/62 | 0 | 24.8 | 35.7 | 0.302 | 363/397 |

First hour after S, per 10 min:

| S+ | stored/min | samp | fs% | marked | hq | useful | expired (of marked) | grants (h) | caprej |
|---|---|---|---|---|---|---|---|---|---|
| 0–10 | 24.7 | 5,990 | 4.0 | 20,653 | 616 | 175 | 19,085 (92%) | 771 (616) | 0 |
| 10–20 | 23.1 | 5,998 | 4.5 | 20,838 | 622 | 199 | 20,492 (98%) | 775 (620) | 0 |
| 20–30 | 23.5 | 6,000 | 4.8 | 18,728 | 626 | 199 | 18,226 (97%) | 783 (627) | 0 |
| 30–40 | 21.8 | 1,556 | 4.7 | 4,637 | 252 | 81 | 6,192 | 819 (254) | 0 |
| 40–50 | 19.7 | 0 | 4.7 | 0 | 0 | 0 | 0 | 750 (0) | 0 |
| 50–60 | 20.1 | 0 | 4.8 | 0 | 0 | 0 | 0 | 746 (0) | 0 |

## Table-full stall
- `table_filled_after_seconds` was 2,100 from S: `dht_sample_cooling` reached 200,000 at S+35 min.
- The stall lasted 326 min. Samples then recovered to about 6,000/min from about S+5 h 35 min. This recovery hasn't been reported before; the brief expected samples to stay near 0.

| Part | Minutes | stored/min | samp | fetch % | q/s | outstanding max | W | q | y |
|---|---|---|---|---|---|---|---|---|---|
| Unstalled | 45 | 23.2 | 5,922 | 4.5 | 159 | 424 | 23.9 | 34.6 | 0.302 |
| Stalled | 326 | 20.8 | 0 | 5.2 | 55 | 166 | 24.6 | 36.1 | 0.278 |

- **Hints in the stalled part:** 36 marked, 57 hint queries, 12 useful. Marking stops with sampling, so nothing is hinted while the table is full.
- **Admission queue:** `fetch_admission_waiters` rose to about 199,500 by S+1 h, then stayed between about 196,000 and 199,900. It is bounded and stays near full; it did not grow without bound.
- `fetch_hint_t1_waiting` reached its cap of 256 while marking ran. Permits in use stayed at 32.

## Verdicts
- **a. Both mechanisms run: yes.** In S+0–60:
  - 4,644 admission grants (2,117 hinted); 0 refusals.
  - 64,856 marked and 64,856 captured; 2,116 hint queries; 654 useful; 63,995 expired.
  - **Expired share 92–98% per 10 min. Useful share 28–32% of hint queries.**
  - Every fallback reason 0; 396 guard skips; actor capacity rejections 0.
- **b. Interaction against the hint-only screen (S+0–30):** mixed.
  - **Capacity rejections: yes.** They fell from about 865k–997k per 10 min to 0.
  - **Hint traffic: yes.** Hint queries went from 194–248 to 616–626, and useful from 45–63 to 175–199.
  - **Expired share: no.** It is 92–98% against 91–100%. Marking (about 20k per 10 min) still far outruns hinted grants (about 620).
  - **Caps: yes.** T1 holders (`dht_hints_held`) peaked at 2,302 and guard entries at 70, both under 20,000.
  - **Nothing ran away:** no counter ran backwards, and no lookup ran without ending (W stayed at 23.8–24.8 s, under the 25 s deadline). The only queue near its bound is the admission queue above, which levels off.
  - **Stored (screen only):** 23.2/min unstalled, against 18.0–21.8 for hint-only and 20.4 for ADM-only. Samples about 6,000/min. Fetch success 4.0–4.8% in S+0–30, against 3.4–4.9%.
- **c. No collapse: yes.** Stored was 23.2/min unstalled and 20.8/min stalled. The base lane stored 16.0 before its stall and 10.0 in it; the stage-1 controls stored 9.6–10.5 stalled. Fetch success was 4.5% unstalled and 5.2% stalled, against about 4.3–4.8%.
- **d. Within the preset: yes.** The peak 1-min rate was 174 q/s (cap 200). Outstanding peaked at 424 (cap 1,024).
- **e. Memory holds: yes.**
  - Hourly anon after S: 319, 349, 359, 349, 369, 335, 363 MiB.
  - Slope over the last 4 h: 9.1 MiB/h (earlier lanes about 320–400).
  - RSS max 451 MiB.
- **f. No panics, debug-assert messages or error bursts: yes.** Over 12 h: 0 panic lines, 0 assert lines, 0 ERROR lines, 1 WARN line.

## Restart drill
Run at 12:00Z, as root from `/opt/lodestar-run`: `docker compose --profile block restart lodestar`, with `LODESTAR_IMAGE` and `DOCKER_GID` set.
- The restart returned 0, and the container was healthy after 11 s.
- **Switches** were the same before and after: ADM, HINT_FIRST and SAFE_RESTART all `true`.
- **Data intact:** torrents and search docs were 7,870 / 7,870 before and 7,870 / 7,870 right after the restart. They reached 8,175 / 8,175 by +12 min.
- **Suppression reason** `resumed`, 0 s, so sampling went live at once.
- **Mechanisms after the restart:**
  - at +10 s: 152 marked, 596 samples sent;
  - at +12 min: 24,983 marked, 707 hint queries, 71,174 samples sent, 305 stored.
- **Logs** in the drill's 15 min: 0 panic and 0 ERROR lines.
