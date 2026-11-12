# Review: PR #747, D-outputs-guide (head 2ac5eaffdf855451f75253f36ff967f3c4b41a36)

Fresh reviewer, 2026-11-12 (UTC). Docs PR: Chapter 7 "Connect other apps", its glossary, index, revisions, appendix answers, two screenshot pairs and one docs-shots test.

**Verdict: CHANGES.** Two things a user would actually hit are wrong: the error table in 7.5 sends the most common failure (Lodestar can't be reached) to the wrong row, and the Windows import command fails in PowerShell. A third issue is cheap to fix: Figure 7.2 outlines both sections. Everything else checks out against the code, the decisions files and the apps' source.

## Gates I re-ran
- `docs_tool.py sync docs/user --check`: up to date.
- `lint_docs.py docs/user --strict`: `LINT: 1924 checks, 0 failed, 0 warnings`.
- Relative links and anchors in 07, 03, 06, README, appendix A, glossary and index: 0 broken (checked by script).
- `prettier --check web/e2e/docs/shots.docs.ts`: clean. CI on the head: `ci-ok` success.
- Hygiene: no model names, no 12-digit numbers, no local zones or places in the diff. The only model string is the harness's Co-Authored-By trailer.

## Blocking findings

### B1. 7.5 sends "can't reach Lodestar" to the "search is off" row, or to no row
I checked this against the apps' `develop` source (fetched 2026-11-12):
- **Timeouts.** `NzbDrone.Common/Http/Dispatchers/ManagedHttpDispatcher.cs` (Sonarr, Radarr, Prowlarr) turns a request timeout into `WebException("Http request timed out")`. Then `HttpIndexerBase.TestConnection`'s `WebException` catch maps any message that contains `"timed out"` to **ServerUnavailable**. The user sees "Unable to connect to indexer, indexer's server is unavailable. Try again later. Http request timed out." That message contains **server is unavailable**, so row 3 sends them to turn on Enable search and restart Lodestar. It doesn't contain **timeout** (it says "timed out"), so row 6 doesn't match.
- **Wrong host or port, connection refused.** These reach `TestConnection` as `HttpRequestException`. Nothing converts them to `WebException`. Sonarr and Radarr show "please check your DNS settings and ensure that IPv6 is working or disabled. Connection refused (…)". Prowlarr shows "This is typically caused by DNS/SSL issues…". Neither contains **timeout** or **connection failure**, so no row matches. This is exactly the `localhost`-inside-a-container mistake that 7.3's Docker note warns about.
- **Lodestar's real 503** (search off; `torznab.rs` `run()` when `s.index` is `None`; also the guard's `keys_unavailable`) shows as "server is unavailable … [503:ServiceUnavailable]". So **503** is what tells it apart.

**Fix:**
- Key row 3 on `503` or **ServiceUnavailable**, not on "server is unavailable" alone.
- Key row 6 on **timed out**, **Connection refused**, **DNS** or **connection failure**.
- Add one line: "server is unavailable" together with "timed out" means the app can't reach Lodestar.

**Also in this table:** the "no results" row quotes Sonarr's and Radarr's wording. Prowlarr, the recommended path, says "Query successful, but no results were returned from your indexer". Key the row on **no results**, which the index marker already uses.

### B2. The Windows import command fails in PowerShell
7.7 says to type `curl.exe` instead of `curl` in PowerShell. That fixes the alias, but the argument is still `@<file>`. In PowerShell an unquoted `@name` is the splatting operator, so `@lodestar-export-….bitmagnet.ndjson` either fails to parse or reaches curl.exe without the `@file` form, and the import fails.

**Fix:** tell Windows users to quote it, as in `curl.exe --data-binary "@lodestar-export-20261112-093015.bitmagnet.ndjson" http://<bitmagnet-host>:3333/import`, or give the whole Windows line. Quoting also works in a Linux shell, so the main line can be quoted too.

## Should fix (cheap; can land with B1 and B2)

### S1. Figure 7.2 outlines both sections
`capture()` in `web/e2e/docs/capture.ts` sets the outline attribute and never clears it. The second capture in the new test (`outputs-export-*`) runs after the first, so `outputs-export-{light,dark}.png` outline both the Torznab section and the export section. The caption says only the export section is outlined.

**Fix:** clear the first outline before the second capture (remove the `OUTLINE` attribute, or reload the page), or split the test in two. Then regenerate the two images.

