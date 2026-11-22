# X-filter-budget-manyfile-perf decisions

#895 (`2026-11-14-X-filter-budget-fix.md`) made torrents with many distinct words cost the core about twice the time per file. On the reference EPYC 7763, its worst case at the budget reached 14.5 ms against the 15 ms bar, and on this build VM, `main` failed the bar. This change wins that time back. It changes no charge.

## Calls

- **The overflow store for words (`WordSpill`) is flat** (runner: fix the measurement's cause, not its scaling).
  - **Before:** a word displaced past `kept` went into a hash map as a plan of its own: three buffers plus a cloned key, all freed when the next read started. A torrent with a distinct number in every file did that for nearly every file, so most of its time went to allocating and freeing.
  - **Now:** entries are fixed-size and sit in one array, copied in and out, and a freed entry is reused within the read. A word with more than six tokens keeps them in a buffer of the store's.
  - **Lookups:** the index is keyed by std's randomly keyed hash, as before, so no choice of words makes look-ups slow. Words whose hashes are equal are chained and compared by their bytes. Each entry stores its hash, so taking one back doesn't hash again.
- **`kept` has 2,048 places, and each word has two of them** (was 512, one place). A displaced word goes to whichever place is free, so many fewer reach the store. `kept` is the overflow table #895 added, not one of the tokenizer's caches: the caches (`PLANS` 512, `CHARS` 2048) stay the size the owner set.
- **Charges are unchanged.** Which words sit in the cache, and so every trade, is exactly as before. Each trade is still charged the same fixed amount whichever table holds the word, and a word is still charged as new once per read. #895's pinned-units tests are unchanged and green:
  - `typical_torrents_cost_what_they_did`;
  - `generated_inputs_reach_the_budget`;
  - `a_read_costs_the_same_whatever_came_before`.
- **Unchanged:** the 15 ms bar, the 50 µs gate, `BUDGET`, and the calibration and skew scaling. The withdrawn skew slice would have scaled cache-heavy shapes on the EPYC 9V45 by one factor. That would also have let a real slowdown in its other shapes pass, so the runner dropped it.
- **Memory:**
  - Between reads, the store keeps room for 1,024 entries (240 B each, about 240 KB). Before, it kept up to 1,024 plans with their buffers.
  - `kept` holds up to 2,048 plans of 88 B, plus their buffers as they're used: about 0.5 MB per tokenizer when full, against about 0.13 MB before. There's one tokenizer per thread that judges.
  - Within a read, the store holds at most about `BUDGET / PLAN` words, as before, since each was charged as new (checked in debug builds).
  - **The earlier reading's tokenizer (`read_split`):** it builds a new tokenizer for each read, so `kept` is allocated and freed per read: 2,048 × 88 B, about 180 KB, against 45 KB before. That only happens to a read that trades at all, which means a torrent with a mark character and more than 512 distinct words. Reusing the thread's splitting tokenizer there would avoid it; that's a follow-up, not part of this change.

## Numbers

The five shapes are the ones that ran slow on #907's EPYC 9V45 job: long CJK folder, alternating folders, typical Latin, spelled out, and long folder with a mark. Times are the core's raw best of five passes, in milliseconds, at each build's largest torrent the budget allows. #895 lowered that size: long CJK folder fell from 17,152 files to 13,696, alternating folders from 25,600 to 18,432, and typical Latin from 12,672 to 8,576.

### CI before this change (`filter budget` jobs)

| Runner, date                                              | Code        | long CJK folder | alternating folders | typical Latin | spelled out | long folder with a mark |
| --------------------------------------------------------- | ----------- | --------------- | ------------------- | ------------- | ----------- | ----------------------- |
| EPYC 7763, 2026-11-14 09:11Z–20:42Z, 11 jobs              | before #895 | 5.37–6.16       | 5.02–5.15           | 6.26–6.55     | 5.31–5.43   | 1.98–2.53               |
| EPYC 7763, 2026-11-14 18:05Z to 2026-11-15 02:23Z, 5 jobs | #895        | 10.17–14.30     | 10.35–14.53         | 8.11–10.31    | 5.64–10.77  | 2.67–4.02               |
| EPYC 9V45, 2026-11-14 10:54Z, 14:34Z, 17:57Z              | before #895 | 3.50–4.08       | 2.53–2.68           | 2.98–3.25     | 2.77–2.97   | 1.05–1.27               |
| EPYC 9V45, 2026-11-14 23:24Z                              | #895        | 6.02            | 5.82                | 4.15          | 5.24        | 1.28                    |
| EPYC 9V45, 2026-11-15 03:22Z (#907, failed)               | #895        | 15.40           | 16.33               | 11.30         | 7.22        | 3.35                    |

On #907's 9V45 job, calibration read 0.62–0.70× the reference's time, so its raw times were scaled up: long CJK folder read 24.42 ms, alternating folders 25.90 ms and typical Latin 17.92 ms, all over the 15 ms bar.

### CI after this change

| Runner, date                        | long CJK folder | alternating folders | typical Latin | spelled out | long folder with a mark |
| ----------------------------------- | --------------- | ------------------- | ------------- | ----------- | ----------------------- |
| EPYC 9V45, 2026-11-15 04:34Z (#910) | 4.04            | 3.00                | 2.79          | 3.20        | 1.05                    |

On that job the worst case on the reference scale was 8.86 ms (ASCII words read anew), against 25.90 ms on #907's 9V45 job, and the 50 µs gate read the mixed set at 43.0 µs.

### This build VM, back to back

An Intel Xeon at 2.10 GHz with 4 vCPUs and a noisy host, calibrating at 1.02–1.03× the reference.

| Shape                                       | before #895 | `main` (#895)        | this change |
| ------------------------------------------- | ----------- | -------------------- | ----------- |
| long CJK folder                             | 7.05        | 12.11                | 6.34        |
| alternating folders                         | 4.77        | 12.04                | 5.53        |
| typical Latin                               | 6.23        | 8.47                 | 5.06        |
| spelled out                                 | 5.05        | 5.36                 | 5.68        |
| long folder with a mark                     | 2.36        | 2.96                 | 2.69        |
| ASCII words read anew                       | 6.43        | 12.52                | 9.85        |
| worst of all shapes, on the reference scale | 7.85        | 16.20 (over the bar) | 9.86        |

### Instructions (callgrind)

Instructions inside `Core::verdict`, three verdicts of the same torrent, at `main`'s sizes.

| Input                                               | `main`  | this change |
| --------------------------------------------------- | ------- | ----------- |
| long CJK folder, 13,696 files                       | 195.0M  | 116.1M      |
| alternating folders, 18,432 files                   | 307.8M  | 193.3M      |
| typical Latin, 8,576 files                          | 255.4M  | 196.3M      |
| ASCII words read anew, 75 files                     | 254.8M  | 200.6M      |
| distinct Arabic, 81 files                           | 252.2M  | 199.8M      |
| the 50 µs gate's 512 typical torrents, three passes | 166.77M | 167.13M     |

The typical set is 0.2% higher, from checking two places on its 6,048 trades. That's still under the 168.6M measured before #895. On this VM, the 50 µs gate reads the mixed set at 52.3 µs on `main` and 51.9–53.0 µs here, with the mixed time over the calibration at 5.0 on both. The VM's CPU isn't in `ENGINE_SPEED`, so both builds read over 50 µs here. CI holds the gate.

## Tests

- `a_metered_read_reads_what_an_unmetered_one_does` (`crates/filter/src/normalize.rs`) gives the same text two reads:
  - **metered:** its words trade places through `kept` and the store;
  - **unmetered:** it never trades.
  - **The check:** the tokens must match. The texts use vocabularies of a few hundred to thousands of words, words with more tokens than an entry holds, and 3,000 distinct numbers. A store that changed a plan on the way in or out fails it; with a token dropped on the way out, it and the three tests around it fail.
  - **Charges pinned:** each case's work and cold work are folded into one number. It's the same on `main` before this change, so a word lost from a table and planned again fails it even where the tokens still match. A mutation that drops a word on its way to the store fails it.
- `a_word_spill_chain_keeps_each_word` forces three words onto one hash. Taking back the chain's first, middle or last word returns it whole and leaves the other two found, and its entry is reused.
- `a_word_spill_keeps_long_plans_and_clears`:
  - plans with more tokens than an entry holds come back whole;
  - their buffer is reused after a clear;
  - room past 1,024 entries is freed.
