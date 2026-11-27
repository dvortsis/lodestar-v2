<!-- Reported, not verified. Copied on 2026-11-27 from the X-test-vm lane session's own report text (its transcript); the instance and its results file were terminated with it. Not a CR-SPEC run unless the text says otherwise. Never evidence of a gain or of the gate. -->

**tbase final report** (shipped defaults; a baseline lead, not an A/B effect)
- **Commit** a91ab090fdbf (contains #1091), local image 04aa019e613e. No local diff.
- **Setup:** an ordinary ABBA start-block; collector-1 was SIGKILLed at 05:29:26Z, so segment 1 (the control) kept running.
- **Environ:** `COORDINATED_ADMISSION=false` (the default, set explicitly by start-block) and `SAFE_RESTART=true`. No other switch.
- **Leak tests:**
  - Run 1: 49 ok, one fail (`vpn_interface_held_down`, the known flake), 0 leaked.
  - Run 2: pass, 55 ok.
  - start-block's own run: 66 ok.
- **Timeline:** Lodestar started 05:25:01Z; suppression ended 11:25:08Z; crawl ran until 17:37Z.
- **a. Baseline rates: yes.**
  - After suppression: 11.51 stored/min, 747 samples/min, fetch success 4.56 %, 65.6 q/s.
  - Unstalled (45 min): 19.47 stored/min. Stalled (328 min): 10.42 stored/min.
  - Per UTC hour 11–17 stored/min: 11.57, 10.87, 9.30, 9.80, 11.82, 10.88, 11.56.
  - Lookup ends after suppression: values 3,183, no_values 595, deadline 23,965 (86 %), cancelled 127, query_cap 0.
  - Capacity rejections per hour: 3.0–4.8 M. W about 24.7 s, q about 36.7, y 0.31 falling to 0.13–0.16.
- **b. Table-full stall: yes, reproduced.** `table_filled_after_seconds` 2152. Stalled 11:58–17:26Z (**328 min**). Sampling then recovered by itself at about 5,990 samples/min.
- **c. No collapse: yes.**
  - Stored/min: 19.47 unstalled against the 16.0 lead; 10.42 stalled against 9.6–10.5 and 10.0.
  - Fetch success: 4.56 % (4.23–5.60 % per hour) against 4.3–4.8 %.
- **d. Within the Balanced preset: yes.** Highest q/s 155.5 (limit 200); outstanding max 398 (limit 1,024).
- **e. Memory holds: yes.** Hourly anon 313–358 MiB after suppression, max 403 MiB, RSS max 436 MiB. Slope over the last 4 h about 0 MiB/h (leads 320–400 MiB).
- **f. Clean logs: yes.** 0 panics, 0 debug-assert messages, 0 ERROR, 1 WARN, 0 container restarts.
- **Restart drill: pass.**
  - Restarted at 17:39:50Z, came back healthy and logged `resumed`. Switches unchanged.
  - Torrents 4330→4331 and search docs 4330→4331, so nothing was lost. No new suppression.
  - 14 min after the restart: about 21 stored/min and about 6,100 samples/min. Cooling was restored at 198,844, so the stall should come back soon.
- **Results file:** `~lodestar/results/tbase.md` on the instance and the scratchpad copy have the same sha256 (a14e8e8f…). The copy's local path is withheld from the repository.
- **Teardown:** stack stopped, instance terminated at 17:56Z, root volume deleted (`InvalidVolume.NotFound`). The tag check shows only two other lanes' in-use volumes, none left `available`.
- **Instance hours:** 12.8 h (05:07:43–17:56Z), about $2.20.

done