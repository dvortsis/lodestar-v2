# X-filter01-hardening decisions (#855)

Non-blocking findings from the review of X-filter01-fix (#843), named by the runner. Every hold stays fail-closed; these tighten it. Detail of the fix it follows stays held under `docs/security/v1/README.md` §5.

## Calls
- **The meter charges a path read twice in full** (§20.8). Where a path doesn't read the same with its segments apart, the core reads it again, so the meter now adds the cost of its own first read of the file name to the folder's cost, the least the path costs and its tokens. The second read can find the caches cold, so it's counted as the first read cost, never less. Runner, by message. A `budget_worst_case` shape covers it ("long names read twice"); the bench's bound and the CI budget limit are unchanged.
- **The locked-exception guards are tested:** a stored exception row for CS-0a, which never passed validation, is skipped on load with a note, and the torrent is still held. The engine's own guard, which no public judgment reaches (every engine entry holds an over-budget torrent first), is tested directly for every core rule. Both are mutation-checked.
- **Kept holds stay through every release but their own** (§20.7): a release by rule, of a rule's hashes, or of rows as read now skips `KEPT_RULES` rows, as each already skips a worker's floor report. Only `Store::release`, behind the API's warning and password, lifts one. `is_kept_hold` is the Rust form of `kept_hold_sql`, and a test keeps the two equal. Runner, by message: defence in depth behind the API's lock check.
- **The worst-case wording** in `docs/architecture.md` §20.5 and on `BUDGET` now matches the decisions file of X-filter01-fix: under 15 ms on the reference runner, 2–12 ms measured.

## Numbers
- **Held in the repo's corpora: 0 of 515.** That's 198 torrents in the rule packs' corpora and 317 release names in the release parser's golden files (`the_repos_corpora_are_judged_whole`). These are small, made-up torrents. They show what an ordinary torrent costs, not what share of a real crawl the budget holds. No realistic crawl sample is in the repo, so the runner takes the count the owner asked for to the owner.
- **`budget_worst_case`:** 28 shapes, worst 8.8 ms scaled on the build VM in this slice's run (bound 15 ms).

## Follow-ups
- **In X-filter01-rejudge** (runner): a CS-0a hold is never re-judged from its sealed name alone.
