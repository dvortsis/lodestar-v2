# AniDB plugin fixtures (hand-written, synthetic)

Written by hand in the shape of AniDB's titles dump (`https://anidb.net/api/anime-titles.xml.gz`) and its HTTP API's anime record (`http://api.anidb.net:9001/httpapi?request=anime`), as AniDB's wiki describes them. They were **not recorded from the live service**. Replace them with recordings before the plugin is switched on. If a real answer differs from this shape, add it here and fix `sources-feeds`' parser in that PR.

Every anime id (`900…`) and title is made up. `lodestartest` / `1` is a placeholder client for these tests only: it is **not** a registered AniDB client, and no client is built into the plugin. `SENTINEL…` text (creators, characters, descriptions, tags, episode titles, error text, HTML) must never reach a result or an error. The records carry fields the plugin never reads, as AniDB's do.

The dump bodies are plain XML (the parser reads gzip and plain the same; `gzip_answers_are_read` wraps them).

| Case | What it exercises |
|---|---|
| `anidb_match` | The dump, then anime 9001's record: type, years, titles, `restricted="false"` |
| `anidb_no_match` | The dump only: an unknown title, one two anime share, a short title, a title that folds to nothing; no record asked for |
| `anidb_restricted` | Anime 9005's record with `restricted="true"`: marked adult and refused by the terms |
| `anidb_two_lookups` | Two records, at least 2 s apart; a repeat answered from memory |
| `anidb_banned` | `<error>Banned</error>`: the plugin holds a day |
| `anidb_api_error` | Another `<error>`: the plugin holds an hour |
| `anidb_rate_limited` | 429: the host limiter backs off |
| `anidb_server_error` | The dump answers 503: an error, and not retried within the day |
| `anidb_bad_body`, `anidb_html_body` | A record cut short, or HTML: an error, never a miss |
| `anidb_wrong_id` | A record for another anime than the one asked for: an error |
| `anidb_timeout` | A transport failure (timed out) |
| `anidb_redirect` | A 302 to another host: not followed, a plain failure |

`fuzz/seeds/anidb/` holds the dump, the record (plain and gzip-flagged), the restricted record and the ban, each behind the fuzz target's pick byte.
