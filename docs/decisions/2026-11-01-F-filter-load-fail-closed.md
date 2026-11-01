# F-filter-load-fail-closed decisions (#TBD)

This supersedes the FX-6-I9 decisions' statement that `load.rs` skips a stored row it can't parse. FX-6-I9's stage-2 plan still stands.

A security review of #350 (FX-6-I9) found that `crates/filter/src/load.rs` dropped any stored row it couldn't parse, with only a note. A rule that no longer parses (newer syntax after a rollback, a corrupted row, a parser regression) then silently stopped hiding, quarantining or dropping, and an Is adult classification rule silently stopped marking. That loosens filtering. This slice only tightens: the runner decided it under the owner's content-safety rules (§20). No migration and no schema change.

## Design: what an unreadable row does, per kind
Losing a row that **withholds** loosens filtering, so it fails closed. Losing a row that **releases** only tightens, so it's skipped. Either way the row is listed in `Engine::unreadable` and raises the visible exception below. "Unreadable" means its query doesn't parse, or a stored code (action, strength, match kind, rule id) isn't one this version knows.

| Kind | Readable effect | When unreadable |
| --- | --- | --- |
| Your rule, enabled, action hide / quarantine / drop, query unreadable | withholds what its query matches | **Quarantines every torrent that reaches your rules outside its `unless`.** That's the only scope known to cover what the real query matched. Its exceptions, *Always allow* and CSX-5 still release from it, as they would from the real rule. |
| Your rule, query readable, action code unknown | an action from a newer version | **Quarantines within its own query and `unless`.** |
| Your rule, disabled, or flag / demote | withholds nothing | Skipped. |
| A rule's `unless` | narrows its rule | **Dropped, so the rule applies without its exception** (stricter). |
| Classification rule, Is adult | marks matches adult, which adds the Adult-only core checks | **Matches everything in its place:** a torrent is adult unless a newer, readable Not adult rule matches it. |
| Classification rule, Not adult | marks matches not adult | Skipped (more adult, never less). |
| CSX-5 entry, exception, *Always allow* entry | release | Skipped. |

- **Why quarantine, not the rule's own action.** Hide stamps `flags::HIDDEN` on stored rows, and nothing clears it when the rule is fixed. Drop keeps a tombstone. A quarantine keeps only the hash, rule id and sealed name, expires after 30 days (§20.3), and can be released. So it's the strongest action that stays reversible. A drop rule's own matches are quarantined rather than dropped while it's unreadable. Neither is stored or searchable, so nothing is exposed.
- **A stand-in never pre-empts a stronger decision.** Your rules are first match wins. A stand-in that matched everything would otherwise stop a later rule, pack or list from dropping a torrent the real rule never matched. Instead the engine notes the stand-in's quarantine, carries on through the later rules and the pack layer, and applies the stronger of the two (`Engine::stand_ins`). The cost is paid only while a stand-in exists.
- **Why not apply option (b), "keep the last good compiled form".** Nothing stores one, and storing it would need a schema change. At start the engine has only the stored text.
- **Why not a narrower guess at the query** (for example, dropping only the clause that doesn't parse). Taking clauses out of a conjunction widens it, but that holds only if the outer structure was read correctly, and the text didn't parse. A wrong guess would loosen filtering, so this doesn't try.
- **The cost is broad, and it's meant to be loud.** While a withholding rule is unreadable, new torrents go to quarantine instead of the index, and the app keeps working: search, existing torrents and Filters & Safety all do. The exception is an **error** and names the row. This happens only after a rollback, a corrupted row or a parser bug, all of them rare.
- **A shadow-mode rule keeps its shadow period.** While it lasts, the stand-in records what it would quarantine (`filter_hits` with the shadow bit, within that table's retention). After that it acts.
- **An unreadable Is adult rule also stores what it marks as Adult** and judges it with the Adult-only core. A torrent that only those checks hold becomes an adult hold. The fix path below classifies everything again. Adult holds made meanwhile show in the review queue as `adult_hold` and can be released there.
- **The locked floor (CS-1) is untouched.** It's judged before any of these layers, and its hashes and signatures load from the filter state, not from these rows. Tests cover it at start and on apply: `crates/filter/tests/load_fail_closed.rs` and `crates/pipeline/tests/filter_load.rs`.

## Fix path and surfacing
- **Insights exception `filter_rows_unreadable`** (§10.9.2 level 5). It names up to 20 rows as the Filters & Safety screens show them (`U-3`, `C-2`, `O-4`, `E-5`, `A-6`) in a new `rows` field on `InsightsException`, with the total in `value` (unit `rows`). It's an **error** while any rule, `unless` or classification rule is unreadable, and a **warning** while only releasing entries are. `since` is when the first row was found. It persists across reloads and resets on restart. The fix link goes to `/settings/filters`. The digest says the same in its alerts. The `rows` labels are display ids only, never stored.
- **`/metrics` gauge `filter_rows_unreadable{kind}`** (`docs/metrics.md`). It's a gauge, not the `_total` counter the brief suggested, because what matters is how many rows are unreadable now. It drops to 0 when they're fixed.
- **The start log** keeps `pipeline.filter_rule_skipped` with the new reason `fails_closed`. The note text is still never logged.
- **Fixing clears it.** Every load recomputes the list, so editing or removing the row clears it on the next apply. **Updating a rule whose stored form was unreadable releases its quarantine** (as removing one already did), so whatever the stand-in held is fetched and judged again by the fixed rule. **Removing an unreadable classification rule re-queues every stored torrent** for classification, since it matched everything while it was loaded (`requeue_text` now reads unparseable text as `Node::All`; before, it matched nothing).
- **One new string, en and zh** (`insights_overview_exc_filter_rows_unreadable`, in the Insights feature's own messages). The generic fallback would have shown the raw kind, which tells the user nothing.
- **Budgets (§20.8).** The hot path is unchanged when every row reads. While an Is adult rule is unreadable, every torrent is judged twice (as detected and as adult), as an Is adult rule that matches it already does.

## Known limitation: fixing by upgrading back
The repairs above run when you edit or remove the row in Filters & Safety. If instead you upgrade back to a version that reads the row, the exception clears on that load, but what the stand-ins did stays:
- torrents stored meanwhile keep the Adult label an unreadable Is adult rule gave them, until they're next re-enriched;
- quarantines made by a rule's stand-in stay until they're released or their 30 days are up;
- adult holds made meanwhile become tombstones at that purge (§20.3).

Repairing this automatically needs a record, across restarts, of which rows were unreadable. That's new persisted state, beyond this no-schema slice. It's a follow-up for the runner to schedule. Until then, after an upgrade back, release from the review queue anything held by the rule, and remove and re-add the classification rule to re-classify.

## Refinements for the runner to flag (§2.19)
- An unknown action code now fails closed (quarantine within the query). Before, the rule was skipped.
- Unreadable flag/demote rules are skipped, not applied to everything: they hold nothing back, and flagging everything would flood the review queue.
