# Cloud screens, 2026-11-23 to 2026-11-24 (reported)

Five X-test-vm lane reports, copied on 2026-11-27 from each lane session's own report text, because the instances and their results files were terminated and the reports never reached the runner. Every number here is **reported**: it hasn't been re-derived from raw scrapes.

| File | Lane | Commit | Switches on | Measured interval (UTC) | Restart inside the interval |
|---|---|---|---|---|---|
| `allon.md` | allon | 6bec9388 | all 12 | 2026-11-24T04:23Z (suppression end) to 10:32Z; hours 0–5 full, hour 6 is 9 min | no; drill at 10:32:51Z |
| `combo.md` | combo | 6bec9388 | ADM, split, lookup end, preset slots | 04:24Z to 10:31Z; hours 1–6 | no; drill at 10:31:48Z |
| `admhint.md` | admhint | 9ea2e855 | ADM, hint-first | see file | see file |
| `tadmsoft.md` | tadmsoft | a91ab090 | ADM, soft timeout | 11:31Z to about 17:39Z (2026-11-24) | no; drill after |
| `tbase.md` | tbase | a91ab090 | none (shipped defaults) | 11:25Z to 17:37Z (2026-11-24) | no; drill after (summary only) |

**How they differ from CR-SPEC:**
- **Scraper:** each lane used its own 1-minute `/metrics` scraper, not the CR-COL collector (`tbase` and `tadmsoft` ran start-block's collector for segment 1, then their own).
- **Hours:** counted from suppression end, with no 60 min warmup.
- **Counter:** "stored" is the hour's increase in the lane's stored counter. The reports don't name the series, and they don't apply the carry-in bound or the network-searchable cohort. Stored to searchable conversion isn't reported.
- **Store:** fresh.
- **Design:** an on-only block (A/A for `tbase`), no control.

The rates justify host time. They don't establish a 24 h mean, a worst hour, a latency, or a gain from any one switch.
