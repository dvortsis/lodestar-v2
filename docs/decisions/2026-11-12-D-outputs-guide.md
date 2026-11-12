# D-outputs-guide decisions: the user guide's Chapter 7, connect other apps

Slice D-outputs-guide, 2026-11-12 (UTC). Brief `briefs/D-outputs-guide.md` (runner-state). Facts from `docs/decisions/2026-11-11-TZ-0.md` ("For the user guide", "Auth and errors"), `docs/decisions/2026-11-11-OUT-0.md` and `docs/decisions/2026-11-12-OUT-2.md`, the UI strings, and the apps' own source as cited below.

- **The chapter is `docs/user/07-connect-other-apps.md`, not `docs/user/outputs.md`** (runner, 2026-11-12). github-docs' lint requires numbered chapter files (CHAPTER_FILES). `docs_tool.py sync` regenerates the contents on `docs/user/README.md` and chapter 06's navigation footer. The chapter's check questions put their answers in `appendix-a-answers.md`.
- **Categories are category ids, not ports** (runner, 2026-11-12; the brief had them crossed). Lodestar serves Torznab on its web UI and API port, `8090` by default (`packaging/unraid/lodestar.xml`), and the chapter says never to forward it. Sonarr's defaults are `5030` and `5040`, so connected directly it misses `5000` (TV with no parsed resolution) and `5045` (UHD): the chapter tells people to add both. Radarr's defaults (`2000`–`2060`) miss nothing.
- **Facts checked against the apps' source** (GitHub, `develop` branches, read 2026-11-12):
  - **Error messages.** `TorznabRssParser.cs` in Sonarr (`src/NzbDrone.Core/Indexers/Torznab/`), Radarr (same path) and Prowlarr (`src/NzbDrone.Core/Indexers/Definitions/Torznab/`) read an XML `<error>` the same way:
    - code 100–199 → "Invalid API key";
    - description "Request limit reached" → "API limit reached";
    - anything else → "Torznab error detected: <description>".

    Lodestar's errors are `application/xml` (`crates/api/src/torznab.rs`), so its `100` (bad key) and `102` (no Torznab scope) both read as "Invalid API key". Its `429` (`500`, "Request limit reached") reads as "API limit reached", and its `503` (`910`, "API disabled") as "Torznab error detected: API disabled".
  - **Fields and defaults.** `NewznabSettings.cs` in Sonarr, Radarr and Prowlarr: **URL**, **API Path** (an advanced field, default `/api`), **API Key** and **Categories**; Sonarr's categories default to `5030, 5040`, Radarr's to `2000`–`2060`. The English labels come from each app's `Localization/Core/en.json`; **Show Advanced** reveals **API Path**.
  - **Prowlarr's sync.** `Applications/Sonarr/SonarrSettings.cs` syncs `5000, 5010, 5020, 5030, 5040, 5045, 5050, 5090` by default, and `Applications/Sonarr/Sonarr.cs` gives each synced indexer Prowlarr's own URL and API key. So through Prowlarr, Sonarr gets `5000` and `5045`, and the Lodestar key stays in Prowlarr alone. That's why the chapter recommends connecting through Prowlarr.
  - **bitmagnet's import** (`internal/importer/httpserver/httpserver.go`) answers with "N items imported" and "import complete".
- **Not verified, so left out or kept general:** the exact screens and button positions in each app's UI beyond the field labels, and anything about Lidarr or Readarr beyond their categories in TZ-0's decisions file.
- **No Torznab adult switch is documented.** `content.torznab_adult` has no field on any screen and no `config.toml` key, only the settings API. The chapter says Torznab leaves Adult out, with a switch **(planned)**.
- **The screenshot** (`docs/user/images/outputs-{light,dark}.png`) comes from `npm run docs:shots` over mock fixtures, through a new test in `web/e2e/docs/shots.docs.ts`. Its Torznab address shows the test server's origin, which the caption says. No secret is on screen: Lodestar shows one only in the reveal dialog. A capture of Prowlarr's form is pending from the owner (`images/SHOTLIST.md`).
