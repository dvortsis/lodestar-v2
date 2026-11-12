# X-percolate-promoted-hold decisions

Spec: the runner's brief `briefs/X-percolate-promoted-hold.md`. It closes rv-754 I1's residual (`reviews/rv-754-29215524.md`) and the follow-up that `docs/decisions/2026-11-12-GS-0.md` ("A promoted torrent's pass isn't held") left open. Affects architecture §8 ("Saved searches → alerts") and SRC-2b's promoted queue (`docs/decisions/2026-11-02-SRC-2b.md`).

- **A promoted torrent's pass is now held the way a forward run is.** `promoted_due` sets `hold_unresolved: true`. Before, while `payload_sig` was backfilling, a promoted clone of `sig:<H>` alerted only if H was already backfilled. Otherwise its row was consumed and the alert was lost for good.
- **What "held" means.** A search counts as held on a row when `percolate` left it out and it reads a population-tracked field. Left out means it's not in `matched`, not in `failed`, and the run wasn't stopped. The fields are the ones `Node::population_fields` names. So the percolator follows whatever readiness test `percolate` applies and doesn't name `sig:` itself.
  - **Today only `sig:` is held,** because `percolate`'s test is `payload_sig`-only (`mentions_same_payload` and `SearchIndex::prefilter`).
  - **GS-2, RX-1 and ENR-11** get the promoted hold for free once they widen that test in `crates/search`, and add their field to `population_fields`.
  - **Two guards stop a row being queued for ever.** A search counts as held only when it reads a population-tracked field. And a row whose torrent is past the snapshot's highest id is done at once: say it was removed while waiting and it was the highest id. `percolate` passes over a search whose range is empty without saying so, and nothing can match a torrent the snapshot doesn't reach. That's the old consume behaviour for that case.
  - **Inferred, not explicit** (the runner's condition 2). `percolate` returns no held set, and giving it one needs `crates/search`, outside this slice. The inference is pinned by `the_held_set_is_what_percolate_holds`: in the middle of the backfill, `percolate` leaves the `sig:` search out of both `matched` and `failed` and runs the rest, including a `source:` search that's complete. `matched_promoted` then reports exactly the `sig:` search held, and none once the field is Ready. **Follow-up for the next slice in `percolate.rs`:** return the held ids explicitly and use them here.
- **Durable design: a row isn't recorded done while any search due on it is held** (the brief's second option). This needs no migration, because the queue rows are the same `settings` keys. Two additive store methods carry it, approved by the runner (2026-11-12) with the signatures given here:
  - **`Store::promoted_due_after(after, applied, limit)`** is `promoted_due` starting past seq `after`. `promoted_due` is now `promoted_due_after(0, …)`.
  - **`Store::record_promoted_rows(seqs, results, now)`** works like `record_promoted`, but drops exactly the rows in `seqs` rather than a prefix. A held row can then stay queued while the rows after it go. `record_promoted` is unchanged.
    - **Every seq must name a queued row, each once** (the runner's condition). Otherwise the call returns `Error::NotFound` and rolls back, so no row is dropped and no alert is made. Store tests cover a partial set, an unknown seq, a seq given twice, and the rollback of rows already deleted in the same call.
    - **`results` is per search, not per row,** so the runner's "paired one-to-one" can't apply as worded. A search's hits can come from a row that stays queued, because its other searches ran. Replaying those hits adds nothing. The store checks the seqs, and the alert inserts keep their existing checks (paused or edited searches get none).
- **The other searches on a held row run at once,** and their hits are recorded in the same transaction that drops the finished rows. The held row stays queued.
- **Bounded and live.**
  - **A cursor steps past held rows.** `Seen::promoted_after` (in memory) is where the next batch starts: past every row the last batch examined. So a full batch of held rows doesn't stall the queue, and the next run takes the rows after it.
  - **The cursor moves only once the batch's write commits.** After a failed write, the same batch is taken again.
  - **The cursor goes back to the head** when a batch reaches the queue's end or a row the index hasn't applied. The run after that retries the held rows, so each held row is retried at least once per cycle through the queue: every run when the queue holds fewer than `PROMOTED_BATCH` rows.
  - **Retries are cheap.** `Seen::held` keeps, for each held row, the searches still held on it. A retry runs only those, so a row's other searches don't run twice in one process. While the field stays partial, a held search is passed over before it's compiled.
  - **Nothing spins.** `more` is set only when the budget stopped the batch, or when a full batch percolated at least one row. A run whose rows are all held writes nothing and waits for the next commit or the idle poll (`promoted_poll`, 10 s).
- **After a restart** the in-memory state is gone. The cursor starts at the head, and every search due on a held row runs again. Alerts are unique per search and torrent (`ON CONFLICT DO NOTHING`), so a search that already alerted on the row adds nothing.
  - **Known limit (accepted by the runner):** if pruning removed that alert between the first pass and the replay, the replay adds it again as new. It's rare because three things have to coincide: a restart while the row is held, at least 1,000 newer alerts for the same search in that window (`keep`), and an hourly prune between them that drops the alert. We accept it rather than store per-search progress in the queue row.
- **Not changed (in `crates/search`, outside this slice):** the doc comment on `Due::hold_unresolved` still says a promoted pass runs at once. The search-crate test `one_id_passes_hold_forward_runs_but_not_promoted_ones` still tests `hold_unresolved: false`, which `percolate` still supports. Both are named in the PR for the next slice that touches `percolate.rs`.
- **Tests** (`crates/lodestar/src/percolator.rs`) reopen an on-disk index with `payload_sig` one generation on, which starts the walk again. They cover:
  - a held clone isn't alerted and isn't lost, then alerts exactly once after Ready;
  - an unrelated search on the same row alerts at once;
  - under a spent budget, the cursor steps past a held row, so the rows behind it go one per run;
  - a full batch of held rows doesn't stall the rows after it, doesn't spin, and retries only the held search;
  - repeated runs over an all-held queue only read: no alert, no write, no `more`;
  - the held set is exactly what `percolate` holds;
  - a restart while a row is held keeps it queued, and it runs once after Ready with no duplicate alert.

  A mutation that sets `hold_unresolved` back to `false` made the three behaviour tests fail. Two store tests cover the new methods.
