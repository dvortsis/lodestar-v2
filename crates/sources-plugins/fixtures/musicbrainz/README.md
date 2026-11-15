# MusicBrainz plugin fixtures (hand-written, synthetic)

Written by hand in the shape of MusicBrainz's web service (`/ws/2/release-group?query=…&fmt=json`), as its search documentation describes it. They were **not recorded from the live service**. Replace them with recordings when the slice that lifts the External consent gate records them. If a real answer differs from this shape, add it here and fix `sources-feeds`' parser in that PR.

Every artist, album, id and date here is made up. `SENTINEL-…` text (tags, sort names, aliases, disambiguation comments, error messages) must never reach a result or an error.

The `.http` files list only the `Accept` header: the `User-Agent` holds the version, so `tests/musicbrainz.rs` checks it instead.

| Case | What it exercises |
|---|---|
| `musicbrainz_match` | `The Quiet Engines - Harbour Lights` (2004): the album, a same-titled album by another credit, and a live album; only the first agrees |
| `musicbrainz_candidates` | Three groups called Echoes (two albums and a single) and an EP: the year picks one; an album wins over a single of the same year; with no year, two albums tie and it's no match |
| `musicbrainz_no_match` | An empty search: no match |
| `musicbrainz_two_lookups` | A match then a miss: requests at least a second apart |
| `musicbrainz_rate_limited` | 503 with `Retry-After`, MusicBrainz's answer over the rate: back off |
| `musicbrainz_error` | A 200 whose body is an error object: an error, never a miss |
| `musicbrainz_error_then_match` | The error with `Retry-After: 5`, then the match once the wait is over |
| `musicbrainz_server_error` | 500 |
| `musicbrainz_bad_body`, `musicbrainz_html_body` | An answer cut short, or HTML: an error, never a cached miss |
| `musicbrainz_timeout` | A transport failure (timed out) |
| `musicbrainz_redirect` | A 302 to another host: not followed, one request, a plain failure |

`fuzz/seeds/musicbrainz/` holds the match and candidates bodies, an empty search and the error object.
