# X-notify: ntfy and Discord senders, and the RSS renderer
**Model:** Sonnet (helper; adapters behind a trait the main session defined). **Branch:** `slice/X-notify`.
**Spec:**
- `runner/standards.md` (the bar; §5.7 means this slice also gets `/security-review`);
- architecture §8 ("Saved searches → alerts": notifications go to webhook, ntfy, Discord, email, or an RSS feed per saved search), §10.9.7 (the digest goes out on the same channels: plain text plus simple HTML, no images), §18.7 (private results stay local);
- **`crates/integrations/README.md`: the rules every channel follows. Follow them exactly.** `src/notify/mod.rs` (`Notifier`, `deliver`, `check_status`, `Notification`) and `src/notify/webhook.rs` + its tests are the reference.

## Own
- `crates/integrations/src/notify/{ntfy,discord,rss}.rs`;
- in `src/notify/mod.rs`, only the `pub mod` lines and the new `Channel` variants (plus their `as_str`, if any);
- `crates/integrations/tests/{ntfy,discord,rss}.rs` and `tests/fixtures/{ntfy,discord}/**`.

Nothing else: not `net.rs`, `secret.rs`, `clients/**`, `tests/support/**` or `Cargo.toml`. **No new dependencies.** X-clients is editing `src/clients/**` at the same time. **Email is not in this slice:** SMTP needs a guarded raw-TCP connect in `net.rs` and a new dependency, so it's a separate Opus slice.

## Build
1. **ntfy** (`ntfy.rs`):
   - publish by `POST` to the topic URL (`https://ntfy.sh/<topic>` or a self-hosted server, so `allow_private` is supported), body = message text, headers `Title`, `Tags`, `Priority`, `Click` (a link back to Lodestar when the caller provides one), `X-Lodestar-Delivery`-style idempotency isn't supported by ntfy, so don't invent one;
   - optional auth: an access token (`Authorization: Bearer`) or username/password (basic), each a `Secret`, headers marked sensitive;
   - the topic URL itself is secret-ish (anyone with it can read the topic): treat it as a `Secret` and print only `redact()` output;
   - limits: ntfy's message size limit (4,096 bytes by default); truncate the body cleanly at a character boundary, with an ellipsis, and say so in a test.
2. **Discord** (`discord.rs`):
   - webhook URL `https://discord.com/api/webhooks/<id>/<token>` (also `discordapp.com`, `canary.`/`ptb.` hosts); the whole URL is a `Secret`, and the constructor refuses any other host. Public policy only, no `allow_private`;
   - JSON payload: `content` (≤ 2,000 characters) and/or up to 10 `embeds` (title ≤ 256, description ≤ 4,096, total ≤ 6,000), `allowed_mentions: {"parse": []}` always, so a torrent name can never ping `@everyone`;
   - `?wait=true` so a success is confirmed;
   - rate limits: 429 with a JSON body `retry_after` (seconds, may be fractional) and the `Retry-After` header; feed it through the existing retry path; extend nothing in `mod.rs` beyond what's owned; if `check_status` can't see the body value, use the header and note the gap in the PR.
3. **RSS renderer** (`rss.rs`), no network:
   - a pure function rendering an RSS 2.0 document from a channel title, link, description and a list of items (title, link, guid = the item's stable id, `pubDate` in RFC 2822, description). The API's per-saved-search feed (a later slice) will serve it.
   - **Escaping is the security boundary:** every text node and attribute is XML-escaped (`& < > " '`); characters invalid in XML 1.0 (C0 controls except tab/newline/CR, U+FFFE/U+FFFF, lone surrogates can't occur in Rust strings) are dropped; no CDATA. Torrent names are attacker-controlled.
   - Private items are never rendered: build from `shareable_items()` only.
   - Tests: golden output for a fixed input; property test that any input string yields well-formed XML (parse the output back with a minimal check you write in the test, e.g. no raw `<` inside text, balanced tags), and that every input character survives or is a documented drop.

## Tests (ntfy and Discord)
- Fixtures in `tests/fixtures/<channel>/`, in the README's format. ntfy: record from a real server if you can run one in your session (`ntfy serve` from its release binary); otherwise build from the documented API and say so in `recorded_from`. Discord: build from the documented API (no live webhook), and say so.
- Cover: success, 429 with `Retry-After` then success (through `deliver`), 4xx final, 5xx retried, oversized and HTML answers, private items withheld with the count sent, mention suppression (Discord), truncation (both).
- The reference **no-leak test** per channel: TRACE subscriber over a full delivery, asserting the token, password and secret URL path appear in no log, `Debug` or error text.

## Done when
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass, after merging `origin/main`.
- No new dependency; `Cargo.toml` and `Cargo.lock` are unchanged.
- The PR description lists the threats per channel and how each is handled, as #49 did.
