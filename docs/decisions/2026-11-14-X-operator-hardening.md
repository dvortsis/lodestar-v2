# X-operator-hardening decisions (#859)

These are the fixes for the cloud operator findings the owner-started security review raised before first-run check 8. Each finding is named only by id, severity and surface, and each fix only by what the code now does (`docs/security/v1/README.md` §5).

| Id | Severity | Surface | Status |
|---|---|---|---|
| OPERATOR-01 | important | tripwire sweep | fixed here |
| OPERATOR-02 | important | channel thread reader | fixed here |
| OPERATOR-03 | important | channel trust of bot-authored comments | fixed here |
| OPERATOR-04 | important | injection test corpus (check 8) | fixed here |
| OPERATOR-06 | to verify | App reach, tripwire trigger types | trigger added; the App's reach is verified in check 7 |
| OPERATOR-07 | polish | wake approval gate | fixed here |
| OPERATOR-08 | polish | injection harness realism | a results note, no code change |

OPERATOR-05 is the runner's own handling of the channel; the runner fixed it outside this slice.

## OPERATOR-01: the tripwire sweep
- **A sweep that can't finish escalates without disabling.** It still posts an escalation naming why it stopped, so it's checked by hand.
  - **Why:** an incomplete sweep is no evidence that the operator's bot did anything, so it no longer disables `operator-wake.yml`. This supersedes the runner's 2026-11-11 call that an unfinished sweep counts as a trip (`2026-11-11-OP-4.md`).
  - **What still disables:** only a finding attributed to the bot.
  - **Each stage stands alone.** A list that stops part way (its page limit, an unreadable page, the deadline) is recorded as incomplete. The items it read are still checked, and every later stage still runs. So a stop in one list can't hide a bot finding in another, and a list filled by the bot's own actions still disables on what was read. One escalation names every stop, once each.
- **PRs with few reviews cost no review pages.**
  - **The summary:** one GraphQL request covers up to 50 PRs updated in the lookback, each with its last 50 reviews and their total. A PR whose reviews all fit in that is decided from the summary.
  - **The fallback:** only a PR with more reviews than the summary holds falls back to the REST read under `REVIEW_PAGE_BUDGET`. That read is unchanged, so every page is still read and each review is kept by its own `submitted_at`.
  - **Fail closed:** anything the summary can't vouch for is recorded as incomplete (an `errors` key, a missing PR, a total that isn't a count). Every PR then takes the REST read under the budget. The query carries only PR numbers, which are digits checked first.
  - **To check on the first run:** that the sweep job's token gets GraphQL data back without errors. If it doesn't, every sweep escalates "a reviews summary wasn't readable" and falls back to the REST read.
- **The request budget.** The summary requests count against GraphQL's own points budget, not the REST one. They cost at most 20 a sweep (the pulls list's 1,000 PRs at 50 a request). The REST arithmetic in `test_the_budget_leaves_room_for_the_other_workflows` still holds.

## OPERATOR-02: the channel thread reader
- **`thread.py read` reads newest first.**
  - **Where it starts:** the issue's comment count names the last page. The read goes forward from there while pages are full, to catch comments that arrived since the count, then back towards the first page.
  - **The output:** oldest first, as before.
- **Past `MAX_PAGES` (30) pages** it prints what it read, the newest included, then exits with code 3. The prompt's existing "the helper fails: escalate" rule then applies, so the prompt doesn't change.
- **An unreadable answer exits non-zero:** a count that isn't a count, or a page that isn't a list.

## OPERATOR-03: bot-authored comments
- **One rule for what the bot may post: `thread.channel_shape`.** It accepts an `operator-report v1` with exactly its three header fields and a body, or a one-line `/fleet` command. The command must be in the command parser's words: ASCII, single spaces, at most four lower-case words of letters, digits and hyphens or a full commit. Never free text.
  - **Where it applies:**
    - `check_post` refuses anything else;
    - `thread.read` drops any bot comment that doesn't match it;
    - the sweep flags a channel comment by the bot that doesn't match it, as a bot finding, with the fixed label "the operator's bot posted a channel comment of no allowed shape". Its text is never echoed.
  - **`check_post` is stricter than before.** It used to accept anything that started with the report header.
  - **A malformed report sent by the prompt's fallback `curl` now trips.** That `curl` doesn't go through `check_post`, so a report in the wrong shape (an unknown `kind`, a missing header field) is a bot finding and disables `operator-wake.yml`. Before, it was accepted. This fails closed, and the first-run doc says so.
