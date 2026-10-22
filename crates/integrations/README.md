# lodestar-integrations

Download clients and notification channels (architecture §9, §8, §10.9.7), plus the two security pieces they share: sealed credentials and the SSRF guard. This crate has no HTTP endpoints and no UI; the API and Settings slices call it.

qBittorrent and the webhook sender are the reference implementations. Copy them for the other clients (Transmission, Deluge, rTorrent/ruTorrent, Aria2) and channels (ntfy, Discord, email, RSS).

## The rules every adapter follows

1. **Every request goes through a `GuardedClient`.** Never build a `reqwest::Client` yourself, and never call `.execute()` on `GuardedClient::http()`. Use `GuardedClient::send` (no redirects) or `send_following` (307/308 only, each hop re-checked).
   - Download clients use `NetPolicy::download_client()`: public, private and loopback addresses, but never link-local, metadata or reserved ones.
   - Notification channels use `NetPolicy::notification(allow_private)`: public addresses, plus private ranges only when the user opts in for that channel. Never loopback.
   - Parse the user's URL with `net::parse_url` in the constructor, so a refused address fails when the setting is saved, not at send time.
2. **Credentials are `Secret`s.** A `Secret` doesn't print (`Debug` is `Secret(***)`, and there's no `Display` or `Serialize`). Call `expose()` only on the line that puts the value on the wire. Mark auth headers sensitive with `HeaderValue::set_sensitive(true)`. Stored credentials are `SealedSecret`s from `SecretStore::seal(context, &secret)`. The context names the record (for example `"client:3:password"`), so a sealed value can't be moved to another record.
3. **No URL, credential, cookie or raw body in any error, `Debug` or log.** Write a manual `Debug` that shows `net::redact(&url)` (scheme, host and port only). Error enums carry `&'static str` reasons. A client's own refusal text goes through `clients::client_reason` (one printable line, at most 200 characters). Log the endpoint name and status, nothing more.
4. **Bound everything you read.** Use `net::read_limited(resp, limit)` for every body. Parse into typed structs with `serde`, and turn any parse failure into `ClientError::Protocol` (or `DeliveryError::Failed`). Never panic on a response.
5. **Untrusted input is checked at the edge.** Magnets are `Magnet::parse`d: no whitespace or control characters, since several clients take newline-separated lists. Options go through `clients::check_request` before any network call.
6. **Private results stay local (§18.7).** Notifications send only `Notification::shareable_items()`, plus a count of what was withheld.

## A new download client

**File:** `src/clients/<name>.rs`, with a `pub mod <name>;` line in `src/clients/mod.rs`.

1. Write a `struct <Name>` holding the base `Url`, a `GuardedClient`, the credentials as `Secret`s, and any session state behind a `tokio::sync::Mutex`.
2. Give it `new(url, …credentials)`, which builds `GuardedClient::new(NetPolicy::download_client(), CLIENT_TIMEOUT)`, and `with_client(…, http)` for tests.
3. Write a `const CAPABILITIES: Capabilities` that's honest about what the client supports. `check_request` refuses unsupported options with `ClientError::Unsupported(feature)`, and the UI hides them.
4. Implement `DownloadClient`. Each method returns `Box::pin(async move { … })`.
   - **`test`:** log in and return the version.
   - **`add_magnets`:** call `check_request(CAPABILITIES, opts, magnets.len())?` first. If the client has no batch add, loop.
   - **`add_torrent_file`:** return `Err(ClientError::Unsupported(Feature::TorrentFile))` if the client can't take one.
   - **`categories`:** the client's categories or labels, sorted by name.
   - **File selection,** where supported: add paused, wait for the file list, deselect, then start unless `opts.paused`. If the metadata doesn't arrive in time, leave the torrent paused and return `Selection::Pending`.
5. Map the client's answers onto `ClientError`:

   | Situation | Variant |
   |---|---|
   | Wrong credentials | `Auth(BadCredentials)` |
   | Banned after failed logins | `Auth(Banned)` |
   | CSRF or host check failed | `Auth(OriginRefused)` |
   | Session refused right after a fresh login | `Auth(SessionRefused)` |
   | The client said no | `Rejected(client_reason(body))` |
   | Unparseable answer | `Protocol` |
   | Network failures | come from `NetError` via `?` |

   Re-login on an expired session once, never in a loop.
6. Add a `ClientConfig` variant and a `connect` arm.

## A new notification channel

**File:** `src/notify/<name>.rs`, with a `pub mod <name>;` line in `src/notify/mod.rs`.

