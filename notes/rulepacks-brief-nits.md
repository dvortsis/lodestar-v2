# Carry into the RP-0 and RP-3 briefs and the slices.yaml rows (from #412's final review, 2026-11-04)
1. **Shadow disagreement:** define it for both `widens` and `action-heavier`, and add a mislabel gate for `action-heavier`.
2. **Seeding:** SC-2 is seeded at version 1. Copy SC-1's `filter_exceptions` rows to SC-2 in the same transaction.
3. **Applied rows:**
   - a row that exists but won't read counts as state;
   - two rows that read but differ: take the stronger version per rule;
   - a missing marker with applied rows present must never seed (add a gate);
   - report any seeding on a store whose `rule_set` isn't empty.
4. **Both applied rows lost:** define which version counts as "unaccepted" for the quarantine cap.
5. **Downgrade:** a rewrite merges per rule and keeps a newer version this build doesn't know. Add that rewrite to the downgrade-then-upgrade gate.
6. **RP-0 `owns`:** add `crates/store/src/rules.rs` and `crates/api/src/filters.rs` (pack_switches/settings writes, the full-state write, replace_rule_set).
