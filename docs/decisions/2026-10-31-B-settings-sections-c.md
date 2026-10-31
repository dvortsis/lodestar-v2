# B-settings-sections decisions (part c: send to client)

- **`POST /send` is its own PR** (the runner's split of B-settings-sections), after part b (#269), so its content-safety rules (§20.9, §18.7, BEP 27) get a focused review.
- **`POST /send`** takes 1–100 hashes and an optional client `id` (otherwise the default client). It uses the client's saved category, save path and paused setting, and answers one result per hash, in order: `sent`, `duplicate`, `refused` (`not_found`, `hidden`, `adult_hidden`, `private`, `same_torrent`) or `failed` (with the test's codes).
  - **Only what you can see is sent:**
    - a torrent a filter rule hides (`flags::HIDDEN`) is refused;
    - so is an Adult torrent while "Show adult content" is off, with the same check as the detail view;
    - quarantined and tombstoned torrents aren't stored, so they're `not_found`;
    - a private torrent (BEP 27 `flags::PRIVATE`, or found only through a private source, `flags::PRIVATE_SOURCE`, §18.7) is refused as `private`: a magnet with no trackers would make the client look the hash up on the public DHT, leaking it;
    - the magnet is built from the stored torrent's v1 hash, never the request's, and two hashes naming one torrent (a v1 hash and its truncated v2 hash) send it once (`same_torrent`).

    Your own Hide mark (`user_items`) doesn't block a send: it's your choice, not a filter.
  - **Duplicates and partial sends** follow the adapters (§9):
    - when the client finds every torrent already there, they all count as `duplicate`, which is sent (and counted in `sent`). This is recognised by `clients::is_already_there`, not by the error's wording;
    - `ClientError::Partial { sent }` marks the first `sent` sendable torrents as sent and the rest as failed, so nothing is resent.
  - **Rate-limited:** two sends at a time (`429 sends_busy`) and 30 a minute across the server (`429 send_rate_limited`). Nothing is sent past either limit.
  - Magnets carry the hash and the name only. Appending the best trackers (§9's toggle) is left to the slice that builds that toggle.
- **Not wired yet, as a follow-up:** sends aren't counted in the Serve stage's `sends` metric (`docs/metrics.md` §6.7).

## What S-search-5 binds to
- **Send:** `POST /send` `{hashes, client?}`. Show each result's `message`; `sent` is the count to announce (duplicates included, since they're already in the client).
