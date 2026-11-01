# B-settings-sections decisions (part c: send to client)

- **`POST /send` is its own PR** (the runner's split of B-settings-sections), after part b (#269), so its content-safety rules (§20.9, §18.7, BEP 27) get a focused review.
- **`POST /send`** takes 1–100 hashes and an optional client `id` (otherwise the default client). It uses the client's saved category, save path and paused setting. It answers one result per hash, in the order asked:
  - `sent` or `duplicate`;
  - `refused`, with `not_found`, `private`, `same_torrent` or `invalid`;
  - `failed`, with the connection test's codes or `timeout_unknown`.
- **Only what you can see is sent:**
  - **A withheld torrent looks exactly like an unknown one** (`not_found`, with the same message). This covers a torrent a filter rule hides and an Adult one while "Show adult content" is off. It's `discover::withheld`, the rule detail, Compare and export use, so a send can't be used to learn which hashes the server holds or has classified Adult.
  - **Quarantined and tombstoned torrents** aren't stored, so they're `not_found` too.
  - **A private torrent is refused as `private`.** That's BEP 27 `flags::PRIVATE`, or `flags::PRIVATE_SOURCE` for one found only through a private source (§18.7). A magnet with no trackers would make the client look the hash up on the public DHT, leaking it. The code doesn't reveal anything hidden: private torrents are shown to you locally.
  - **Your own Hide mark** (`user_items`) doesn't block a send: it's your choice, not a filter.
- **The magnet is built from the stored torrent, never the request:**
  - `xt=urn:btih:<v1 hash>`, or `xt=urn:btmh:1220<v2 hash>` for a v2-only torrent, whose stored hash is the truncated v2 hash a client can't use as a v1 hash;
  - the name, cut to 1,024 bytes on a character boundary, so the link always fits `Magnet::MAX_LEN` (a legacy-charset name can triple in UTF-8, then triple again when percent-encoded);
  - a magnet that still doesn't parse is `invalid`, in its place, so the results stay aligned with what the client was sent;
  - a v2-only torrent with no v2 hash stored is `invalid` too, never a `btih` link made of its truncated hash.
- **A hash naming a torrent already in the send is `same_torrent`.** That covers a repeat, and a hybrid torrent's v1 hash with its truncated v2 hash. Each torrent goes once.
- **Duplicates and partial sends** follow the adapters (§9):
  - **Every torrent already in the client:** each is `duplicate`, counted in `sent` because it's already where you want it. The adapters answer this with a typed `ClientError::AlreadyThere` (it used to be a `Rejected` with fixed wording), so no client's own refusal text can be mistaken for it.
  - **Some already there:** the adapter can't say which, so all are `sent`.
  - **qBittorrent** answers a duplicate and an invalid link alike (`Fails.`), so a lone torrent already in qBittorrent is `failed`/`rejected`, with "The client refused the torrent; it may already be in qBittorrent." (the runner's call, §2.26). Telling them apart needs a `torrents/info` look-up first, which is left as a follow-up.
  - **Failed part-way:** `ClientError::Partial { sent }` marks the first `sent` sendable torrents as sent and the rest as failed, so nothing is resent.
  - **Out of time:** a send that runs out of its 120 s can't say how far it got. Every torrent is `failed` with `timeout_unknown`, and the message says to check the client before sending again, rather than inviting a retry that would add torrents twice.
- **Rate-limited:** two sends at a time (`429 sends_busy`) and 30 a minute (`429 send_rate_limited`). Nothing is sent past either limit.
  - The limits are per install, shared by every account, because they protect the download clients.
  - **The registry owns them** (`SettingsApi::send_limits`, with a shared default for an implementation that keeps none). Each test's router gets its own, so tests running at once never share them. This is in `SettingsApi` rather than as a new `ApiState` field, which would break every `ApiState` literal in other open PRs.
  - Both limits are tested end to end: 30 a minute, and a third send while two are held at a stand-in client.
- **Magnets carry the hash and the name only.** Appending the best trackers (§9's toggle) is left to the slice that builds that toggle.
- **Not wired yet, as a follow-up:** sends aren't counted in the Serve stage's `sends` metric (`docs/metrics.md` §6.7).
- **`ClientError::AlreadyThere` and `clients::is_already_there`** are new in `crates/integrations`, outside the brief's "Own" list (accepted by the runner): the typed answer to an all-duplicate add, and its check. The adapters' tests that matched the old wording now match the variant.

## What S-search-5 binds to

- **Send:** `POST /send` `{hashes, client?}`. Show each result's `message`. `sent` is the count to announce, duplicates included.
- **A `timeout_unknown` failure** should prompt a look at the client, not a Retry button.
