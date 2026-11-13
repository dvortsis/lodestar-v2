# Outbound integrations review
Reviewed at 55e155a9.

## Coverage
- Read in full (non-test source):
  - `crates/integrations/src/`: `lib.rs`, `net.rs`, `net/tracker.rs`, `secret.rs`, `torznab.rs`, `log_events.rs`, `metrics.rs`
  - `crates/integrations/src/clients/`: `mod.rs`, `qbittorrent.rs`, `transmission.rs`, `deluge.rs`, `rtorrent.rs`, `xmlrpc.rs`, `aria2.rs`, `magnet.rs`, `usenet.rs`, `sabnzbd.rs`, `nzbget.rs`
  - `crates/integrations/src/notify/`: `mod.rs`, `webhook.rs`, `text.rs`, `ntfy.rs`, `discord.rs`, `email.rs`, `rss.rs`
  - `crates/api/src/settings/send.rs`, `crates/api/src/settings/checks.rs`, `crates/api/src/webhooks.rs`, `crates/api/src/alerts.rs`, `crates/lodestar/src/watch.rs`
  - `crates/integrations/README.md`, `crates/integrations/clippy.toml`
- Skimmed (to confirm a finding or a guard): `crates/api/src/settings.rs:610-770`, `crates/api/src/auth/guard.rs:100-280`, `crates/api/src/extract.rs:194-221`, `crates/lodestar/src/settings/mod.rs:393-530` and `:1247-1293`, `crates/lodestar/src/settings/notifications.rs`, `crates/lodestar/src/settings/integrations.rs:113-160`, `crates/lodestar/src/worker/mod.rs:329-360`, `crates/api/src/torznab.rs:349-372`, `crates/api/src/torrents.rs:612-640`, `crates/api/src/export.rs:869-876`.
- Not read: the `#[cfg(test)]` modules and `crates/integrations/tests/**` beyond a grep for fixed sleeps and `#[ignore]` (none of the first; four `#[ignore]`d live-client tests, each labelled "needs a running <client>"). Nothing was built or run, so every finding is from reading.
- Scope note: `crates/lodestar/src/watch.rs` is the `.torrent` watch folder (UP-1), not alert evaluation. Alerts are evaluated by the percolator (`crates/lodestar/src/percolator.rs`, `Store::record_percolation`), which is outside this area's file list and was not reviewed here. `alerts.rs` only lists, announces and feeds what the percolator stored.

## Findings

### INTEG-01 No notification is ever delivered: alerts, updates, health and the digest have senders, settings and a working "Send test", but no dispatcher
- Severity: important
- Kind: gap
- Confidence: confirmed
- Where: `crates/api/src/alerts.rs:61-73`; `crates/integrations/src/notify/mod.rs:302-342`; `crates/integrations/src/metrics.rs:35-53`; `crates/api/src/settings/checks.rs:349-352`; `crates/lodestar/src/settings/notifications.rs:1-12`, `:88-95`; `web/src/lib/features/settings/screens2/channels.ts:7-14`; `docs/architecture.md:267`
- What:
  - `notify::deliver` (the retry loop), `RetryPolicy` and `IntegrationMetrics` have no caller outside the integrations crate. The only production call of `Notifier::attempt` is the Settings test message in `checks.rs`.
  - `route_alerts`, `route_updates`, `route_health` and `digest_channels` are saved, validated and shown, and nothing reads them (a grep of `crates/` outside `crates/lodestar/src/settings/` finds no reader).
  - `alerts.rs` says so itself: "Nothing sends on them yet; sending waits for the notifications settings section". That section has since merged.
  - `IntegrationMetrics::record_send` is not called by `POST /send` either; Insights reports `sends: None` honestly (`crates/api/src/insights/stages.rs:754`).
- Failure scenario: a user sets up ntfy, presses "Send test" (it arrives), ticks ntfy under "Alerts go to" and saves. A saved search then matches new torrents. The app badge and the RSS feed update; no ntfy message is ever sent, and nothing in the UI or the log says that routing does nothing.
- Suggested fix: add the dispatcher slice (percolator result → `Notification` with `PRIVATE_SOURCE` mapped to `private_source` → `deliver` per routed channel → `record_delivery`), with a bounded queue and a dedup key that survives a restart. Until it lands, hide or label the routing fields.
- Tracked?: acknowledged in three decisions files, with no slice carrying it. `docs/decisions/2026-10-23-B11c.md` ("Left to the sender follow-up"), `docs/decisions/2026-11-07-SRC-G.md:62` ("nothing builds alert notifications from the store yet"), `docs/decisions/2026-10-23-S-alerts.md:8`. `runner/slices.yaml` and `docs/build-plan.md` (which lists the notifiers as "already built") have no dispatcher slice that I could find. Rated important, not blocking, because the deferral is recorded; it is a spec gap against §8 if it reaches 1.0.

