# B-settings-sections decisions (part b: test kinds and usage data)

- **`POST /settings/test` runs six kinds:** `download_client` (as before), `torznab`, `ntfy`, `discord`, `email` and `webhook` (§10.10.8). Each runs on the posted values, through the integrations SSRF guard, under the existing limits (four at a time, 30 s, 2,048 characters a field), saves nothing, and answers the same `SettingsTestResult`.
  - **Torznab** asks `GET <url>?t=caps&apikey=…` under the download-client policy: an indexer manager is usually on this machine or the LAN, like a companion (§18.10). The form's own query and fragment are dropped, so only the test's parameters go. `version` is the caps document's `<server version>`, kept only when it's a plain number. `401`, `403` and Torznab error codes 100–199 are `bad_credentials`. The check lives in `lodestar_integrations::torznab`, because only the integrations crate makes outbound HTTP (`GuardedClient`).
  - **A notification test** builds the channel's sender exactly as delivery will and makes one attempt with no retries. It sends a message titled "Lodestar test notification" (`NotificationKind::Test`). Loopback stays refused for every channel.
  - **Results never repeat the server's answer.** A failure's message is the integrations crate's own words: a static reason, an HTTP status or an address class.
- **A secret the form leaves out is the saved one, only for the saved server.** Secrets are write-only, so the form never has them. The test asks the registry (`SettingsApi::saved_secret`) and gets the secret only when every field that says where it goes matches the saved value:
  - a client's `url` origin and `kind`;
  - the Torznab URL's origin;
  - the ntfy topic URL's origin, for the token;
  - the SMTP host, port and security mode, for the password (so a test can't send it over a downgraded connection).

  It's the same rule `SECRET_ENDPOINTS` applies at save, so neither a test nor a save can carry a saved password to another server. `DownloadClientDraft` gains an optional `id` to name the saved client.

- **Usage data:**
  - `GET /usage` gives the count of logged searches, for the dialog.
  - `POST /usage/clear` deletes the whole query log and needs `{"confirm": true}`; without it the answer is `428 confirmation_required`.
  - §10.10.7 puts "clear usage data" in the **destructive** class: a dialog with the exact count, and no password (passwords are for weakening content protection and for security changes). So the brief's fresh-password fallback wasn't needed.
  - Today the query log is the only usage data Lodestar keeps.
  - **The dialog shows the count, not a size.** §10.10.7 asks for both, but the store has no size for one table without a store change (out of this slice's scope), and the log is small: one short row per search, kept at most 30 days. A size can join `UsageStats` when the store reports one.
- **Test coverage of the senders:** Torznab, ntfy, the webhook and email are tested against local stand-in servers, reached through a test-only policy (`Email::with_policy`, documented as for tests only). Discord's URL must be `https://discord.com/…`, which no local stand-in can serve, so its sending is covered by `crates/integrations` `discord.rs`'s own replay tests. Here, its URL checks and SSRF refusal are tested. An email login can't complete against a loopback stand-in, because a login without TLS goes only to a private address. The test holds that the saved password is used for the saved server and refused for another.
- **A failure to seal has its own log event,** `settings.seal_failed` (`section`, `field`), in place of `settings.saved_ignored` with `reason=key_unavailable`, which part a's decisions file named: the save was refused, not a saved value skipped. Still no path or error detail.
- **`POST /send` is in part c** (the runner's split): its visibility, private-torrent and rate-limit rules get their own PR and review.
- **Not wired yet, as follow-ups:**
  - nothing writes the query log from searches yet (`insights.query_logging` has nothing to gate), and the 30-day prune doesn't read `query_retention_days`;
  - notification routing has no deliverer.
- **The new endpoints live under the `settings` module** (`settings/{checks,usage}.rs`), with their routes and OpenAPI paths in `settings::routes` and `settings::ApiDoc`, so no shared file changed. A test holds that each of the module's schemas reaches `/openapi.json` exactly as defined, so a same-named schema in another module can't silently replace it (#255's class). The new types have distinct names: `Usage*`, `*Draft`, `EmailSecurity`.

## What S-settings-2b binds to

- **Test connection / Send test:** `POST /settings/test` with `kind` and the card's values. Leave a secret out to use the saved one; send it to test a new one. Show `error.message` inline and `version` on success.
- **Clear usage data:** `GET /usage` for the dialog's count, then `POST /usage/clear` `{confirm: true}` from the Danger zone.
