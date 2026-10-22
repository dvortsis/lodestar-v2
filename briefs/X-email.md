# X-email: the SMTP email channel, the guarded TCP connect, and the shared text helpers
**Model:** Opus (core: a raw network path behind the SSRF guard, credentials, untrusted text in mail headers). **Branch:** `slice/X-email`.
**Spec:**
- `runner/standards.md` (the bar; §5.7 means this slice also gets `/security-review`);
- architecture §8 (alerts go to email among other channels), §10.9.7 (the digest: plain text plus simple HTML, no images), §12 (security), §18.7 (private results stay local);
- **`crates/integrations/README.md`: the rules every channel follows.** `src/notify/{mod,webhook,ntfy,discord,rss}.rs` and `src/net.rs` are the reference; read #49's and #60's PR descriptions for the threat tables.

## Own
- `crates/integrations/src/net.rs` (for the guarded TCP connect only), `crates/integrations/src/notify/email.rs` (new), `crates/integrations/src/notify/text.rs` (new), `crates/integrations/src/notify/mod.rs`;
- the shared helpers' current homes: `ntfy.rs`, `discord.rs` and `rss.rs`, **only** to move `clean_line`, `clean_text`, `format_bytes`, `web_link`, the truncation helpers and `compose()` into `notify/text.rs` and update the call sites (drop the duplicate `web_link` in `rss.rs`). No behaviour change there: their tests must pass unchanged.
- `crates/integrations/tests/email.rs`, `tests/fixtures/email/**`, `crates/integrations/Cargo.toml` (one approved dependency), `Cargo.lock` (by cargo only), a §16 entry.

Don't edit `src/clients/**`: X-clients is running there.

## Build
1. **A guarded TCP connect in `net.rs`.** SMTP can't go through `GuardedClient`'s HTTP stack, so add `connect_tcp(host, port, policy, timeout) -> TcpStream` that resolves once through the same `Resolve`, refuses the name if *any* answer is disallowed (the same rule as HTTP), connects only to a checked address, and checks literal IPs (all the forms `Url` normalises) before connecting. Same `NetError`s. Tests mirror the HTTP guard's: private/metadata/loopback literals, a name resolving to metadata, mixed answers, rebinding across two connects.
2. **The approved dependency: `lettre`,** pinned exactly at its current release, `default-features = false`, with only what you need: `builder`, `smtp-transport`, `tokio1`, `tokio1-rustls` on the **ring** provider (as the workspace's reqwest and tracker-scrape use; no `aws-lc-sys`, no `openssl-sys`, no native-tls). If lettre can't take an already-connected stream from `connect_tcp`, or can't be built without aws-lc, stop and say so in a draft PR; don't work around the guard. `cargo tree -p lodestar-integrations` must show neither `aws-lc-sys` nor `openssl-sys`.
3. **`email.rs`**, a `Notifier` channel:
   - **Config:** SMTP host and port, security mode (`Tls` implicit on 465, `StartTls` required, `None`), username and password as `Secret`s, from address, one or more to addresses, `allow_private`.
   - **Transport rules:** credentials are sent only over TLS, except to a local host (reuse `is_local_host` from ntfy, moved into `text.rs` or `net.rs`, with the same rule: private IP literal, single-label, `.lan`, `.local`, `.home.arpa`) with `allow_private`. `StartTls` means required, never opportunistic. Certificates verify against the configured host name (SNI) even though the TCP connect went to a checked IP. Policy is `NetPolicy::notification(allow_private)`, so loopback is never reachable.
   - **Message:** `multipart/alternative` with plain text and simple HTML (§10.9.7: no images, no remote resources, no scripts). Every torrent name is HTML-escaped in the HTML part and control-stripped in both. Use `compose()` so the withheld-private note survives truncation. A size cap (e.g. 256 KiB) keeps a huge digest from being refused.
   - **Headers:** Subject and display names come from Lodestar text plus names. Strip CR/LF and controls, and let lettre encode non-ASCII (RFC 2047). Test that a name with `\r\nBcc: attacker@example.com` can't add a header or a recipient.
   - **Addresses:** parse with lettre's `Address` (no free-form strings), validated at save time in the constructor.
   - **Delivery classes:** 4xx SMTP replies and connection failures are retryable through `deliver`; 5xx are final. An auth failure is final and says "check the username and password", never echoing them.
   - **Idempotency:** a `Message-ID` derived from `Notification::id()` (stable across retries).
   - **Redaction:** a manual `Debug` and no-leak tests, as for the other channels: a TRACE subscriber over a full delivery against a local test SMTP server, asserting the password and username appear in no log, `Debug` or error text.
4. **Test SMTP server:** a small in-test SMTP responder (tokio, scripted replies) or a dev-dependency already in the workspace; no new dev-dependency without saying why in the PR. Cover STARTTLS (a test certificate via rustls, generated with an existing dev-dependency or committed as a fixture), implicit TLS, AUTH PLAIN/LOGIN, a 4xx then success, a 5xx, a refused auth, and header injection.

## Done when
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass, after merging `origin/main`.
- `cargo tree -p lodestar-integrations` has no `aws-lc-sys`, `openssl-sys` or `native-tls`.
- The moved helpers' existing ntfy/Discord/RSS tests pass unchanged.
- The PR description has a threat table (as #49 and #60 did) and the dependency's version and feature list.
