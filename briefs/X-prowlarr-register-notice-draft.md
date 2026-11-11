# Legal Notice §2 draft for X-prowlarr-register (for the owner's approval)

Drafted by the runner, 2026-11-11. Registering the Prowlarr/Jackett plugin (built and settings-editable since #658) makes §2 of the Legal Notice untrue today. The notice is owner-approved and versioned, so the wording below needs the owner's yes before X-prowlarr-register starts. Both copies (`docs/legal/legal-notice.md`, `crates/api/src/legal/notice.md`) change identically.

**Version:** `NOTICE_VERSION` 4 → 5. This counts as substantive, because it adds a built source class that sends requests. The admin is shown the notice again at their next sign-in.

**Facts it rests on (checked on main 40945e35):**
- Lodestar contacts only the manager's address that you enter. The manager forwards to its own indexers.
- Lodestar asks each enabled BitTorrent indexer for its newest listings (the RSS view Sonarr and Radarr poll). It sends no search terms.
- Finds from private and semi-private indexers are marked private. Lodestar never downloads a `.torrent` file from any indexer.
- The API key is sealed in the credential store and never shown again.
- This build refuses to turn on every class but None (`consent_text_approved`), so Prowlarr registers switched off and can't be turned on, like the torrent cache.

## Change 1: the "This version has:" inventory
After "...for a torrent's description;" insert:

> one Account source, **Prowlarr or Jackett**, which asks the indexer manager you run for its indexers' newest listings;

## Change 2: after "In this version the torrent cache can't be turned on. A later version will let you turn it on after you accept the websites it contacts."
Insert:

> Prowlarr or Jackett can't be turned on in this version either. A later version will let you turn it on after you accept what an Account source does and enter its API key.

## Change 3: the Account bullet
Before:
> - **Account** sources sign in with credentials you give them. Lodestar encrypts the credentials on your server and never shows them again. None is built yet.

After:
> - **Account** sources sign in with credentials you give them. Lodestar encrypts the credentials on your server and never shows them again. Built: **Prowlarr or Jackett**, the indexer manager you run yourself. Lodestar contacts only the address you give it; the manager passes the request to the indexers you added to it, which may include private trackers. What private indexers report is marked private, and Lodestar never downloads a `.torrent` file from any indexer. Others are planned.

Nothing else in the notice changes. Restricted and Overlay keep "None is built yet."

## What the slice then does (not owner wording)
- `legal_facts.rs`:
  - four registered plugins;
  - the Prowlarr phrases;
  - "None is built yet." appears twice;
  - Prowlarr's manifest is `Consent::Account`.
- The checked-phrase table in `docs/legal/README.md`.
- Registration through `Prowlarr::at_start`. A store error at start-up fails the start.
- Commit 9e7285e5's binary test is brought back.