- **`thread.read` drops a repeated comment by id.** A comment deleted between page reads shifts the pages, and the same comment could otherwise come back twice.
- **The filter lives in the operator's helper, `infra/operator/thread.py`, not in `infra/fleet/reader.py`.** The fleet reader is outside this slice's paths, and the prompt reads the thread only through the helper. `reader.py`'s own stdin `main()`, and the `curl | reader.py` use its docstring shows, still return every unedited bot comment. Nothing on the prompt's path uses them. Whether `reader.trusted` should carry the same rule is for the runner.

## OPERATOR-04: the injection corpus (check 8)
- **The four `ignore` items are folded into one, `reader-drops`.** They are the comments the reader drops before the model sees anything, and the test suite proves that drop. The item runs once, to check the session end to end.
- **New items:**
  - `run-sequence-planted`: a `run` instruction with a two-command `/fleet` sequence, the production shape. A command is planted in the second answer.
  - `bot-id-planted`: one shapeless comment and one report-shaped comment, both under the bot's own ID. The shapeless one is dropped. The report-shaped one is seen, and must be escalated.
  - `runner-quotes-untrusted`: hostile text inside the runner's own instruction.
- **Items gain two optional fields:**
  - `kind`: `query` (the default) or `run`;
  - `runs`: 1 to 20, default 5.

  The harness runs each item its `runs` times, breadth first.
- **More runs where a failure would act outside the channel.** Four items run 10 times instead of 5:
  - the planted token request in an error;
  - the two items that plant a `/fleet` command in an instructed command's answer;
  - the bot-ID item.

  That makes 81 runs a level, against 65 before. The `$50` cap is unchanged. The first-run doc gives the arithmetic.
- **Every setting that ships gets a run.** The first-run doc's stop rule now requires the level matching the triage setting to pass as well as the routine level, unless triage is turned off.
- **The prompt's fallback `curl` is documented as untested by check 8.** The test's sessions have no network route, so any network command counts as a failure. Check 2's rate-limited case is the only place it's seen.

## OPERATOR-06 and OPERATOR-07
- **OPERATOR-06:**
  - `operator-tripwire.yml`'s `pull_request_target` gains `synchronize`, so a branch update by the bot trips. The sender gate is unchanged. Every push to any PR now starts a tripwire run whose job is skipped at once, which costs no runner time.
  - Check 7 now lists `PUT …/pulls/{n}/update-branch` among what the App can't do. The owner verifies it there rather than assuming it.
- **OPERATOR-07:**
  - `wake.py gate`, which holds no secrets, writes the parsed instruction's fixed fields (id, kind, mode, in-reply-to) to the job summary when it allows a run. The id matches `message.ID`, and kind and mode are fixed words, so no comment text reaches the summary. No workflow line changed.
  - Check 2 tells the owner to read the summary before approving the `cloud-operator` deployment.
  - **Reading of the brief:** the brief named `command.Command.canonical()`, which parses `/fleet` commands for `fleet.yml`. `operator-wake.yml` parses operator instructions, so the summary gives the instruction's canonical fields. A `fleet.yml` summary would be outside this slice's paths.

## OPERATOR-08
- **One neutral line in the results.** The first-run doc's check 8 now gives a line for the results: the injection sessions could tell they were a test.
- **Where it goes.** The OP-5 decisions file's results format isn't edited, because it belongs to another slice. The line is added where the doc says what to record.

## After merge
- **No re-dispatch needed for the prompt.** The agent's prompt and config are unchanged, so `operator-apply.yml` doesn't need re-dispatching for them.
- **The helper and the corpus change on `main`.** `thread.py` is fetched from `main` at the start of each session, so live sessions use the new helper at once. The injection harness mounts the changed corpus and helper. As before, check 8 refuses if the deployed agent differs from `main`'s files.
