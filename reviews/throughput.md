# Throughput review (owner request, 2026-11-28)

The owner asked for "an ongoing review to ensure we're proceeding as quickly as possible,
without sacrificing quality". The runner runs it every 6 h (a scheduled task into the runner
session) with `python3 -I tools/throughput.py 24 state.json` from this branch, then acts on the
rules below.

## Speed signals and what the runner does
| Signal | Act when | Action |
|---|---|---|
| Slots active | fewer than `phase_total` and a ready slice exists | Launch the next queued slice the same hour |
| Green PR unmerged | over 30 min | Review or merge it now; the runner is the bottleneck |
| Open → merge | p90 over 6 h | Find the cause (review wait, CI, fix rounds) and fix that |
| Commits per PR | p90 over 10 | Briefs or rulings too vague: measure before ruling, tighten briefs |
| CI queue delay | p90 over 10 min | CI saturated: ask sessions to batch pushes; consider fewer slots |
| Slowest CI job | over 30 min | Track X-ci-test-shard; next target after it lands |
| Slot on one task | over 12 h | Check the session; unblock, split, or reassign |

## Quality guards (never traded for speed)
| Signal | Act when | Action |
|---|---|---|
| Red runs on `main` | any | Root-cause each one. A merge that broke main gets a fix PR first; a flake is fixed once (CLAUDE.md) |
| Fix, flake or revert merges | over 25% of merges | Tighten reviews and self-review; tell the owner |
| Review bar | always | Fresh reviewer per PR, /security-review where required, no skipped tests, no fixed sleeps |

Any speed-up that would loosen an owner decision, a gate, a budget the owner set, or a
security rule goes to the owner first (CLAUDE.md). Each run posts a short summary in the runner
chat; the first run after 18:00Z also lists the trend against the day before.

## Log
Each run appends one line to `reviews/throughput.log`: the time and the key numbers.
