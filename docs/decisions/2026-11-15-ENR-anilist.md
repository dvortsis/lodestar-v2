# ENR-anilist decisions (AniList GraphQL parser, fixed queries and plugin)

Slice ENR-anilist (`runner/slices.yaml`, API lane brief, 2026-11-15). Spec: architecture §18.2 (enrichment plugin 6, AniDB / AniList) and §18.7, plan `docs/plan/2026-11-05-v07-m7.md` (ENR-anilist row, the caps line "AniList ≤ 30/min", Q4, Q5 and Q12), notes "/security-review; fixed query strings, no GraphQL client crate". Built on Wikidata's pattern and its "How to add the next plugin" (`docs/decisions/2026-11-12-ENR-14.md`); only what differs is written here.

## Terms of use and rate limit (owner §2.28), checked 2026-11-15
- **How it was checked:** AniList's own API documentation, read directly on 2026-11-15: the terms of use (`https://docs.anilist.co/guide/terms-of-use`) and the rate-limiting page (`https://docs.anilist.co/guide/rate-limiting`). No request was sent to the API itself.
- **What they say, and what the plugin does:**
  - **Non-commercial use is free.** Commercial use is free under $150 of revenue a month and needs a licence above it. Lodestar is free and self-hosted; it charges nothing and puts nothing behind a paywall. **Recorded here so it stays that way:** AniList data must never sit behind a paid feature.
  - **No backup or data storage service, no hoarding or mass collection.** The plugin sends one search a lookup, only for items the runtime finds eligible (user interest first, then the capped background, Q12), and asks for at most ten media and nine fields. At 30 a minute that's at most 43,200 lookups a day, under Q12's 100,000. **Open question for the runner to put to the owner (§2.19), as for Wikidata and Open Library:** whether a background walk over a whole index counts as "mass collection", and whether keeping a matched AniList id and titles as the torrent's v3 claims, with no expiry, counts as using the API as data storage. A user-interest-only mode for AniList would answer both. Not in this PR.
  - **No competing service.** Lodestar isn't an anime or manga list or tracker; it uses AniList only to name what a torrent is.
  - **Naming:** an app with "AniList" in its own name must say it's unofficial. Lodestar's name doesn't; the source is listed as "AniList", the service's own name, as every plugin names its service.
  - **No attribution requirement** is stated, so the plugin has no `ATTRIBUTION` constant.
  - **Rate limit.** 90 requests a minute, currently degraded to 30, plus a burst limiter. Over it: a 429 with `Retry-After`, `X-RateLimit-Limit`, `X-RateLimit-Remaining` and `X-RateLimit-Reset`, a one-minute timeout, and a GraphQL error body. **The plugin keeps to the plan's cap, 30 a minute: one request every 2 s, no burst** (`HostRate::every(2 s)`, §2.26), which also stays under the degraded limit. The host limiter backs off from 60 s to 6 h after a 429 or 5xx. The `X-RateLimit-*` headers aren't read: at a fixed 30 a minute with no burst they'd never change what's sent.
- **No API key, no account:** reading public media needs none. No user token is ever sent.

## Off by default, External class
- **The manifest:** `anilist`, an Enrichment source, Consent `External`, endpoint `https://graphql.anilist.co` only, `enabled_by_default: false`, never always-adult, never finds private.
- **Kinds: Movie and TV only.** AniList's anime is a film (`MOVIE`) or a series (`TV`, `TV_SHORT`, `OVA`, `ONA`, `SPECIAL`); `MUSIC` and formats the parser doesn't know never match. There is no anime kind in `MediaKind`, so every film or series the runtime finds eligible is asked about; for non-anime titles it's a miss (cached 7 days by ENR-10).
- **No manga for comics.** Neither the plan's ENR-anilist row nor Q5 asks for it, and a comic torrent's title (an issue or a volume) rarely equals a manga's title. Adding it later is a second fixed document with `type: MANGA` and the `MANGA` / `ONE_SHOT` formats.
- **No host wiring**, as ENR-14 left it (runner, 2026-11-12): library code listed in the catalogue, wired with the slice that lifts the External consent gate.
- **`SENDS`** (UI review 02 §5.5) holds the Sources row's text. `Manifest` has no field for it; **for the runner:** the Sources row should read it from the plugin when that field lands.

