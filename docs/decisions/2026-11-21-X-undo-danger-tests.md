# X-undo-danger-tests decisions (#1030)

Runner, 2026-11-21Z, after #1027 (X-safety-undo-danger) merged: its fresh review's one important finding, that two branches of `Assessment.loosens_child_safety` had no test, plus the review's optional nits. Tests only: nothing in `crates/api/src/filters.rs` outside `mod tests` changes.

## What each new test pins

Each test fails when the branch it covers is deleted or narrowed (checked one at a time, each caught by its own test only).

| Branch (`crates/api/src/filters.rs`)                                                             | Test                                                                                                                                                                                   | Mutation that fails it                       |
| ------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------- |
| `a.loosens_child_safety \|= a.preview.removed_from_floor > 0`                                    | `a_floor_removal_loosens_child_safety`: removing a pending floor entry sets the flag; keeping it doesn't                                                                               | deleting the line                            |
| the same rule, for unsubscribing or pausing a floor-marked list (`subscriptions::floor_leaving`) | `crates/api/tests/filters/subscriptions.rs`, `a_floor_marked_list_warns_that_its_entries_leave_the_floor`: both now assert the flag beside `removed_from_floor == 2` (runner's review) | moving the rule into the floor-removal arm   |
| `release`: a tombstone's rule from the snapshot                                                  | `releasing_a_cs_tombstone_loosens_child_safety`: a hash only CS-4a tombstoned sets it; one MW-4 tombstoned doesn't                                                                     | dropping the tombstone half of the condition |
| `undo_preview`: `before.load_rules(&current)`                                                    | `undo_reads_the_rule_set_as_stored_when_the_engine_lags`: with the engine lagging the store on a pack switch, Undo still counts the pack it turns back off                             | deleting the reload                          |
| `loosened`: the exception key includes its value                                                 | `undo_compares_exceptions_by_value`: a kept CS exception that differs from the current one only in its hash counts as put back                                                         | dropping `value` from the key                |
| `loosened`: the override key includes its strength                                               | `undo_compares_overrides_by_strength`: a kept Override where a Neutralize term on the same word is now counts as put back                                                              | dropping `strength` from the key             |

## Left out

- **`"default": false` on `loosens_child_safety` in `openapi.json`:** it would need a `#[schema(default = ...)]` on the field in `crates/api/src/dto/filters.rs`, then a regenerated `openapi.json` and `schema.d.ts`. All three are outside this slice's owns, and the generated types don't change with it (the field is already optional). Left for a slice that owns them, if the runner wants it.
