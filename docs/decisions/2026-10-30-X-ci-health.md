# X-ci-health decisions

Two failures that hit unrelated PRs' CI, fixed once (`docs/owner-decisions.md` §2.24). Neither changes an owner decision.

## Phrases are wrapped in a seek guard (search, architecture §8)

- **The bug is tantivy 0.27's, not ours.** `Exclude` (a `MustNot` clause) calls `seek_danger(doc)` on the excluded scorer with the required side's candidate, which can be behind the excluded scorer's own document: a phrase whose first match comes later, or one that matches nothing in the segment and is already `TERMINATED`. The `DocSet::seek_danger` contract allows that: the default implementation answers it with `SeekLowerBound(doc)`, and `Weight::scorer_danger` checks for it. `PhraseScorer::seek_danger` instead `debug_assert!`s `target >= doc()`, so a debug build panics on `-"a b"` (the property test found `-"e,h"~*é0`). `Disjunction` forwards `seek_danger` to its children the same way, so a phrase inside a negated OR can hit it too.
- **Release builds were already correct.** The call falls through to the first posting list's default `seek_danger`, which returns its current document as the lower bound, never a wrong `Found`. `a_negated_phrase_excludes_its_matches` passes in release without the guard and fails (panics) in debug.
- **The workaround: `seek_guard::SeekGuard`.** `compile::parts_query` wraps every `PhraseQuery` in it. Its scorer answers `seek_danger(target)` with `SeekLowerBound(doc())` when `target < doc()`, and delegates everything else (`scorer_danger` too, so intersections keep tantivy's fast start). That's the answer release builds already gave, so results don't change. A phrase's `doc()` is its first posting list's document, which only moves to a seek target or past a document checked for the phrase, so it's always a valid lower bound.
  - Every phrase rather than only negated ones: a phrase reaches `Exclude` nested in a word's alternatives, a field OR or a group, and the cost is one virtual call.
  - Remove it when tantivy fixes `PhraseScorer::seek_danger` (or `Exclude`); the regression tests then pass without it.
- **Tests:** the property test's seed is committed; `a_negated_phrase_suggests_without_panicking` runs the found query and phrases that match nothing or start later; `a_negated_phrase_excludes_its_matches` checks `dark -"…"` against `dark` minus the phrase's matches, and `-"…"` against everything.

## The rtorrent "nothing listening" test holds its port (test harness)

- **Cause:** `nothing_listening_is_unreachable` bound `127.0.0.1:0`, dropped the listener and connected to the freed port. Under load, another test's `ReplayServer` was given that port, answered the stray `POST /RPC2` with its own script (so the test saw `Rejected("Could not find info-hash.")`, `Auth`, a 599…), and lost the scripted reply its own test needed. That's the victim side: `paused_loads_without_starting_and_bulk_skips_duplicates` and others got the next reply ("already in the client"). Reproduced by narrowing the ephemeral port range: 21 wrong answers and several victims in 300 runs at 64 threads.
- **Fix:** `support::refused_addr()` returns a socket that is bound but never listens, held for the test's lifetime. Connecting is refused, and without `SO_REUSEADDR` no other socket can bind the port meanwhile. No sleeps.
- **The other adapters' tests** (`aria2`, `deluge`, `qbittorrent`, `transmission`) use the same bind-and-drop pattern. They aren't this slice's to edit; each can switch to `refused_addr()` in one line.
