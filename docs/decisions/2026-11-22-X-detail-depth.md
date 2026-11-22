# X-detail-depth decisions (PR 1: the drawer and /t)

Runner brief, 2026-11-22Z, split into two PRs by the runner (`runner/` r14): this one covers I16, U13, U14, I28, I26 and 02 §5.5 for the drawer and `/t/{hash}`. The Compare "Differences only" toggle (P11, 02 §4e) is the next PR, `slice/X-detail-depth-compare`. Architecture §10.5.

## What changed
- **Swarm section (§10.5.4)**, a lazy chunk. A seeders and leechers line over time from `swarm_history` (daily samples, else weekly), a per-tracker table from `swarm_trackers`, and the DHT estimate with "≈" and the word "estimate". An in-house SVG line, not uPlot. A null sample or a missing bucket (more than 1.5 buckets between samples) breaks the line: gaps, never zeros. The value axis starts at 0 and names its unit; each sample's tooltip has the full date; a hidden table carries the same numbers.
- **Tracker URLs show the host only** (security, runner). A tracker URL can hold a private tracker's passkey in its path or query, so the host is the only part ever rendered, in cells, titles and aria text. An unreadable address shows "Unreadable address".
- **Technical details (U14)**, a lazy, closed disclosure: piece size, info hash and v2 hash, flags, metadata time, junk score with its reasons. The Summary now leads with size, files, discovered, last seen, seeders, leechers and trackers reporting; it shows junk reasons only when there are some. The "Swarm data" age line moved into the Swarm section.
- **Actions (§10.5 header, U13).** "Open magnet" is now "Open in torrent client", an outline button beside Copy magnet (the magnet link only; handing off to a configured client stays with X-client-handoff). The loose "Block this…" and "Open full page" buttons became one "More actions" menu, loaded on the first press so Bits UI stays out of the first view. In the drawer the actions sit in a row under the title, in the DOM too, so focus follows the visual order at every width.
- **With no download client** the header shows "Send to a download client. Set up" once per browser (`localStorage`, every access in try/catch; without storage it shows each time, which is harmless).
- **Back link on `/t/{hash}`:** "Back to Search", history-aware (the browser's Back when the visitor came from this site, else `/search`).
- **ID links (02 §5.5):** an id becomes a link only when it matches the strict shape of a known scheme (imdb `^tt\d{7,10}$`, tvdb, anidb, anilist, wikidata), into a fixed https template with `encodeURIComponent`, `rel="noopener noreferrer"` and the external-link icon. A URL from the API is never used. `tmdb` and `mbid` get no link because the link needs a movie/tv or entity kind the API doesn't send.
- **I28:** the file filter has a visible label above it; the placeholder is a hint only.
- **I26:** the composition's list is hidden when there is one group, since it repeats the bar.

## Not done, and why
- **Full parsed metadata (I16).** Resolution, source, codecs, channels, HDR, container, group and edition exist only in `POST /compare` (2–5 hashes). `GET /torrents/{hash}`, the search hit and `/siblings` don't carry them. Nothing is faked; it needs a per-torrent field or `/compare` with one hash (server follow-up).
- **Favorite, Hide, Labels, Fix parse and "Not this one"** have no door to call yet, so the menu holds Open full page and Block this…. They join when their doors exist.
- **Per-tracker label** (only the host is known) and a "last check" per tracker beyond `at` are all the API offers.
- **`/alerts/{id}` back link** is outside this slice's paths: a follow-up.

## Size
- **Compare's size-limit figure rose with no real bytes added** (runner, §2.26). Compare loads the same 24 modules before and after, and their summed gzip moves 48,830 → 48,843 B (+13 B, chunk-name noise). The reported +0.25 KB is `own()`'s credit for chunks shared with other routes, which shifted when the new lazy detail modules joined the graph. Compare's limit is raised to the measured size on the merged tree, only if the module set is still unchanged.
- **"Detail sections (on demand)" excludes `ui/overlays/Menu.svelte` and bits-ui,** as the row menu does: RowActions already loads the shared Menu on demand. "Detail actions menu (on demand)" counts only `ActionsMenu`'s own chunk.
