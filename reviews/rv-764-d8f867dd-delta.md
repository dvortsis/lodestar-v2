# Runner delta self-check: PR #764 (ENR-14), 5a96f302 -> d8f867dd

VERDICT: MERGE (on green ci-ok, web-ok, e2e-ok)

Delta: 65284e76 (the fix round, 5 files +92/-15) and a clean merge of origin/main at 925f69a9.

- **B1 fixed.** The `aliases` map's `langs` list is checked against `max_texts` before any scan; past it, keys are skipped and the entity is marked `truncated`. So each key costs at most a 64-entry scan. `many_empty_alias_lists_are_read_in_linear_time` reads 300,000 empty lists whole, with a 20 s bound (a deadline, not a sleep). The label repeat check is now case-insensitive and is bounded by the kept labels.
- **I1 fixed.** Every id the search named must be in the entities answer, or the lookup fails with BAD_ANSWER: covered for an empty answer, a `missing` entry, and a redirect's target key. A `missing` item, deleted between the search and the fetch, is now a retried error, never a cached miss. That's the conservative reading, and it's accepted.
- **Also:** a title that folds to nothing sends no request (tested with a silent wire).
- **I2:** the User-Agent stays; the §2.19 owner flag is in the runner's FYI batch.
- **I3:** the decisions file lists `credential_spec.rs` and `manifests.rs`.
- **I4:** a logged condition for the consent-lift slice.
