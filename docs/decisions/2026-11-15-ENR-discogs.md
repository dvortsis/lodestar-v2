# ENR-discogs decisions (Discogs parser and plugin, Account)

Slice ENR-discogs (`runner/slices.yaml`; no brief). Spec: architecture §18.2 and §18.7, plan `docs/plan/2026-11-05-v07-m7.md` (ENR row, Q4, Q5, Q12 and the caps line), and Wikidata's pattern, `docs/decisions/2026-11-12-ENR-14.md` ("How to add the next plugin"). The Account side follows TMDB's plugin (ENR-tmdb) and the music title split and matching follow MusicBrainz's (ENR-mb), so the two music plugins agree. Everything below that those don't already settle is this slice's choice.

## Terms and usage policy (owner §2.28), checked 2026-11-15
- **How it was checked:** from Discogs' published API documentation and API Terms of Use as known to this slice. Both pages (`discogs.com/developers`, the support site's "API Terms of Use") answered **403 to an automated fetch on 2026-11-15**, so they could not be re-read today. **The runner should re-read both before the consent gate is lifted** and correct anything below that differs. No request was sent to the API itself.
- **Rate limit:** Discogs allows 60 requests a minute for an authenticated client (25 unauthenticated), counted over a moving minute, and reports `X-Discogs-Ratelimit`, `X-Discogs-Ratelimit-Used` and `X-Discogs-Ratelimit-Remaining` on each answer; over it, 429. **The plugin sends at most 25 a minute with no burst** (the plan's cap, §2.26: `HostRate::every(2.4 s)`, `min_interval` 2.4 s), one request a lookup. A 429 reads as "limiting requests" and the host limiter backs off from 60 s to 6 h. When an answer says `X-Discogs-Ratelimit-Remaining: 0` (another client on the same token, say), the plugin uses that answer and then sends nothing for 60 s, the length of Discogs' window, rather than earn a 429.
- **User-Agent:** Discogs requires a unique User-Agent that identifies the application. **The plugin sends `Lodestar/<version> ( <the project's repository URL> )`** (`discogs::USER_AGENT`), MusicBrainz's form, the same for every install, with nothing about the user, their host or their account.
- **Authentication:** database search needs a token. **The user's own personal access token, sent only in `Authorization: Discogs token=<token>`**, never the `token=` query parameter, so it can't reach a URL, a log line or a proxy's access log. No OAuth: a personal token reads public data only, and Lodestar never acts for the account.
- **Licence:** Discogs releases its core release data (the "CC0 data": titles, artists, years, labels, tracklists) under CC0; images and everything user-contributed beyond that (reviews, notes, ratings, collection and marketplace data) are "Restricted Data" under the API terms, with limits on storing and showing it. **The plugin keeps only CC0 fields**: the master's id, its title as printed and its year. Images, genres, styles, formats, labels, barcodes, community counts and links are passed over unread.
- **Attribution:** the API terms ask an application that shows Discogs data to credit it with "Data provided by Discogs", linked to Discogs (and, where shown, Discogs' logo per their brand guidelines). They also require that the application not imply Discogs endorses it. **`discogs::ATTRIBUTION` is "Data provided by Discogs." and `discogs::ATTRIBUTION_URL` is `https://www.discogs.com/`.** `Manifest` has no field for these (or `SENDS`), so they are constants for the Sources row and the Legal Notice to read later; **runner:** the slice that adds an attribution field picks them up. The logo is the owner's call with the Legal Notice (§2.33).
- **Storage and caching:** the terms' storage limits apply to Restricted Data, which isn't kept. The CC0 fields go into ENR-10's cache and the claims like every enrichment answer.
- **Commercial use:** Lodestar is free and sells nothing; the terms' commercial limits don't apply.
- **Owner question (none blocking):** Q4 lists Discogs among the services whose terms allow this with keys and rate limits, so no owner answer is needed to build it. If the runner's re-read of the terms finds a storage limit on CC0 data or a required wording other than the above, that's an owner question before the consent gate.

## Off by default, Account class
- **The manifest:** `discogs`, an Enrichment source, Consent `Account`, endpoint `https://api.discogs.com` only, `enabled_by_default: false`, never always-adult, never finds private. One declared credential, `personal_access_token` (label "Personal access token", a secret, required), so `Discogs::need` asks for it before enabling, and the settings form never shows it (`ops.rs`' list).
- **The token is read on each lookup** from the credential store (SRC-4), checked to be 1–256 ASCII letters and digits (Discogs' tokens are 40) before it goes into a header, and dropped once the request is built. A token with any other character is "isn't a valid token" and nothing is sent. No error, log or `Debug` names it (tests: `without_a_valid_token_nothing_is_sent`, `requests_carry_the_album_and_artist_only`, `the_declared_names_are_the_ones_it_uses`).
- **Eligibility is ENR-10's, as is:** never a `PRIVACY` item, held, hidden or Adult-off; only Q5's title, year and kind (`a_private_item_is_never_sent`).
- **Kind: Music only.** Books are Open Library's; films and series aren't on Discogs.
- **No host wiring**, as for TMDB and Wikidata: the plugin is library code listed in the catalogue.

## What is sent (Q5)
- **One request a lookup:** `GET https://api.discogs.com/database/search?type=master&release_title=<album>[&artist=<artist>]&per_page=5&page=1`, headers `Accept`, `User-Agent` and `Authorization` only.
- **The title is split as MusicBrainz splits it:** at its first ` - ` into artist and album when both sides have a letter or digit, otherwise the whole title is the album and no artist is sent.
- **Plain fields, no query language.** Discogs' search takes the album and artist as their own parameters, so unlike MusicBrainz nothing needs escaping; each is one parameter encoded by the `url` crate (`a_hostile_title_is_its_own_parameters`).
- **Masters only** (`type=master`): a master is Discogs' release group, the work the torrent names, as MusicBrainz's release group is. Its id is stable across reissues.
- **The year and the kind aren't sent.** The year chooses among the answer's masters locally.

## The parser (`sources-feeds/src/discogs.rs`)
- **On the shared bounded JSON reader**, Wikidata's shape: pure, no I/O, no panics, no unwraps, linear time (a bitmask over the read keys; `many_unknown_keys_are_read_in_linear_time`).
- **The caps** (`LIMITS`): 512 KiB, depth 8, 50,000 values, 1,000 elements per array, 4 KiB per decoded string, 10 results. A search result is a flat object with a few short arrays (formats, labels, genres); five of them are a few KiB, so the caps leave two orders of magnitude.
- **Strict shape:** used only when `Page::whole()`. A result needs a positive integer `id` and a `title` that's not empty once cleaned. `type`, when present, must be `master` (only masters were asked for); `year` is a string of digits or a number, with `"0"`, `0`, `""` and `null` meaning not known, and anything else spoils the result. A root `message` key is an error object. A repeated or escaped read key spoils its part.
- **What it keeps:** the id, the title as printed (`Artist - Album`) and the year. Nothing else.

## Choosing a match (`discogs::choose`)
- **A master agrees** when some ` - ` in its printed title splits it into an artist side and an album side such that the album side folds (MusicBrainz's `fold`) to the query's album and, when the query names an artist, the artist side does too, after dropping Discogs' marks: a trailing ` (n)` (its number for two artists of one name) and `*` (a name variation) (`discogs::artist_name`). A title with no ` - ` agrees on the album alone, only when the query names no artist. When the query has a year, the master's year must be it.
- **One master agreeing is the match.** Several (the same id twice counts once) are no match: a master has no primary type to break a tie as MusicBrainz's release groups do, so it never guesses.
- **The year is the master's** (Discogs sets it from the earliest release), as MusicBrainz's first release year; a reissue's year is a miss, never a wrong match.
- **Confidence:** MusicBrainz's numbers: 90 with artist and year, 75 artist only, 70 year only, 60 title only, all under ENR-10's cap (95).
- **The answer:** `discogs:m<id>` (the store's `discogs` rule takes `m` for a master); the album side of the title as the canonical title; the year (the query's, else the master's). **No alternative titles** (a search result carries none), **no genres** (Discogs' genres and styles are left out to keep to the core fields; they could come later from its fixed genre list if wanted), **no artist** (the claims have no field for one, and the torrent's own name already holds it).

## Judged before storage
- **Nothing here judges text.** The title goes into ENR-10's claims and the writer's ingest judges it with the torrent's name (`an_answer_is_judged_and_a_held_one_is_not_stored`).

## Errors
- **Status first:** 200 is parsed; 401 or 403 is "refused the personal access token"; 429 is "limiting requests"; the host limiter holds the host after any of them or a 5xx; a 3xx is never followed; anything else, or a transport failure, is "the request failed". A 200 that isn't `whole` (an error object, HTML, a cut-short or oversize body) is "wasn't understood". **None of them is a miss**, and no error holds the service's text or the token. A title with nothing to search for sends nothing and reads no token.

## Files outside the slice's `owns`
- The `mod` lines in `crates/sources-feeds/src/lib.rs` and `crates/sources-plugins/src/lib.rs`, and the `[[bin]]` in `fuzz/Cargo.toml` (the lane's shared files).
- `crates/sources-plugins/src/catalogue.rs`: one entry (its test requires every module listed) and the module in its `use` line.
- `crates/sources-plugins/tests/manifests.rs`, `crates/sources-plugins/tests/credential_spec.rs` and `crates/lodestar/src/ops.rs`: one entry each in their lists of every catalogued plugin, which fail without it; `credential_spec.rs` and `ops.rs` also check the declared token. ENR-14 set this pattern.
- `fuzz/README.md` (one row) and `fuzz/seeds/discogs/` (new files, which the parser's tests include).
- No new metric, so `docs/metrics.md` is unchanged. `Cargo.lock` is unchanged.

## Before it can be switched on
- **Real recordings:** the fixtures are hand-written to the documented shape (`fixtures/discogs/README.md`). The slice that lifts the Account consent gate records them with a token (stripped) and fixes the parser for any shape they differ in. To confirm then: that `year` is a string, that an unknown year is `"0"` or absent, and that every master result has `type: "master"`.
- **Terms re-read** (above) and the Legal Notice's line and logo, the owner's (§2.33).
