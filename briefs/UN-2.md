# UN-2: SABnzbd and NZBGet adapters

**Runner brief, 2026-11-12.** Opus, core work, `/security-review` (it handles credentials, SSRF and untrusted responses). Branch `slice/UN-2` from fresh `origin/main` (at or after 9a6fcb48, #748 UN-0). The PR title is `UN-2: <title>`. Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Goal
Add the download-client adapters that send a Usenet item to **SABnzbd** or **NZBGet**, so UN-3 can add "Send to SABnzbd/NZBGet" (§18.6, §18.10). This is v0.7 work. Nothing in the UI or API calls them yet. UN-3 wires them up and UN-1 produces the items.

## Owns
- `crates/integrations/src/clients/sabnzbd.rs` (new)
- `crates/integrations/src/clients/nzbget.rs` (new)
- `crates/integrations/tests/usenet_clients.rs` (new)
- `crates/integrations/fixtures/usenet_clients/**` (new: recorded API answers)
- `docs/decisions/2026-11-12-UN-2.md`
- A one-line `runner/slices.yaml` entry only if one is missing. UN-2 already exists, so normally none.

**Shared, and allowed small:**
- the `pub mod` lines and the minimal type or trait registration in `crates/integrations/src/clients/mod.rs`;
- `crates/integrations/README.md`'s adapter list.

Tell the runner **before** pushing if `mod.rs` needs more than about 40 lines, or if any other file outside these needs to change.

## Read first
- `docs/architecture.md` §18.6 (network tiers: `usenet` results are NZB links sent to SABnzbd/NZBGet), §18.7 (private_source) and §18.10 (companion manager: detection, credentials, health).
- `crates/integrations/src/clients/mod.rs` and `qbittorrent.rs`, the reference adapter, plus the README's list of what a new adapter must do and test.
- `crates/integrations/src/net.rs` (`GuardedClient`, `NetPolicy`, `AddrClass`, `redact`) and `secret.rs`.
- `docs/decisions/2026-11-12-UN-0.md` (how a usenet item is stored).
- CLAUDE.md, `runner/standards.md`.

## Requirements
1. **The interface.** Use a trait or type for Usenet clients that fits UN-3, separate from the torrent `DownloadClient` if the magnet-shaped trait doesn't fit. Don't force NZBs through magnet types. It needs to:
   - **Add an NZB:** submit NZB **bytes** (with a file name, an optional category and an optional priority) and get back the client's job id.
   - **Test the connection:** version and auth check, for the settings screen.
   - **List categories:** optional, for the settings screen.

   No listing of downloads, and no history scraping.
2. **Send bytes, never a credentialed URL (runner call, §2.33).** NZB links from Prowlarr carry an API key (UN-1 never stores them).
   - Lodestar fetches the NZB itself at send time (that's UN-3's job) and uploads the bytes: SABnzbd `mode=addfile` (multipart), NZBGet JSON-RPC `append` with base64 content.
   - Never `addurl` or a URL `append`: that would copy the indexer's key into the client's history and logs.
   - Write the reasoning into the decisions file.
3. **Bounds on untrusted data.**
   - Cap NZB bytes before upload at 16 MiB, as a named constant; justify the value in the decisions file.
   - Cap every API answer read, as `MAX_LIST_BYTES` does.
   - Parse answers strictly: SABnzbd JSON `status`/`nzo_ids`, NZBGet JSON-RPC `result`/`error`.
   - Don't panic or unwrap on client data, and don't let integer overflow in ids reach anything.
   - Error text is bounded and never echoes the answer body in full.
4. **Credentials.**
   - SABnzbd's API key and NZBGet's username and password are held as `Secret`.
   - Never log them; `redact` URLs. SABnzbd puts `apikey` in the query string, so make sure no log line, error or `Debug` output carries it.
   - Prefer a header or POST body where the client accepts one.
5. **SSRF and network.**
   - Every request goes through `GuardedClient` / `NetPolicy`, exactly as the torrent adapters do (LAN/loopback rules, no redirects to other hosts, timeouts).
   - No new egress path, and no `reqwest` client of your own.
6. **Tests** (`tests/usenet_clients.rs`, plus unit tests):
   - Use recorded fixtures, or an in-process fake server through the existing test seam. There are no real clients in CI.
   - Cover: a successful add (job id returned); auth failure; a wrong API key; an oversize NZB refused before any request; an oversize or garbage answer refused; JSON-RPC error mapping; a timeout; and a redirect refused.
   - A test that the API key never appears in any `Display` or `Debug` output of errors or requests.
   - No fixed sleeps; any wait is a condition with a deadline ≥ 10 s.
   - If the crate has fuzz targets for answer parsers, add one for each new parser (`fuzz/fuzz_targets/…`, a shared edit you may make). Otherwise use a proptest that the parser never panics.
7. **Versions.** Target the current SABnzbd 4.x API and NZBGet 24.x / nzbget-ng JSON-RPC. Record the versions you followed and any differences.

## Not in scope
- UI and API routes (UN-3, UN-4).
- Prowlarr NZB fetching (UN-1, UN-3).
- Companion install or supervision (§18.10's manager): detection only if it falls out trivially from "test connection".
- Any change to how usenet items are stored.

## Done
- fmt, clippy (`--all-targets -D warnings`) and the workspace tests pass.
- The PR description names the shared edits and the bytes-not-URL call.
- Send the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green. A fresh Opus review and `/security-review` follow.
- Messages to the runner say who decided what, and ask for a reply when one is needed.