## Polish (not blocking)
- **P1.** 7.7's index marker lists `export_adult`, an internal setting name the chapter never shows. Use the label **Include adult torrents**.
- **P2.** Row 1 of 7.5 could add "expired". A Security key can expire, and the guard answers 401 for it, the same as for a revoked key.
- **P3.** The decisions file says only 401 and 403 reach Prowlarr's parser. In fact any status below 500 other than 429, with an XML body, reaches it. Lodestar's 400 parameter errors and its 405 show as "Torznab error detected: …". This doesn't affect users; correct the record.
- **P4.** The runner approved one task-finder row; the PR adds two (Prowlarr/Sonarr/Radarr, and bitmagnet). Both are reasonable. The runner decides.
- **P5.** Out of scope here: the top-level `README.md:64` still says "Torznab is planned". Flag it to whoever owns the README.
- **Known, not blocking:** the curl line in Figure 7.2 is clipped by OUT-2's UI defect (follow-up PR). No real key is shown.

## What I verified as correct
- **Endpoint and API Path:**
  - the route is `/torznab/api`, nested under `/api/v1` (`crates/api/src/torznab.rs` `routes()`, `crates/lodestar/src/http.rs`);
  - the screen shows `<origin>/api/v1/torznab` (`outputs/logic.ts`);
  - Sonarr's, Radarr's and Prowlarr's `NewznabSettings` default `ApiPath` to `/api` as an Advanced field, and the labels URL, API Path, Show Advanced and Test match the apps' `en.json`;
  - `/api/...` paths that match no route get the API's 404 (`web.rs` `serve`), so the 404 row holds.
- **The key:**
  - Outputs makes keys with only the `torznab` scope (OUT-2);
  - keys are read-only (the guard refuses anything but GET and HEAD) and can't sign in;
  - the secret is shown once, behind the password, in Security's reveal dialog;
  - the strings "Create key", "Your password", "Copy the secret for {name}", "Copy", "Done", "Regenerate key" and "Revoke key", and the regenerate warning, all match `en.json`.
- **Statuses** (`auth/guard.rs`, `torznab.rs`):
  - a bad, revoked or expired key gets 401 with code 100;
  - a key without the torznab scope gets 403 with code 102;
  - rate limits get 429 with `Retry-After`: 10 failed checks per IP at one every 6 s, and 300 requests a burst per key at 5/s;
  - search off gets 503 with code 910;
  - the unaccepted notice gets 409, but only for a browser session, since keys skip the gate. Leaving it out of the table is right.
- **How the apps show those statuses:**
  - Sonarr and Radarr throw in `HttpClient` on status ≥ 400. `TestConnection` then shows "invalid credentials" for 401 and 403, "server is unavailable" for 5xx, and `[429:TooManyRequests]` or `[404:NotFound]` in the text otherwise;
  - Prowlarr suppresses HTTP errors, throws for 429 and 5xx, and parses codes 100–199 as "Invalid API key", shown inside "Credentials appears to be invalid";
  - rows 1, 2 and 4 are consistent with this.
- **Categories:**
  - Sonarr defaults to `5030, 5040` and Radarr to `2000`–`2060`;
  - Prowlarr's Sonarr sync covers `5000`–`5090`, including `5000` and `5045`;
  - these match TZ-0 and the Outputs hint.
- **Port and exposure:**
  - `8090` is the default (`packaging/unraid/lodestar.xml`);
  - "never forward 8090" and the VPN advice match 3.1, and so does the gluetun note.
- **Export:**
  - the strings match: Done, the truncated warning, cancelled, failed, lost, paused, stale;
  - after a cancel or failure the guide says to delete the file, and a truncated file can be imported (OUT-0: the capped file ends cleanly, with no trailer);
  - the job is on Settings → Jobs;
  - the filename pattern is right;
  - the adult switch is separate from Show adult content, asks the 18+ question first and turns off without asking;
  - no screen has a switch for Torznab adult results (no `torznab_adult` in `web/src`, and the `content` section isn't a generic screen), so "(planned)" is right;
  - bitmagnet answers "N items imported" and "import complete" (`httpserver.go`), and its `/import` reads the raw body, so `--data-binary` with no header works.
- **Enable search** is in Search & ranking and needs a restart (`restart_required: true` in `settings_index.txt`).
- **Safety:**
  - keep the key secret, one per app, regenerate on leak, never forward the port, Prowlarr recommended;
  - nothing weakens CSP or auth or exposes the API;
  - the 03 edit removes a "(planned)" that would otherwise contradict the new chapter.
- **Format:** What you will learn; tasks with **Result:** lines; a Recommended choice; a worked example; check questions with answers in Appendix A; key takeaways; glossary entries for bitmagnet, Indexer, Newznab category, Torznab and Torznab key; a revisions row with a UTC time; the nav footer.
- **Screenshots:** made from the real UI over mock fixtures. They show the test origin `localhost:4173`, a fake prefix `ls_k7Qd…`, no hostnames, emails or torrent names, and no secret. Both are referenced with light/dark `<picture>` pairs and alt text.
- **Scope:** `docs/user/**`, images, the slice's decisions file, and the docs-shots test the user-docs skill names. OK.
