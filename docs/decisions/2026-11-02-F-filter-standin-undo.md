# F-filter-standin-undo decisions

This follows `2026-11-01-F-filter-load-fail-closed.md` (#357). It undoes what stand-ins did once their rows read again (an upgrade back, a repaired row), where that can be told apart safely, and says what can't. §20 says false positives must be correctable. The runner set the scope under the owner's content-safety rules. **No schema change and no migration.**

## Recording which rows failed closed
- **What was missing:** which rows failed closed, and when, across restarts. #357 kept that in memory only (`Engine::unreadable`, reset on restart), so after an upgrade back nothing knew.
- **Where it's kept now:** one `settings` row, `filter.stand_ins` (`lodestar_filter::load::StandIns`). It's a value under a new key in an existing table, not a schema change.
  - **One entry per row that fails closed:** its label, the time it was first found (`since`), and, once it no longer fails closed, the time that was first seen (`until`). Example: `U-3@1762041600 C-2@1762045200~1762090000`.
  - **Labels and times only, never an item,** so it isn't the record of releases that §20.11 rules out. #357's file says these labels are never stored; that no longer holds for this record.
  - Labels this version doesn't know are kept as they are.
- **When it's updated:** at every start, and after every filter change once the engine is reloaded (`FiltersApi::undo_stand_ins`).
  - `since` is the load's time, before any batch is judged on that engine.
  - A rule entry counts as recovered only once the stored row reads (or is gone), checked in the store, so an engine that hasn't loaded the rules never releases anything.
- **Only from this version on.** The version that can't read the row is the one that writes the record. A rollback to a version from before this slice records nothing, and #357's manual steps still apply to holds made under it.

## A rule's stand-in (query, action or `unless` unreadable): re-judged
- **What a stand-in does, by kind (#357):**
  - An unreadable query or action makes a stand-in that **quarantines**.
  - A lost `unless` keeps the **rule's own action**. A drop rule then leaves tombstones, and a hide rule stamps `HIDDEN` on stored rows. That action isn't turned into a quarantine: that would weaken #357's "keep the stronger action" (runner, review of #367).
  - Every stand-in's decisions are stored under the rule's own id, never as an adult hold (an adult hold is always an Adult-only core rule's).
- **Telling its holds apart:** they are the rows of `U-n` with `since ≤ at ≤ until`.
  - Before `since` they're the real rule's.
  - After `until` the real rule, readable again, made them.
  - Rows from the second `until` falls in can be either. They're re-judged, which costs at most a refetch.
- **Only quarantine is re-judged.** A tombstone or a hidden row keeps no name to judge, and a tombstone's refetch is what it exists to prevent. So `U-n`'s tombstones in the window are counted (`dropped`), and so are its hide decisions that `filter_hits` still records (`hidden`, within that table's retention). Both are left as they are, for review in Filters & Safety.
- **How they're re-judged:** a chunk of 512 at a time, under the control store's lock.
  - Filter changes reload the engine under that same lock (§20.13: the engine changes first, then release), so the engine can't change partway through a chunk.
  - Each row goes through `Engine::holds`, with its hash and its sealed name, classified from that name as an import candidate is (`reclassify::judge_candidate`): the floor (CS-1 and CS-8 hashes), lists, *Always allow*, CSX-5, packs, your rules with their exceptions, and your classification rules.
  - A row the engine would quarantine or drop stays. Anything weaker (hide, demote, flag, nothing) is released.
  - **A sealed name that won't open stays held:** it can't be judged.
- **A release is judged again in full.** Releasing only makes the hash fetchable again. When intake fetches it, the writer judges the whole torrent, files, sizes and classification included, before anything is stored. Rules that read files or sizes are applied then. This is how every release in Filters & Safety works (§20.10), and nothing is shown or stored in between.
- **Rows a writer has replaced stay.** A row is deleted only if its hash, rule and `at` are still as read (`Store::release_rows`). Intake's held set follows the store under its own lock (`HeldSet::remove_released`).
- **Editing or removing an unreadable rule** still releases all its holds first, as #357 did (behind the password). The re-judge then finds nothing left.

## An Is adult classification rule's stand-in: adult holds stay
- **They can't be told apart safely.** An adult hold (`…+adult`) is made when your **per-torrent correction** or a classification rule makes a torrent adult that detection didn't. Its row records neither which one did it nor which rule. The correction is deleted with the stored torrent, so a refetch can't make the hold again (§20.9; the purge comment in `crates/store/src/filter.rs` says the same).
- **Re-judging them would be a loosening.** Re-judging by name would release a hold that your correction made while the rule was unreadable, and the Adult-only child-safety checks would never see that torrent again. That's not a readable rule's own decision, so per the brief it **stays held**, in quarantine and as a tombstone, before and after the 30-day purge.
- **What happens instead:**
  - Every stored torrent is queued to be classified again (`classify.requeue_all`, set before the entry leaves the record). Re-enrichment relabels them with the readable rules and your corrections.
  - The adult holds made in the stand-in's windows are **counted for review** in the Insights notice: one count of distinct hashes over every recovered entry's window. It's an upper bound, because a readable Is adult rule or a correction may have made some of them, and the notice says so. The review queue releases them one at a time, with the adult-hold warning and the password (§20.11).
  - Removing an unreadable Is adult rule does the same.
- **Proposal for the runner (not built):** to release these automatically, an adult hold must record what made it adult.
  - That means a change to the stored rule-id format, for example `CS-5a~C-2+adult` for a classification rule's hold and the plain `+adult` for a correction's. The `+adult` suffix keeps older versions' purge and adult-hold reads working.
  - It also means the writer and reclassify paths passing the deciding rule's id (`ClassRules::decide` would return it).
  - It's an on-disk format change across `pipeline` and `filter`, so it's a decision for the runner and owner, not this slice.

## Gates and surfacing
- **No password for the automatic re-judge.** That's the runner's position, and my reading of §20.11 agrees: it releases only rule stand-in quarantine that the rules you wrote, now readable, wouldn't hold, and each release is judged again in full before anything is stored. The stand-in was the engine's fallback for a row it couldn't read, not a protection you chose. Removing, disabling or editing a rule still asks for the password.
- **No manual "release all stand-in holds" action.** The review queue already releases items one at a time, with the warning and the password.
- **Insights notice `filter_stand_ins_rejudged`:** a warning for 7 days, also shown when a run stopped partway.
  - `value` is how many were released.
  - A new optional field, `counts`, holds `kept`, `adult`, `dropped` and `hidden`.
  - It's stored as `filter.stand_in_undo` (`{at} {released} {kept} {adult} {dropped} {hidden} {partial}`), counts only.
  - A run that follows one a stop cut short adds to its `released`, which is final. It replaces the other counts, which that run counts again.
  - New strings, en and zh: the notice and the log event's name and help. The digest gets a matching line.
- **Log event `filters.stand_ins_rejudged`** (Info): the counts `released`, `kept` and `review` (adult, dropped and hidden together), never a hash, name or rule text.

## Where it runs, and its edges
- **At start,** in the start's background task, before the pending re-queues.
- **After each change,** on a blocking task the change's request starts once it's answered, so a request never waits for it (runner, review of #367). Removing an unreadable Is adult rule already queues every stored torrent to be classified again, so that run skips its own full re-queue.
- **Over-strict edges** (each costs a refetch or keeps a hold, never releases one the rules hold):
  - `until` is taken when the re-judge first sees the row read again, after the reload, at start possibly after a few batches the readable rule judged. Those holds are re-judged too.
  - `since` is kept from the first time the record was written. A version that never wrote one leaves earlier stand-in holds alone.

## Budgets
- The hot path is unchanged. The record is one `settings` row, read and written at start and after each change.
- A re-judge costs one judgement per row in a stand-in's window, once, after its row reads again, using the `quarantine_rule` index.