1. Write a config struct whose endpoint URL and tokens are `Secret`s, plus an `allow_private: bool` where the channel can point at a LAN service.
2. Write a constructor that parses the URL with `parse_url(…, NetPolicy::notification(allow_private))`.
3. Implement `Notifier::attempt`: one try, no retry loop.
   - Build the payload from `shareable_items()`, and include the withheld count.
   - Send with `send_following`.
   - Classify the answer with `notify::check_status`: 2xx is done; 408, 429 and 5xx are retryable, with `Retry-After` honoured; the rest are final.
   - Network errors convert with `?`: `AttemptError: From<NetError>` already decides what's retryable.
4. Don't retry inside the channel. `notify::deliver(&channel, &n, &RetryPolicy::default())` owns retries, backoff with jitter and the `DeliveryOutcome`. The caller records that outcome with `IntegrationMetrics::record_delivery`.
5. Use `Notification::id()` as the idempotency key if the service has one; it stays the same on every retry.

## Fixtures

**Layout:** `tests/fixtures/<name>/<exchange>.json`. Each file is one exchange recorded from the real client or service:

```json
{
  "recorded_from": "qBittorrent v4.6.3 (WebUI API 2.9.3)",
  "note": "what this exchange shows",
  "request": { "method": "POST", "path": "/api/v2/auth/login", "query": null },
  "response": { "status": 200, "headers": [["set-cookie", "SID=…; HttpOnly; path=/"]], "body": "Ok." }
}
```

**How to record:**
- Run the real client locally. For example, `apt-get install qbittorrent-nox`, then `qbittorrent-nox --profile=/tmp/qbt --webui-port=18080`; it prints a temporary admin password.
- Make each request once with a small script and save the status, headers (minus `Date`) and body.
- **Never record request bodies:** they hold credentials. Record only the method and path.
- Use throwaway credentials. Session IDs in recorded responses are from a throwaway instance.
- Record the failure modes too, not just the happy path: wrong password, ban, CSRF refusal, expired session, invalid input and unknown IDs.

**How tests use them:** `tests/support/mod.rs` has the `ReplayServer`. It's a local axum server that plays a scripted list of fixtures in order. It fails the test if a request doesn't match the next scripted method and path, or if a scripted exchange never happens. Tests then assert on what the adapter sent: headers, form fields, multipart parts and query strings. **No test touches the network.**

## Required tests for a new adapter

Write them in `tests/<name>.rs`, using `mod support;`.

- **`test`:** login and version, including which headers carry the session and the CSRF origin.
- **Auth:** wrong credentials, a ban, and an expired session that logs in again once. Refused straight after a re-login must stop.
- **Add:** a magnet with options (assert the fields sent); a bulk add; an add the client refuses; and a `.torrent` file, if supported.
- **Selection:** applied, all files selected, metadata pending, and an out-of-range index (if supported).
- **Capabilities:** unsupported options and invalid requests make **no** request (`server.finish()` is empty).
- **Bad responses:** oversized, malformed or wrong-content answers become `Protocol` errors, not panics.
- **Unreachable:** nothing listening gives `Unreachable`.
- **Secrets:** capture logs at `TRACE` (globally; see `capture_logs` in `tests/qbittorrent.rs`). Assert that neither the password nor the session token appears in logs, `Debug` output (adapter, config, boxed trait object) or error text.

A notification channel needs unit tests with a local receiver (see `src/notify/webhook.rs`) for:
- the payload and its schema version;
- private items withheld;
- retry on 5xx and 429 with the same ID;
- a 4xx being final;
- a redirect to a refused address not being followed;
- a refused endpoint failing at construction;
- the URL never appearing in `Debug`, errors or logs.

## Secret storage

`SecretStore::open_or_create(data_dir)` keeps a 32-byte key in `<data_dir>/secrets.key`. The file holds `LSK1` followed by the key.

- **Creation:** the file is written privately and linked into place atomically, so concurrent first starts agree on one key.
- **Permissions:**
  - **Unix:** the file is created `0600`. A key file found readable by group or others is tightened back to `0600`, with a warning.
  - **Windows:** `icacls <file> /inheritance:r /grant:r *S-1-3-4:F *S-1-5-18:F *S-1-5-32-544:F` replaces the ACL. Inheritance is removed, and full control goes to the file's owner (`OWNER RIGHTS`), `SYSTEM` and `Administrators`. `Users` and `Everyone` get nothing. `icacls` is used so the crate stays free of `unsafe` Win32 code (the workspace forbids it). If `icacls` fails, key creation fails rather than leaving a readable key.
- **Sealing:** secrets are sealed with XChaCha20-Poly1305 (`chacha20poly1305`) under a fresh random 192-bit nonce. The caller's context is the associated data. The sealed format is `0x01 ‖ nonce(24) ‖ ciphertext ‖ tag(16)`.
- **Losing the key** makes stored credentials unreadable (`SecretError::Open`). Settings then asks for them again. The database alone never reveals them.
