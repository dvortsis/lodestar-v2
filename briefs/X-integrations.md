# X-integrations: the integrations crate (download clients and notifications)
**Model:** Opus (core: credentials, SSRF, the trait design that helpers will follow). **Branch:** `slice/X-integrations`.
**Spec:**
- `docs/owner-decisions.md` and `runner/standards.md` (branch `runner/standards`, PR #39, until it merges);
- architecture §9 (download-client integrations: qBittorrent, Transmission, Deluge, rTorrent/ruTorrent, Aria2, with one-click and bulk send; notifications: webhook, ntfy, Discord, email, RSS), §8 (saved searches → alerts), §10.9.7 (digest), §12 (security), §18.7 (privacy: private results stay local) and §18.10 (companions; credentials random and encrypted at rest);
- §13 (`integrations/` crate).

## Goal
Lay the pattern that Sonnet helpers will copy for the other four clients and four notification channels. Get the security-sensitive parts right once, here.

## Do (new crate `crates/integrations`, nothing outside it)
1. **`DownloadClient` trait** (async):
   - `test()` returns version and reachability;
   - `add_magnet(s)`, with optional save path, category/label, paused, and selected file indices (for "Send selected files", §10.5);
   - `add_torrent_file`;
   - `categories()`.
   - A typed `ClientError`: auth, unreachable, rejected, unsupported (with the feature), timeout.
   - Capability flags, because not every client supports file selection or labels.
2. **qBittorrent adapter** (WebUI API v2):
   - cookie login;
   - the CSRF `Referer`/`Origin` it requires;
   - re-login on 403;
   - the `torrents/add` multipart form;
   - file priorities after add, for selected files.

   Test it against **recorded fixtures**: a local axum mock server replaying real qBittorrent responses. No network in tests.
3. **`Notifier` trait** with the webhook sender as the pattern:
   - a JSON payload with a schema version;
   - retry with backoff;
   - the outcome reported for the Serve and Insights metrics.
4. **Secret storage:**
   - Credentials are encrypted at rest with a per-install key in the data directory (file mode 0600; on Windows, a restricted ACL, documented), using an AEAD with a vetted crate (e.g. `chacha20poly1305`) and a random nonce per secret.
   - No secret appears in logs, `Debug` or error strings; test that.
   - Expose `SecretStore` for later use by companions and plugins.
5. **SSRF guard for user-configured URLs:**
   - Webhooks and notification endpoints refuse loopback, link-local, metadata (`169.254.169.254`) and, unless the user opts in per channel, private ranges.
   - Resolve DNS once, connect to the checked address, and refuse redirects to disallowed addresses.
   - Download clients are LAN services by nature, so they're allowed on private ranges but still not on link-local or metadata addresses.
   - Unit-test the address classification, including IPv6, IPv4-mapped, and DNS rebinding via a second resolution.
6. **Docs:** a `crates/integrations/README.md` explaining the pattern a new adapter follows (file layout `src/clients/<name>.rs`, `src/notify/<name>.rs`, fixtures under `tests/fixtures/<name>/`, the required tests).

## Don't
- Add HTTP endpoints or UI (later slices).
- Touch other crates. The workspace's `crates/*` glob picks the crate up. Add workspace dependency lines only if needed, and list them in the PR.

## Done when
- fmt, clippy and test pass.
- `/security-review`-level care: the PR description lists each threat (credential leakage, SSRF, CSRF against the client, response parsing) and how it's handled.
