# Wikidata plugin fixtures (hand-written, synthetic)

Written by hand in the shape of Wikimedia's Action API (`wbsearchentities`, `wbgetentities`, `format=json`), as its documentation describes it. They were **not recorded from the live service**: this slice's session couldn't reach `www.wikidata.org`. Replace them with recordings when a session can reach it. If a real answer differs from this shape, add it here and fix `sources-feeds`' parser in that PR.

Q172241 and its IMDb (`tt0111161`) and TMDB (`278`) ids are the real film's public, CC0 data. Every other id (`Q9000…`) is made up, and so are the people. `SENTINEL-…` text (descriptions, sitelinks, search labels, error messages) must never reach a result or an error.

The `.http` files list only the `Accept` header: the `User-Agent` holds the version, so `tests/wikidata.rs` checks it instead.

| Case | What it exercises |
|---|---|
| `wikidata_match` | Search, then entities: the film (with a deprecated date, cast, a `somevalue` genre and a wrong TMDB TV id) and the novella it's based on; the film is chosen |
| `wikidata_no_match` | An empty search: no match, and no second request |
| `wikidata_candidates` | Three items called Dune (two films and a novel): the year picks one; without it, a tie is no match |
| `wikidata_two_lookups` | A match then a miss: requests at least a second apart |
| `wikidata_rate_limited` | 429 with `Retry-After`: back off |
| `wikidata_maxlag` | A 200 whose body is an API error (`maxlag`): an error, never a miss |
| `wikidata_server_error` | 503 |
| `wikidata_bad_body`, `wikidata_html_body` | An entities answer cut short, or a search answer that is HTML: an error, never a cached miss |
| `wikidata_timeout` | A transport failure (timed out) |
| `wikidata_redirect` | A 302 to another host: not followed, one request, a plain failure |

`fuzz/seeds/wikidata/` holds the match's two bodies, the `maxlag` error and a missing entity.
