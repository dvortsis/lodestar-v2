# F-hits-two-readings decisions

A fix for the `fuzz (lists)` failure the runner found on #384 (head 14f6d63e, not that PR's cause). The fuzz target checks `Core::verdict(item) == Core::hits(item).find(on)`.

- **The cause.** Since #381 (`2026-11-02-F-normalize-invisible.md`), `Core::verdict` reads a marked text both ways and keeps the stronger hit, but `Core::hits` read only the joined reading. On a name holding U+FFF3, the earlier reading's Cs5b hit decided the verdict and `hits` had no switched-on hit. The verdict was right (stricter); the list was incomplete.
- **The fix: `Core::hits` reads both ways too** (`core::both_readings`).
  - The earlier reading's hits come first, then the joined reading's for rules the earlier reading didn't hit. Each rule is listed once, with the earlier reading's evidence, as `verdict` keeps it on a tie.
  - Where the joined reading's first switched-on hit is strictly stronger, it leads, matching `stronger`. Every pattern rule quarantines today, so this can't happen yet; a unit test covers it with built hits, so `hits` and `verdict` can't drift apart if a rule's action changes.
  - The floor is unchanged.
- **Callers.** `Core::hits` is called only by tests and the fuzz target. Explanations, Insights and `filter_hits` rows come from the engine, which already lists both readings' hits (`Core::pattern_hits`), so nothing they show changes.
- **The fuzz target is unchanged**: its invariant is the right one.
- **Tests** (`tests/hits_two_readings.rs`):
  - the crash input, decoded as the fuzz target decodes it;
  - a property test of the invariant on stand-in terms, with marks;
  - one on the shipped lists, with marks spliced in.

  Without the fix, the crash case and the stand-in property both fail.
