# X-torznab-id-caps decisions

Slice X-torznab-id-caps: what Torznab's caps advertise for ID search, and what an ID search answers, while identification is partial. It follows the runner's audit of #1110 (ENR-11). It **supersedes ENR-11's "Caps advertise them unconditionally"** (`docs/decisions/2026-11-24-ENR-11.md`, Torznab) and restores TZ-0's caps (`docs/decisions/2026-11-11-TZ-0.md`, Caps). These are runner-level refinements (§2.19), flagged to the owner. None changes a number or loosens an owner or security decision.

## Caps
- **Caps never advertises `tvdbid`, `imdbid` or `tmdbid`, at any coverage.** `tv-search` is `q,season,ep` and `movie-search` is `q`, as in TZ-0. The golden caps test pins it, and a test checks it's the same with no releases identified, some and all. Reasons:
  - **An advertised ID parameter hides releases.** Sonarr and Radarr search in tiers: the ID tier first, with no title, then a title search only when the ID tier finds nothing. With partial coverage the ID tier finds the identified subset, which is taken as the whole answer, so every release no source identified is never seen. ENR-11 assumed a fallback that only happens on an empty answer.
  - **Coverage is never complete on a DHT index.** New finds arrive unidentified all the time, and some releases no catalogue will ever match. "Complete enough" would hide releases at any threshold short of 100 %, and 100 % doesn't hold for more than a moment.
  - **Caps can't follow coverage anyway.** Prowlarr syncs an indexer's caps when it's added and keeps them, the reason TZ-0 always lists 6000. Caps that switched on with coverage would stay advertised after it fell.
  - **A title search loses nothing.** ENR-11 indexes up to 8 catalogue titles per identified torrent, so the *arr apps' title search finds identified releases by their catalogue names as well as every unidentified one.
- **Rejected: advertise them and widen the answer** with a title search over unidentified releases. The *arr apps' ID requests carry no `q`, so there's no title to search with. Turning an ID into a title needs the catalogue's title for that ID, which lives in the store and search crates (outside this slice) and would still miss releases named differently from the catalogue.

## ID parameters a client sends anyway
- **They're still read** (ENR-11), for clients that ignore caps, a Prowlarr that synced the earlier caps, and manual searches.
- **With `q` as well, the words match on their own:** the clause is `(id OR id … OR words)`, beside the season, episode and categories, instead of `id AND words`. So an unidentified release the title finds is served next to the identified ones. The words match over every release, not only unidentified ones, so a release identified as another work whose name holds the same words matches too; the index has no "has no ID" term to exclude it, and adding one is a search change. Reason: never hide a release the same request without IDs would find. The extra results are exactly what a title search returns; the apps check each release name against the series or film themselves.
- **With no `q`, an ID request finds the identified releases only.** There's nothing else to find the rest by. Since caps doesn't advertise the parameters, only a client ignoring caps sends this.

## Stale caps
- **No release advertised the ID parameters.** ENR-11 (2026-11-24) is newer than the last release, so no Prowlarr install has synced caps holding them and no re-sync note is needed in the release notes.

## For later slices
- **The "results may be incomplete" notice** for ID clauses (`imdb:`, `tmdb:` and the rest) in search needs `crates/api/src/search.rs` (SRC-3b). Suggested text: "ID search only finds releases a catalogue has identified. Many releases aren't identified yet, and some never will be. Search by title to see them all." While the sweep runs, add: "Identification is still catching up."
- **Torznab docs and the *arr setup guide** (D-outputs-guide) should say Lodestar searches by title, not by ID, and why, so no one turns on an ID-only search expecting it to be complete.
- **Advertising IDs again** needs a guarantee that every release the mode would serve is identified, held for good, which a DHT index doesn't give. A future "only identified releases" output could carry one.
