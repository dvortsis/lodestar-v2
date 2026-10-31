# B-settings-sections decisions (part b: test kinds, usage data, send to client)

- **`POST /settings/test` runs six kinds:** `download_client` (as before), `torznab`, `ntfy`, `discord`, `email` and `webhook` (§10.10.8). Each runs on the posted values, through the integrations SSRF guard, under the existing limits (four at a time, 30 s, 2,048 characters a field), saves nothing, and answers the same `SettingsTestResult`.
  - **Torznab** asks `GET <url>?t=caps&apikey=…` under the download-client policy: an indexer manager is usually on this machine or the LAN, like a companion (§18.10). The form's own query and fragment are dropped, so only the test's parameters go. `version` is the caps document's `<server version>`, kept only when it's a plain number. `401`, `403` and Torznab error codes 100–199 are `bad_credentials`. The check lives in `lodestar_integrations::torznab`, because only the integrations crate makes outbound HTTP (`GuardedClient`).
  - **A notification test** builds the channel's sender exactly as delivery will and makes one attempt with no retries. It sends a message titled "Lodestar test notification" (`NotificationKind::Test`). Loopback stays refused for every channel.
  - **Results never repeat the server's answer.** A failure's message is the integrations crate's own words: a static reason, an HTTP status or an address class.
- **A secret the form leaves out is the saved one, only for the saved server.** Secrets are write-only, so the form never has them. The test asks the registry (`SettingsApi::saved_secret`) and gets the secret only when every field that says where it goes matches the saved value:
  - a client's `url` origin and `kind`;
  - the Torznab URL's origin;
  - the ntfy topic URL's origin, for the token;
  - the SMTP host and port, for the password.

  It's the same rule `SECRET_ENDPOINTS` applies at save, so neither a test nor a save can carry a saved password to another server. `DownloadClientDraft` gains an optional `id` to name the saved client.
- **`POST /send`** takes 1–100 hashes and an optional client `id` (otherwise the default client). It uses the client's saved category, save path and paused setting, and answers one result per hash, in order: `sent`, `duplicate`, `refused` (`not_found`, `hidden`, `adult_hidden`) or `failed` (with the test's codes).
  - **Only what you can see is sent:**
    - a torrent a filter rule hides (`flags::HIDDEN`) is refused;
    - so is an Adult torrent while "Show adult content" is off, with the same check as the detail view;
    - quarantined and tombstoned torrents aren't stored, so they're `not_found`.

    Your own Hide mark (`user_items`) doesn't block a send: it's your choice, not a filter.
  - **Duplicates and partial sends** follow the adapters (§9):
    - when the client finds every torrent already there, they all count as `duplicate`, which is sent;
    - `ClientError::Partial { sent }` marks the first `sent` sendable torrents as sent and the rest as failed, so nothing is resent.
  - **Rate-limited:** two sends at a time (`429 sends_busy`) and 30 a minute across the server (`429 send_rate_limited`). Nothing is sent past either limit.
  - Magnets carry the hash and the name only. Appending the best trackers (§9's toggle) is left to the slice that builds that toggle.
- **Usage data:**
  - `GET /usage` gives the count of logged searches, for the dialog.
  - `POST /usage/clear` deletes the whole query log and needs `{"confirm": true}`; without it the answer is `428 confirmation_required`.
  - §10.10.7 puts "clear usage data" in the **destructive** class: a dialog with the exact count, and no password (passwords are for weakening content protection and for security changes). So the brief's fresh-password fallback wasn't needed.
  - Today the query log is the only usage data Lodestar keeps.
- **Not wired yet, as follow-ups:**
  - nothing writes the query log from searches yet (`insights.query_logging` has nothing to gate), and the 30-day prune doesn't read `query_retention_days`;
  - sends aren't counted in the Serve stage's `sends` metric (`docs/metrics.md` §6.7);
  - notification routing has no deliverer.
- **The new endpoints live under the `settings` module** (`settings/{checks,send,usage}.rs`), with their routes and OpenAPI paths in `settings::routes` and `settings::ApiDoc`, so no shared file changed. A test holds that each of the module's schemas reaches `/openapi.json` exactly as defined, so a same-named schema in another module can't silently replace it (#255's class). The new types have distinct names: `ClientSend*`, `Usage*`, `*Draft`, `EmailSecurity`.

## What S-settings-2b binds to
- **Test connection / Send test:** `POST /settings/test` with `kind` and the card's values. Leave a secret out to use the saved one; send it to test a new one. Show `error.message` inline and `version` on success.
- **Send** (S-search-5): `POST /send` `{hashes, client?}`. Show each result's `message`; `sent` is the count to announce.
- **Clear usage data:** `GET /usage` for the dialog's count, then `POST /usage/clear` `{confirm: true}` from the Danger zone.
