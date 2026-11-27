# X-torznab-id-caps decisions (#1132)

Slice X-torznab-id-caps: what Torznab's caps advertise for ID search, and what an ID search answers, while identification is partial. It follows the runner's audit of #1110 (ENR-11). It **supersedes ENR-11's "Caps advertise them unconditionally"** (`docs/decisions/2026-11-24-ENR-11.md`, Torznab) and restores TZ-0's caps (`docs/decisions/2026-11-11-TZ-0.md`, Caps). These are runner-level refinements (§2.19), flagged to the owner. None changes a number or loosens an owner or security decision.

## Caps
- **Caps never advertises `tvdbid`, `imdbid` or `tmdbid`, at any coverage.** `tv-search` is `q,season,ep` and `movie-search` is `q`, as in TZ-0. The golden caps test pins it, and a test checks it's the same with no releases identified, some and all. Reasons:
  - **An advertised ID parameter hides releases.** Sonarr and Radarr search in tiers: the ID tier first, with no title, then a title search only when the ID tier finds nothing. With partial coverage the ID tier finds the identified subset, which is taken as the whole answer, so every release no source identified is never seen. ENR-11 assumed a fallback that only happens on an empty answer.
  - **Coverage is never complete on a DHT index.** New finds arrive unidentified all the time, and some releases no catalogue will ever match. "Complete enough" would hide releases at any threshold short of 100 %, and 100 % doesn't hold for more than a moment.
  - **Caps can't follow coverage anyway.** Prowlarr reads caps when an indexer is added, tested or saved, and the *arr apps refresh theirs from Prowlarr now and then (TZ-0's reason for always listing 6000). Caps that followed coverage would lag it, and stay advertised for a while after it fell.
  - **A title search loses nothing.** ENR-11 indexes up to 8 catalogue titles per identified torrent, so the *arr apps' title search finds identified releases by their catalogue names as well as every unidentified one.
- **Rejected: advertise them and widen the answer** with a title search over unidentified releases. The *arr apps' ID requests carry no `q`, so there's no title to search with. Turning an ID into a title needs the catalogue's title for that ID, which lives in the store and search crates (outside this slice) and would still miss releases named differently from the catalogue.

## ID parameters a client sends anyway
- **They're still read** (ENR-11), for clients that ignore caps, a Prowlarr that synced the earlier caps, and manual searches.
- **With words in `q`, the IDs are ignored:** the request is answered exactly as the same request without them (runner, review of #1132). Reason: any ID clause beside the words can find less than the words alone. An `id OR words` clause did, two ways: search's stem fallback (`Lease::new_at`) runs only when the exact query finds fewer than 10, and the ID's matches count toward that, so `tvdbid=79349&q=dexters` lost the unidentified release `q=dexters` found by its stem; and a word with no tokens (`q=-`) compiles to nothing and dropped out of the `Or`. The title search still reaches identified releases by their catalogue titles (ENR-11). Tests: `ids_never_narrow_a_title_search` and `an_id_search_with_words_is_the_title_search`.
- **With no words in `q`, an ID request finds the identified releases only.** There's nothing else to find the rest by. Since caps doesn't advertise the parameters, only a client ignoring caps sends this. It's search's own path, so every exclusion holds (`an_id_search_keeps_every_exclusion`: a private source's release and an Adult one, with `torznab_adult` off, aren't served).

## Stale caps
- **No release advertised the ID parameters.** ENR-11 (2026-11-24) is newer than the last release, so no Prowlarr install has synced caps holding them and no re-sync note is needed in the release notes.

## For later slices
- **The "results may be incomplete" notice** for ID clauses (`imdb:`, `tmdb:` and the rest) in search needs `crates/api/src/search.rs` (SRC-3b). Suggested text: "ID search only finds releases a catalogue has identified. Many releases aren't identified yet, and some never will be. Search by title to see them all." While the sweep runs, add: "Identification is still catching up."
- **Torznab docs and the *arr setup guide** (D-outputs-guide) should say Lodestar searches by title, not by ID, and why, so no one turns on an ID-only search expecting it to be complete.
- **Advertising IDs again** needs a guarantee that every release the mode would serve is identified, held for good, which a DHT index doesn't give. A future "only identified releases" output could carry one.
