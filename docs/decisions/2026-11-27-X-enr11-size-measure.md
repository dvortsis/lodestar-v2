# X-enr11-size-measure decisions

Slice X-enr11-size-measure (runner brief, 2026-11-27T08:41Z): the §3 size budgets measured with identification answers in the synthetic corpus. Spec: architecture §3, `docs/decisions/2026-11-24-ENR-11.md`, `docs/decisions/2026-11-12-ENR-10.md`. No budget changes here (brief).

## The synthetic answers (`bench/src/synth.rs`)
- **An answer is what ENR-10's runtime stores** (`Identification::into_claims`): one claims row from an enrichment source, with one to three ids of the schemes that fit the kind of work (IMDb, TMDB and Wikidata for films; TVDB, IMDb, TMDB and Wikidata for shows; AniDB, AniList, TVDB and Wikidata for anime; MusicBrainz, Discogs and Wikidata for music; Open Library and Wikidata for books; IGDB, Steam and Wikidata for games and software), a canonical title, alternative titles, one to three genres, a year and a match confidence of 60–100. Each passes `claims::encode`'s checks (a test).
- **Alternative titles:** none for 15 % of answers, 1–3 for 30 %, 4–8 for 30 %, 9–16 for 18 % and 17–32 (the claim's cap) for 7 %, a mean of about 6. A quarter are Latin-script variants of the title (a country suffix, upper case, an added word); the rest are translations in 25 languages across ten scripts (Latin with accents, Cyrillic, Greek, Arabic, Hebrew, Devanagari, Thai, Han, Japanese and Hangul), about as long as the title. Each script draws from a fixed 600-word vocabulary, so translated titles share words across the corpus as real ones do.
- **The canonical title is the release's title** for 65 % of works (the index then skips it as a duplicate), with a subtitle for 20 % and an article for 15 %.
- **One work, one answer:** everything but the confidence depends on the seed, the kind and the title (`work_title`, the name's words before the year or episode), so releases of the same work carry the same ids and titles, as a catalogue answers them. Ids are otherwise random in each scheme's real range, which keeps the index's id terms close to unique: the conservative case.
- **Which torrents:** `--identified P` gives an answer to P % of the identifiable torrents, chosen by a hash of `(seed, id)` so the shares nest (a torrent identified at 30 % is identified at 60 %). Identifiable means stored as Movie, TV, Music, Audiobook, Ebook, Comic, Software or Game, and not `flags::PRIVACY`. That's a superset of what ENR-10 sends: it also refuses a name that says nothing on its own (`NoQuery`), which the bench doesn't check, so the identified share is an upper bound for a given percent, on the conservative side for the budgets. One source answers (registered after the DHT, so it isn't the finder), and its terms record allows passing answers on, so every answer is served (`Store::served_identifications`). The report fails closed when the served count differs from the answers written.
- **The generator's `VERSION` stays 1.** The torrent stream and the swarm values are unchanged; answers draw from their own streams. A persisted 10M dataset (M8-PERF-1) holds no answers, so `--dataset` takes `--identified 0` only.

## The default share: 60 % of identifiable torrents
- **Why 60 %:** ENR-10 sends only informative names of the eight kinds above, and the catalogues it targets (TMDB, Wikidata, MusicBrainz, Open Library, IGDB) hold most released films, shows and albums but far fewer books, software and small releases. The estimate is that about 70–80 % of identifiable films and shows match with a confidence and fewer of the rest, about 60 % overall once the background walk has covered the store (100,000 lookups a day per plugin: about 100 days for 10M torrents). In the synthetic mix 90.8 % of torrents are identifiable, so 60 % is 54.7 % of the corpus. This is an estimate, not a measurement; the first plugin's soak (ENR-14) should replace it.
- **`size-report` still defaults to 0 %, and CI's gate stays there for now** (runner decision, 2026-11-27, §2.26, flagged to the owner). CI runs `--torrents 50000 --gate` with no flag, so it keeps measuring the corpus it always has, and earlier figures stay comparable. `--identified 60` measures the realistic share. Reason: database size is the owner's second priority, so the runner won't loosen a budget before trying to shrink the data. A follow-up slice, X-claims-size, tries that first: a zstd dictionary for claims rows, fewer stored titles, and pruning the claims indexes. After it the gate moves to the 60 % share, and the budgets change only for whatever overrun is left, decided then. No budget and no `ci.yml` change here.
- **0 % and 100 % bound it:** `--identified 0,60,100` builds one corpus per share and ends with every budget line by share. 0 % writes nothing at all (no sources, no claims), so its report is the one before this slice: the store and file index figures are identical to main's, and the torrent index varies by under 1 B between runs, as main's own does (549.9 and 550.4 B in two runs of main's binary), from Tantivy's merges.

## Findings
Measured at CI's size: 50,000 torrents, seed 1, `size-report --torrents 50000 --identified 0,60,100`. Each answer averages 2.0 ids and 7.2 titles (canonical included). Its claims blob averages 237 B.

| Part, bytes per torrent | 0 % | 60 % (54.7 % of the corpus) | 100 % (90.8 %) | Budget |
|---|---:|---:|---:|---:|
| Canonical store (SQLite file) | 949.3 | 1114.4 (+165.2) | 1224.5 (+275.3) | 976 |
| Torrent index | 550.6 | 613.0 (+62.4) | 642.6 (+92.0) | 560 |
| File index (smart) | 789.2 | 789.2 | 789.2 | 800 |
| File-tree blob mean | 346.1 | 346.1 | 346.1 | 600 |

- **These budgets don't hold once identification is on.** At the 60 % default share, the store is 1114.4 B against 976 and the torrent index 613.0 B against 560 (runner decision §2.26 above: the gate stays at 0 % until X-claims-size):
  - the canonical store is 138 B over its 976 B;
  - the torrent index is 53 B over its 560 B;
  - so store plus index is 1,727 B, over §3's 1.5 KB (1,536 B).
  - Even 0 % leaves only 27 B of headroom in the store. The runner was told, per the brief, on 2026-11-27.
- **Where the store's bytes go:** `torrent_claims` takes 145.5 B per torrent at 60 % (about 266 B per answer with page overhead), and its three indexes about 24 B. Rows are zstd-compressed one at a time with no dictionary, so the repeated structure isn't shared. A trained claims dictionary, like the file trees', is the obvious lever; X-claims-size tries it.
- **Where the index's bytes go:** the `tag` field's id terms (fast +14, terms +11, postings +3) and the extra titles (postings +15, positions +9, terms +6, `title_stem` +5). The 8-title cap holds the index's share well below the store's.
- **The file index is untouched:** identification adds nothing to it.
- **Run time at CI's size:** about 65 s for one share, as before (main measured 68 s on the same machine). Storing the answers takes 6 s at 60 % and 12 s at 100 %. The three-share comparison takes about 3 minutes.

## Not measured
- **The plugin's cache in `source_state`** (ENR-10: up to about a million entries in 1,024 keys per plugin) is bounded per plugin, not per torrent, so it isn't in a per-torrent figure. At 50k torrents it would dominate the figure; at 10M it's a few bytes per torrent.
- **A second plugin's row:** two plugins that both answer keep two rows (ENR-10). Each adds about one claims row's store bytes; the index adds only titles and ids it doesn't already hold, within the 8-title cap.
- **The DHT's source row:** a run above 0 % registers the DHT as source 0 (the server always does), which adds a `source:dht` term to every document. It's under 0.3 B per torrent of the index delta (reviewer's measurement), and a 0 % run leaves it out so that report stays as it was.
- **The backfill's rewrite after an answer** (ENR-10) is skipped: for the informative names ENR-10 looks up it leaves the record's title as it is.
