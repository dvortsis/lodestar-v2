# B-settings-sections: server settings for Integrations, Notifications, Trackers and Insights; test kinds; clear usage data; send to client
**Model:** Opus: secrets, SSRF-guarded outbound tests, the settings schema and an on-disk config format. Runner brief, 2026-10-31. Backend for S-settings-2b, plus the send-to-client endpoint S-search-5 deferred. `/security-review` is required.

## Why
S-settings-2 found that four of its five screens have no server backing:
- `crates/lodestar/src/settings/` has sections for general, crawler, network, storage, search, transport, preferences, content, logging and updates, but none for integrations, notifications, trackers or insights;
- `POST /settings/test` only accepts `kind: download_client`;
- there's no "clear usage data" endpoint.

S-search-5 also found no HTTP endpoint to send torrents to a download client, single or bulk. `crates/integrations` already has the clients and notifiers (`notify/{ntfy,discord,webhook}.rs`, the download clients), so this slice wires them to settings and the API.

## Read first
- `docs/architecture.md`:
  - §10.10.8, the rows for Integrations, Notifications, Trackers and Insights & digest, and the "Test connection / Send test" paragraph: tests run on unsaved values, through the SSRF guard, saving nothing, with the result inline;
  - §10.10.4–10.10.7 (fields, secrets, saving, risky changes);
  - §10.10.9 (the API additions);
  - the download-client section around line 2560 (`Partial`, duplicates, bulk send);
  - §10.9.7 (the digest);
  - query logging, which is off by default, stored locally, kept 30 days and clearable (§10.9.5 Serve);
  - §20 for any safety interplay (an adult or quarantined torrent must not be sendable unless visible to the user).
- `crates/lodestar/src/settings/README.md` and an existing section (`updates.rs` or `logging.rs`) as the pattern: schema hints, revisions, env and read-only locks, golden files, `screen_tests.rs`.
- `crates/api/src/settings.rs` (`POST /settings/test`, `DownloadClientDraft`, the SSRF guard path) and `crates/integrations/**`.
- `crates/lodestar/src/settings/golden`. Regenerate goldens with the repo's tooling; never edit them by hand.

## Do
1. **Settings sections**, following the existing pattern (schema, defaults, validation, env overrides, revisions, golden):
   - `integrations`: download clients (kind, URL, credentials, defaults) and Torznab/*arr.
   - `notifications`: channels (ntfy, Discord, email, webhook) and event routing.
   - `trackers`: the list, scrape on/off and cadence. Health is read-only state, not config.
   - `insights`: digest schedule and channels, query logging on/off (default **off**) and retention.

   **Secrets** use the existing secret-field mechanism: never returned in GET, never logged, kept when not re-sent, and redacted in the config export.
2. **`POST /settings/test` kinds:** `torznab`, plus one per notification channel (`ntfy`, `discord`, `email`, `webhook`).
   - Each runs on the posted unsaved values, through the same SSRF guard and timeouts, saves nothing, and returns a fixed-shape result with the server's message as plain text.
   - A send test posts one clearly labelled test message.
3. **`POST /usage/clear`** (or the name §10.10.9 implies): clears query logs and usage data. It's a destructive action, so require whatever confirmation primitive §10.10.7 and §20.11 use for this class. If the spec is unclear, require the fresh-password confirmation and say so in the decisions file.
4. **Send to client:** `POST /send` with one or many hashes and a target client.
   - It goes through the integrations client, with the `Partial` and duplicate semantics described around line 2560.
   - It respects visibility: never send what the user's filters hide, and never adult content unless "show adult" is on.
   - It's rate-limited, and the result is per hash.
5. **OpenAPI:** new types get distinct names. Watch for the collision class #255 fixed; a test that fails on duplicate component names is welcome. Regenerate `openapi.json` and `schema.d.ts`.
6. **Tests:**
   - each section's round trip, validation, env lock and golden;
   - secret non-disclosure (GET, export, logs);
   - each test kind against a local fake server, plus refusal of private or link-local targets by the SSRF guard;
   - clear-usage's confirmation;
   - send: partial, duplicate, hidden-content refusal, rate limit.
7. **Decisions file:** `docs/decisions/<date>-B-settings-sections.md`. It also lists what S-settings-2b's screens should bind to.

## Own
`crates/lodestar/src/settings/{integrations,notifications,trackers,insights}.rs` (new) plus the section registry lines, `crates/api/src/settings.rs` and the new send and usage endpoints, `crates/integrations/**` (only for wiring), goldens (regenerated), `web/src/lib/api/openapi.json` and `schema.d.ts` (regenerated), and your decisions file. No store schema change. If you need one, stop and ask the runner.

## Size
This will exceed 1,500 lines. Plan the split:
- **part a:** the four sections plus secrets plus goldens;
- **part b:** test kinds, clear usage and send.

## Done when
- fmt, workspace clippy `-D warnings` and `cargo test --workspace` pass;
- the openapi drift check passes;
- `npm run check` passes in `web/`;
- `/security-review` is clean on each PR's diff.

Open the PR(s), follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with each PR's number and head. Never merge. Work frugally: the account is under a usage warning. Push once per round of fixes.
