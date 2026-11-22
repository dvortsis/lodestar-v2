# CR-UTP-FIRST-WINDOW (runner brief, 2026-11-22Z)

Kind: core, model opus. Dep X-flake-utp-lossy-win is merged (#1008). **Analysis first.**

## Question
#1008's probe found that about 1.25–1.8% of honest fetches over a 15%-loss uTP link miss the 4 s first-window deadline (`FIRST_WINDOW_DEADLINE`, crates/peer-wire/src/fetch.rs), at 4.03–4.57 s. The test now passes `FetchOptions.first_window_deadline` explicitly, and production is unchanged. Re-judge the deadline for lossy uTP links.

## Rules
- **Analysis first:** a short write-up in the decisions file covering:
  - what the first window bounds (slow or hostile peers holding a fetch slot);
  - how the uTP retransmit and RTO behaviour adds to the time to the first window at 5–20% loss;
  - the measured miss rate at loss levels 5, 10, 15 and 20% (in-process, using the existing lossy test harness);
  - the cost of each candidate.
- **Candidates:**
  - **(a) keep 4 s:** document the miss rate;
  - **(b) a transport-aware first window:** uTP gets more time only while its retransmits show progress, with the bound against a silent peer unchanged;
  - **(c) a longer deadline overall.**
- **Who decides:**
  - (c), and any change that loosens the resource bound against slow peers, is the **owner's**. Write it up and stop; don't build it.
  - A change that keeps the bound, such as (b), is the runner's call (§2.26), so send it at step 0 for a decision.
- **No network beyond loopback.** Tests use the in-process lossy link. No fixed sleeps; any wait at least 10 s. The test may take about 20 s at most.

## Step 0 (send before building anything beyond measurements)
1. The measured miss rates per loss level, with how many fetches each.
2. Your recommendation among (a), (b) and (c), with what it changes and the bound it keeps.
3. Files and an estimate.

## Owns
- `crates/peer-wire/src/fetch.rs`
- `crates/peer-wire/tests/utp.rs`
- `docs/decisions/*-CR-UTP-FIRST-WINDOW.md`
- this slice's `slices.yaml` entry

## Branch and PR
Branch `slice/CR-UTP-FIRST-WINDOW`, PR title "CR-UTP-FIRST-WINDOW: <title>". Push once per round, then send "ready <pr> <sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