## What is sent (Q5): a fixed document, the title as one variable
- **One request a lookup:** `POST https://graphql.anilist.co` with headers `Accept: application/json` and `Content-Type: application/json` only, and the body `{"query": SEARCH_ANIME, "variables": {"search": <title>, "perPage": 10, "isAdult": false}}`, serialized by `serde_json`.
- **The document is a `const &str`** (`sources_feeds::anilist::SEARCH_ANIME`), one line, fixed at compile time: `type: ANIME` is written in it; the title, the page size and the adult flag are variables. **No text is ever written into the document,** so no character of a title can close a selection, add a field, comment out the rest or ask for `__schema` (`a_hostile_title_never_changes_the_query`: quotes, backslashes, braces, `#`, newlines, `__schema`, `$isAdult: true`; the document stays byte for byte the same and the title one string variable). No GraphQL client crate.
- **The title is sent as the query holds it** (ENR-10's `Query::new` has already dropped control characters and cut it to 256 bytes), not folded: as a JSON string variable it can't reach any syntax, and AniList's search ranks better on the real title. A title that folds to nothing sends nothing.
- **`isAdult: false`** always, as TMDB's `include_adult=false`: AniList leaves adult media out. A result marked adult anyway, or with no `isAdult`, is never chosen (`an_adult_or_unmarked_media_is_never_chosen`; the match fixture holds one).
- **The year and the kind aren't sent** (the type is the fixed `ANIME`); both are matched locally. Nothing else: never the hash, the name, the files, or anything about the user or install.

## The parser (`sources-feeds/src/anilist.rs`)
- **On the shared bounded JSON reader**, with Wikidata's rules: pure, no panics, no unwraps, no truncating casts, linear time (bitmasks over the keys read at each level), and an answer is used only when `Page::whole()`.
- **The caps (`SEARCH_LIMITS`):** 256 KiB (ten media of nine short fields are a few KiB; the slack covers a service that sends more), depth 8 (the shape is six deep), 20,000 values, 500 elements per array (a long series has a few dozen synonyms), 4 KiB per decoded string, 10 media, 64 titles kept per media. Past the media cap the parse stops (`Stop::Items`); past the title cap the rest are passed over undecoded and the media says `truncated`.
- **Strict where it reads:** `data`, `Page` and `media` must be objects and an array, each key once and unescaped. A media must be an object with an `id` that is a positive integer in plain digits; `format`, `countryOfOrigin` strings; `isAdult` a boolean; `seasonYear` and `startDate.year` numbers; `title` an object of strings; `synonyms` an array of strings. Any of them may be `null` but `id` and a synonym. A wrong type, a repeated or escaped key among these, or a lone surrogate spoils the media, and with it the answer. Every other key is passed over unread, and its repeats don't matter. A number that isn't a year 1–9999 keeps no year; a country that isn't two upper-case letters keeps none; a format it doesn't know is `Other`.
- **An `errors` key at the root** is an error answer, whatever `data` holds (a partial answer is never used).

## Choosing a match (`anilist::choose`)
- **One media must agree:** marked not adult; its format fits the kind; its romaji, English or native title or a synonym equal to the query's after folding (Wikidata's `fold`); and, when the query has a year, that year as its start year or season year. **None, or more than one, is no match** (`anilist_candidates`: the year picks one Frieren series; at 2023 a TV series and an ONA tie; the music video never fits).
- **Confidence:** 90 with a matching year, 70 without, as Wikidata's.
- **The answer:** `anilist:<id>`; the English title as the canonical one, else the romaji, else the native; the other titles once each, with languages: romaji `ja-latn` (or `ko-latn`, `zh-latn`, `und-latn` by country of origin), English `en`, native `ja`, `ko` or `zh` by country (`und` otherwise), synonyms `und`; no genres (AniList's include `Hentai` and `Ecchi`, and a genre list from the service is left for a later slice with a fixed map, as TMDB's); the query's year, else the start year. No MyAnimeList id: `mal` isn't among ENR-9's schemes.

## Judged before storage
- **Nothing here judges text.** `an_answer_is_judged_and_a_held_one_is_not_stored` runs the plugin's real answer through `unverified::ingest`: clean, it's stored; when a synonym completes a child-safety pattern with the torrent's name, the hash is held and nothing is stored.

## Errors
- As Open Library's: 200 is parsed; 429 is "limiting requests" and the host is held; a 3xx is never followed; anything else (AniList answers a bad query with 400) or a transport failure is "the request failed". A 200 with `errors` holds the plugin for its `Retry-After` (60 s when there's none, at most an hour) and is "answered with an error". A 200 that isn't `whole` is "wasn't understood". **None of them is a miss**, so none is cached as one, and no error holds the service's text.

## Files outside the slice's `owns`
- The `mod` lines in `crates/sources-feeds/src/lib.rs` and `crates/sources-plugins/src/lib.rs`; the `[[bin]]` entry in `fuzz/Cargo.toml` (shared files, one line or block each).
- `crates/sources-plugins/src/catalogue.rs`: one line listing the manifest.
- `crates/sources-plugins/tests/manifests.rs`, `crates/sources-plugins/tests/credential_spec.rs` and `crates/lodestar/src/ops.rs`: one entry each in the tests that list every catalogued plugin, as ENR-14 did.
- `fuzz/README.md`: the target's row. `fuzz/seeds/anilist/`: four seeds, copies of the fixture bodies.
- No new metric, so `docs/metrics.md` is unchanged.

## Before it can be switched on
- **Real recordings:** the slice that lifts the External consent gate re-records these fixtures from the live service and fixes the parser for any shape they differ in.
- **The owner's answer** to the bulk-use and storage question above.
- **The Legal Notice** gains AniList's line then, by the owner (§2.33); it isn't changed here.