### INTEG-02 The RSS feed's magnet enclosure is wrong for v2-only torrents
- Severity: important
- Kind: correctness
- Confidence: confirmed (by reading; not run)
- Where: `crates/api/src/alerts.rs:356-369`; `crates/integrations/src/notify/rss.rs:89-118`. Same shape outside this area: `crates/api/src/torrents.rs:631-635`, `crates/api/src/export.rs:870-876`.
- What: `feed_xml` passes `hex(&row.info_hash.0)` to `RssMagnet`, and `magnet_url` always writes `magnet:?xt=urn:btih:<hash>`. For a v2-only torrent the stored `info_hash` is the truncated v2 hash. `send.rs:252-263` documents that this is "the truncated v2 hash, which no client can use as a v1 hash" and builds `btmh:1220<v2>` instead; `torznab.rs:352-365` does the same. The feed, the detail view and export do not.
- Failure scenario: a saved search with RSS matches a v2-only torrent. A feed reader or download client takes the enclosure `magnet:?xt=urn:btih:<truncated sha-256>`; the client joins the swarm but rejects the metadata (its SHA-1 doesn't match), so the download never starts. "Send to client" for the same torrent works.
- Suggested fix: one shared magnet builder (the logic in `send.rs::topic` or `torznab.rs::magnet`) used by all five sites; give `RssMagnet` the exact topic instead of a bare 40-hex hash. Add a v2-only case to the feed test.
- Tracked?: not found.

### INTEG-03 "Send selected files" and `.torrent` sends are built in every adapter and reachable from nothing
- Severity: important
- Kind: gap
- Confidence: confirmed
- Where: `crates/api/src/settings/send.rs:347-352` (`selected_files: None`, magnets only); `crates/api/src/dto/settings.rs:450-457` (`ClientSend` has `client` and `hashes` only); `docs/architecture.md:481`, `:2561-2569`; `web/src/lib/ui/lists/Tree.svelte:4`
- What: the five torrent adapters implement and test file selection (`select_files`, `Selection::Pending`), `add_torrent_file` and `categories`. No API route takes a file selection or sends a cached `.torrent`. The only production calls into `DownloadClient` are `test` (`checks.rs:151-152`) and `add_magnets` without a selection (`send.rs:374-375`); a grep finds no production caller of `DownloadClient::categories`, `add_torrent_file` or `add_magnet` with a selection.
- Failure scenario: §10.5 promises per-file checkboxes for "Send selected files" in the detail file tree. The tree component mentions them; the send request can't carry them, so the feature can't work whatever the UI does.
- Suggested fix: either add `files` to `ClientSend` (single hash only, capability-checked) and the UI action, or record the deferral and remove the claim from the UI comment.
- Tracked?: not found in `runner/slices.yaml`, `docs/build-plan.md` or `docs/decisions/2026-11-01-S-send.md`. The Usenet half (SABnzbd, NZBGet `add_nzb`) is tracked as UN-3 and UN-4.

### INTEG-04 qBittorrent's file selection skips the shared file-count cap
- Severity: important (latent: not reachable until INTEG-03 is wired)
- Kind: resource
- Confidence: confirmed
- Where: `crates/integrations/src/clients/qbittorrent.rs:66`, `:383-422`; compare `crates/integrations/src/clients/mod.rs:1074-1121` and its use in `transmission.rs:398`, `deluge.rs:508`, `rtorrent.rs:303`, `aria2.rs:270`
- What: `select_files` reads `torrents/files` within `MAX_FILE_LIST` (64 MiB) into `Vec<IgnoredAny>`, takes `files.len()` with no `check_file_count`, builds `skipped: Vec<String>` with one entry per unselected file, and posts them 1,000 per request. Every other adapter refuses more than `MAX_FILES` (1,000,000), whose comment says a client reporting more "is broken or hostile, and the count would otherwise size allocations".
- Failure scenario: a broken or hostile WebUI answers `torrents/files` with a 64 MiB array of small values (about 33 million elements). Lodestar allocates about 33 million `String`s (over 1 GB) and starts roughly 33,000 `filePrio` requests, each bounded only by the caller's timeout.
- Suggested fix: use `FileCount` for the answer (it stops at `MAX_FILES` without storing) and `skipped_files`, as the other adapters do; lower `MAX_FILE_LIST` to match.
- Tracked?: not found.

### INTEG-05 Download clients, Torznab, SABnzbd and NZBGet send credentials over plain `http` to any host, with no warning; ntfy and email refuse that
- Severity: polish
- Kind: security
- Confidence: confirmed
- Where: `crates/integrations/src/clients/qbittorrent.rs:183-193`, `transmission.rs:240-242`, `nzbget.rs:160-163`, `sabnzbd.rs:190-193`, `crates/integrations/src/torznab.rs:47-54`; the rule that exists for channels: `ntfy.rs:142-149`, `email.rs:250-259`, `net.rs:1015-1030`; `crates/lodestar/src/settings/integrations.rs:116-132`
- What: notification channels refuse credentials over plaintext unless the host is plainly local. Download clients and the Torznab test have no such rule and no warning, so a seedbox entered as `http://seedbox.example.com:8080/` gets the WebUI password in clear text on every send.
- Failure scenario: as above; anyone on the path reads the client's password or the indexer's API key.
- Suggested fix: reuse `is_local_host`: warn in Settings (or refuse without an explicit opt-in) when a client or Torznab address is `http` to a host that isn't plainly local and a credential is set.
- Tracked?: not found.

### INTEG-06 ntfy's plaintext-credential rule checks the name only; email also checks the address it connected to
- Severity: polish
- Kind: security
- Confidence: confirmed
- Where: `crates/integrations/src/notify/ntfy.rs:142-149`; compare `crates/integrations/src/notify/email.rs:383-396`
- What: with `allow_private` on, ntfy accepts a token over `http` to a single-label, `.lan`, `.local` or `.home.arpa` name. Email adds a second check, that the connected address is private, because "a local-looking name can still resolve to a public address (a search domain, a `.local` name served by DNS)". ntfy has no such check, and `allow_private` doesn't forbid public addresses.
- Failure scenario: `http://ntfy/alerts` with a token, on a network whose search domain resolves `ntfy` to a public host: the token goes out in clear text.
- Suggested fix: for a credentialed `http` channel, build the client with a policy that allows private addresses only (so a public answer is `Blocked`).
- Tracked?: not found.

### INTEG-07 The metadata address list misses two cloud endpoints that sit in otherwise allowed ranges
- Severity: polish
- Kind: security
- Confidence: likely (the classification is confirmed; how much each endpoint gives up without further headers was not checked)
- Where: `crates/integrations/src/net.rs:79-83`, `:95-113`, `:128-135`
- What: `168.63.129.16` (Azure's WireServer) classifies as `Public`, so every policy reaches it. `fd00:ec2::254` is the only AWS IPv6 entry; the rest of `fd00:ec2::/32` (for example the EKS Pod Identity agent at `fd00:ec2::23`) is `Private`, which download clients and `allow_private` channels reach. Their IPv4 twins (`169.254.x`) are refused as link-local.
- Failure scenario: Lodestar on an Azure VM or an EKS pod; a webhook or client URL pointed at one of these is not refused as metadata.
- Suggested fix: classify `168.63.129.16` and all of `fd00:ec2::/32` as `Metadata`.
- Tracked?: not found. Open PR 791 (NOS-2a) reuses the guard for raw TCP and inherits the same list.

### INTEG-08 TLS verification can't be relaxed per target, so a LAN client with a self-signed certificate can only be used over plain `http`
- Severity: polish
- Kind: gap
- Confidence: confirmed
- Where: `crates/integrations/src/net.rs:657-668` (no trust option on the builder); `crates/integrations/src/notify/email.rs:340-352` (a test-only root)
- What: there is no `danger_accept_invalid_certs` anywhere (good), and also no opt-in to trust one certificate or CA for one client or SMTP relay. qBittorrent's built-in HTTPS and many LAN relays use self-signed certificates.
- Failure scenario: `https://nas.lan:8080/` with a self-signed certificate fails as `unreachable`; the user switches to `http://`, which sends the password in clear text (see INTEG-05).
- Suggested fix: a per-target pinned certificate fingerprint or CA file, never a global switch. Or record the decision not to.
- Tracked?: not found; no decisions file mentions self-signed certificates.

### INTEG-09 The first write of `secrets.key` needs hard links
- Severity: polish
- Kind: correctness
- Confidence: likely (the code path is confirmed; which mounts refuse `link()` was not tested)
- Where: `crates/integrations/src/secret.rs:248-267`; caller `crates/lodestar/src/settings/mod.rs:503-513`
- What: the key is created by `fs::hard_link(tmp, path)`. On a data directory whose filesystem has no hard links (exFAT, FAT, some SMB and FUSE mounts) this fails, and every attempt to save a secret returns an error. `watch.rs:875-879` already handles the same limitation for the watch folder.
- Failure scenario: a data folder on an exFAT USB disk: saving a download client's password fails with "couldn't create the secrets key".
- Suggested fix: on `Unsupported`/`PermissionDenied` from `hard_link`, fall back to `OpenOptions::create_new` on the final name followed by write and sync (the file is restricted before the key bytes are written either way), or to a no-replace rename.
- Tracked?: not found. PK-exc (data folder on FUSE or a network filesystem) surfaces the filesystem in Insights but doesn't change this.

## Checked and found sound
- **SSRF guard, classification** (`net.rs:88-174`): IPv4-mapped, IPv4-compatible, NAT64 (well-known and local-use) and 6to4 are classified by the IPv4 they carry; IPv6 is default-deny outside `2000::/3`; `0.0.0.0/8`, `::`, multicast, broadcast, documentation, benchmarking and Teredo are `Reserved`; link-local and metadata are refused by every policy.
- **IP spellings** (`net.rs:277-311`): hosts are read through `Url`, which normalises decimal, octal and hex IPv4 to dotted decimal before `literal_ip`; the same string is what hyper parses, so the literal check and the connector agree. Schemes other than `http`/`https` and any URL userinfo are refused.
- **One resolution per connection** (`net.rs:345-417`, `:728-735`): names go through `GuardResolver`, which refuses the whole name if any answer is disallowed and hands only checked addresses to the connector; literals never reach the resolver and are checked in `send`. A pooled connection stays on its checked address. System proxies and automatic redirects are off.
- **Redirects** (`net.rs:829-855`): only 307/308, at most three, each hop back through `send` (scheme, userinfo, literal and DNS checks again); `https`→`http` refused; a cross-origin hop keeps five allow-listed headers only. Discord and the download clients never follow.
- **Raw TCP** (`net.rs:1066-1133`, `email.rs:355-413`): SMTP resolves once through the same checks and connects only to a checked address; implicit TLS hands lettre socket addresses, not a name.
- **Who can reach the LAN through the guard**: `POST /send` and `POST /settings/test` need a signed-in session; API keys and feed tokens are refused on every non-GET (`crates/api/src/auth/guard.rs:191-199`), and both routes require same-origin JSON. So no low-privilege caller can probe the LAN with the download-client policy. `test_failure` and `send_failure` never repeat a response body, only an HTTP status (`crates/api/src/settings.rs:723-769`, `send.rs:320-338`).
- **Saved secrets in a test** go only to the saved origin (`crates/lodestar/src/settings/mod.rs:1247-1293`, `Endpoint::Url` by scheme, host and port).
- **Secret storage** (`secret.rs`): XChaCha20-Poly1305 with a fresh random 192-bit nonce; AAD is header plus context, with a `0xFF` separator keeping byte seals and text seals apart; key file created empty, restricted (`0600`, or `icacls` by absolute path), then written and synced; a loose file is tightened or the open fails; `Secret` has no `Display`/`Serialize` and a fixed `Debug`; key and plaintext buffers are zeroized. The key lives in `secrets.key` beside, not in, the database.
- **Debug and errors**: every adapter and config has a manual `Debug` showing `redact(url)` only; `NetError`, `ClientError`, `DeliveryError` carry no URL; reqwest and lettre error text is dropped (`net.rs:982-997`, `email.rs:453-489`); `SavedClient` and `DownloadClientDraft` hide the password and the address.
- **Response bounds**: every body goes through `read_limited`; list answers are parsed by streaming visitors that keep only named fields (`clients/mod.rs:738-918`); NZBGet's `config` answer keeps only category names and is zeroized.
- **XML-RPC** (`clients/xmlrpc.rs`): any `DOCTYPE` is refused, only the five predefined entities resolve, nesting stops at 16, unknown value types are errors.
- **rTorrent command injection** (`rtorrent.rs:486-518`): values are quoted with `"` and `\` escaped, a leading `$` is refused, control characters are refused by `check_text`; labels are percent-encoded.
- **Magnets** (`clients/magnet.rs`, `send.rs:278-290`): built from the stored torrent, never from the request; ASCII-graphic only, so a newline can't add a second URL to qBittorrent's `urls`. Aria2 and NZBGet are never handed a URL to fetch.
- **Header and CRLF injection**: user fields reach headers only through `HeaderValue` (which refuses CR/LF) or base64; ntfy credentials and tags are restricted to visible ASCII; the SABnzbd upload name loses paths, quotes and control characters.
- **Email** (`notify/email.rs`): STARTTLS is required, never downgraded; the certificate is verified against the configured name; a plaintext login needs a local name and a private connected address; recipients are parsed addresses and the envelope comes from them alone; the Subject is control-stripped with `=?` broken up; both parts are base64.
- **RSS output** (`notify/rss.rs`): every text node is XML-escaped, descriptions are HTML-escaped first, XML-forbidden characters are dropped, links must be absolute `http(s)`, the enclosure URL is assembled from a checked hash and a percent-encoded name. The feed response carries `nosniff` and a sandboxing CSP (`alerts.rs:467-487`).
- **Discord**: host allow-list, `https` only, no redirects, `allowed_mentions.parse` empty, markdown escaped.
- **Inbound Sonarr/Radarr webhooks** (`webhooks.rs`): 256-bit token, only its SHA-256 stored, constant-time compare against a fixed hash when none is set, per-IP failure bucket shared with API keys, body read only after authentication and capped at 256 KiB, one 401 for every failure, the hash cache can't keep a removed token valid (`change_token` under the store lock).
- **`POST /send`** (`send.rs`): hidden, Adult-off and unknown hashes all answer `not_found`; private torrents are never sent; two at a time and 30 a minute; a whole-send timeout reports `timeout_unknown` instead of guessing.
- **Watch folder** (`watch.rs`): Unix acts through the folder's own descriptor with `O_NOFOLLOW` and refuses hard-linked files; Windows holds the folder, `added\` and `failed\` open without delete sharing and refuses reparse points; files are read through a size cap after two unchanged polls; moves never replace; the never-read-again set is bounded. No file name is logged.
- **Retry policy** (`notify/mod.rs:247-342`): four attempts, exponential with jitter, `Retry-After` capped at a minute; 4xx final. (Unused in production, see INTEG-01.)

## Area-level concerns
- **Magnet links are built in five places with two behaviours** (`send.rs`, `torznab.rs` right for v2-only; `alerts.rs`/`rss.rs`, `torrents.rs`, `export.rs` not). INTEG-02 is the symptom in this area; one builder would end it.
- **Outgoing webhooks are unsigned.** The payload has a stable `id` and `created_at` but no HMAC, so a receiver can authenticate Lodestar only by keeping its URL secret, and can't detect a replay beyond de-duplicating on `id`. The architecture doesn't promise signing (`docs/architecture.md:2562` describes the payload and its `id` only), so this is a design note, not a defect; decide it before the dispatcher (INTEG-01) ships, since adding a signature later changes the contract.
- **The dispatcher will need what the senders can't give it**: a bounded queue, a persisted "already notified" marker (a `Notification` id is random per construction, so a restart between percolation and delivery would either drop or repeat an alert), and the fixture gate. None of that exists yet, so "duplicate sends after restart" and "queue bounds" could not be reviewed.
- **Secrets-key rotation has no production caller, and not every sealer could follow one.** `SecretStore::rotate`/`retire_old_keys` are called only from `worker::rotate_key`, which itself is called only by tests (`crates/lodestar/src/cluster/serve.rs:29-35` says "`serve` has no rotation flow yet"). Settings caches one `Arc<SecretStore>` for the process (`crates/lodestar/src/settings/mod.rs:503-513`) and has no reseal pass for its `sealed:` values, so a rotation flow that retires old keys would leave every saved client password and channel secret unopenable unless it also re-seals settings and companion credentials. Whoever adds rotation needs a registry of everything sealed under `secrets.key`.
- **Zeroization is best-effort.** `Secret` zeroes its own buffer, but values are copied into plain `String`s on the way in and out: `SavedClient.password`, the test drafts, `saved_secret`'s return, `WireRequest` header values, lettre's `Credentials`, aria2's `token:` parameter. Fine as a posture, but the README's "its memory is zeroed on drop" overstates what a memory dump would show.
- **Tests are recorded-fixture replays.** They pin request shapes and error mapping well, but the four live-client tests are `#[ignore]`d and nothing in CI runs a real client, so a client release that changes an answer (qBittorrent 5.x's JSON add answer is already special-cased) is found by users first.
