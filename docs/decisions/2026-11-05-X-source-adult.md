# X-source-adult decisions (always-adult sources)

Slice X-source-adult (runner brief, 2026-11-05). It closes the OPEN item in `2026-11-05-SRC-10e.md`: owner Q7 says every Sukebei item is "forced into the Adult category, hidden unless Show adult content is on", but nothing in the pipeline read the `content-type` claim. Spec: architecture §18.1, §18.5, §20.7, §20.9. This file supersedes that OPEN item. SRC-10e's HOLD on Q2 (terms and robots) still stands.

- **The signal is the source's manifest.** It isn't the item's claims. `Manifest::always_adult` (`crates/sources/src/manifest.rs`, default `false`) is set only on `SUKEBEI_MANIFEST`. It's a property of the plugin's code, so a feed can't turn it off. The `content-type:adult` claim stays, for information only: the pipeline never reads it, so a claim can't make anything non-adult. At start the pipeline builds a bitset of the registered always-adult sources (`source_adult::AdultSources`, the same form as a torrent's `sources`). While none is registered it's empty and costs nothing. Today that's always, because Sukebei isn't registered.
- **Applied as an Is adult correction is, stricter only** (`crates/pipeline/src/source_adult.rs`). The item is stored `ContentType::Adult` with detection's adult kind, or failing that the nearest one (`Classification::as_adult`, confidence 100). Filtering judges both detection's reading and the adult reading, and the stricter verdict stands, as `Engine::judge_classified` does for an Is adult rule. So the floor's hit comes back alone and first, the child-safety core applies to both readings, and the Adult-only core (CS-5a, CS-5b) checks the adult reading whatever the name. When the Adult-only core holds such an item, it's an adult hold: a tombstone when the quarantine ends (§20.9).
- **It's derived, never stored.** A torrent is source-adult when its `sources` bitset (or, for an unverified item, its finders) includes an always-adult source. Nothing is written to `class_corrections`, so the classification never pretends you corrected the item, and the store schema is unchanged. Each place that classifies applies it:
  - **prepare** (the worker): the fetch's sources;
  - **commit**: the writer re-judges a result when the lease's source or an unverified item under its hash is always adult and the worker didn't know. That covers remote workers, older rule sets, and a DHT fetch of a hash Sukebei only claimed. Re-judging (`writer::rejudge`) never lets a classification rule un-adult it;
  - **promotion**: the item's claims are judged as adult again;
  - **claims** stored, and swept by the refile (`unverified::judge` with `adult`);
  - **a stored torrent an always-adult source finds** (claims on it, a bare sighting, or a merge): it's classified as adult at once, in its own transaction after the commit (`enrich::adult_by_source`). If the rules changed under it, it's marked for the backfill, which does the same. A torrent the DHT stored first with a clean name was already visible before Sukebei saw it, so the short gap isn't a regression;
  - **the background enrichment** (`enrich_stored_marking`, after a parser upgrade): `source_adult::correction` adds Is adult from `sources`.
  
  An item seen by both an always-adult source and the DHT stays adult: any always-adult finder is enough.
- **Your own Not adult correction wins; a Not adult classification rule doesn't** (§2.19, runner's call, flagged to the owner). A per-torrent correction is your explicit act on one item you've looked at, as the brief recommends. A rule matches many torrents by name, and the source is the stronger signal, so a rule never un-adults a source-adult item.
- **Known gap, for a follow-up in `crates/api`:** if you *clear* a correction with `/classify/correct`, the API reclassifies with no source signal. The item then stays at detection's type until an always-adult source next finds it, or the backfill runs. The API needs the same `source_adult::correction`. That's outside this slice's paths, so it's left to the runner.
- **Sukebei's category claim has its own scheme**, `sukebei-category:<id>`, not `nyaa-category` (#461 review). Sukebei's ids name different categories from Nyaa's, so the two must never be read as one scheme.
- **Tests.**
  - `crates/pipeline/tests/source_adult.rs`, end to end through the pipeline:
    - a clean name from an always-adult source is stored Adult and stays adult through the backfill;
    - a non-adult source's clean name isn't affected;
    - the floor refuses its hash first;
    - CS-5a holds a name only the adult reading meets;
    - a DHT fetch promotes an unverified Sukebei item as adult;
    - a stored torrent becomes adult when the source finds it, except one you marked Not adult;
    - claims and the refile sweep meet the Adult-only core.
  - Unit tests in `source_adult.rs` and `worker.rs`.
  - `crates/api/tests/source_adult.rs`: search, counts and detail by hash leave such a torrent out with Show adult content off and show it when it's on.
  - Exports and feeds use the same stored class (`export.rs`, `digest.rs`). No fixed sleeps.
- **Paths outside the brief's list, needed for the above.** The brief only owned `worker.rs`, but promotion and the commit re-judge are in `writer.rs`, and the backfill is in `enrich.rs`, so those are here too. `lib.rs` builds the bitset. Each other `Manifest` literal gets one `always_adult: false` line. Two new test files are added, and three test files gain `..Default::default()`. Nothing in `crates/filter` or `torrent_cache.rs` changes.
