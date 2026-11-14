# Release screenshots and pending captures

This guide targets v0.5.1. Existing Search, detail, Discover and Insights images were generated from mock fixtures by `npm run docs:shots`. They are examples, not captures of the published binary. Refresh them against v0.5.1 before treating them as exact release evidence.

The setup illustration uses labels from `web/src/lib/features/auth/messages/en.json` and step order from `SetupForm.svelte` at tag v0.5.1. The Windows illustration uses `packaging/windows/main.wxs` at that tag. Both are stamped illustrations. Unraid illustrations use the template fields.

| Chapter | Screenshot | Source | Status |
|---|---|---|---|
| 2.1 | Unraid Add Container with Repository pinned to `0.5.1` and Data on the pool | Owner | Pending host capture; `unraid-template-illustration.svg` shows the main fields |
| 2.3 | PUID 99 and PGID 100 in Advanced View | Owner | Pending host capture; `unraid-puid-pgid-illustration.svg` (its source stamp is `lodestar.xml:28-31` since the unused Hot path and Cold path fields were removed) |
| 2.5 | Container log with setup token redacted | Owner | Pending host capture |
| 2.5 | v0.5.1 setup: token, Legal Notice, then password | Release UI | Pending release capture; `setup-v051-illustration.svg` replaces the development setup capture |
| 4.2 | Checksum and Minisign verification | Owner | Pending host capture |
| 4.3 | SmartScreen More info and Run anyway | Owner | Pending host capture |
| 4.4 | MSI Options with both boxes unchecked | Owner | Pending host capture; `windows-options-illustration.svg` |
| 4.4 | Windows Services showing Lodestar: Automatic (Delayed Start), `NT SERVICE\Lodestar` | Owner | Pending host capture; `windows-service-illustration.svg` (source: `packaging/windows/main.wxs` `ServiceInstall`) |
| 4.6 | Insights exception for a data folder on a FUSE or network filesystem | Fixture capture | Pending; needs a `data_filesystem` fixture in `web/e2e/docs/shots.docs.ts` |
| 4.7 | Insights exception with the `netsh` command | Fixture capture | Pending; needs an `ephemeral_ports` fixture in `web/e2e/docs/shots.docs.ts` |
| 4.8 | Updates status card with a version available and Install now (newer than v0.5.1) | Fixture capture | Pending; needs a Windows updates fixture in `web/e2e/docs/shots.docs.ts`; until then `windows-update-status-illustration.svg` (source: `web/src/lib/features/settings/updates/messages/en.json`) |
| 5.1 | Search with facets and composition bars | Existing mock fixtures | Present; pending v0.5.1 verification (`search-light.png`, `search-dark.png`) |
| 5.2 | Torrent detail drawer | Existing mock fixtures | Present; pending v0.5.1 verification (`detail-light.png`, `detail-dark.png`) |
| 5.3 | Discover and Insights | Existing mock fixtures | Present; pending v0.5.1 verification (`discover-{light,dark}.png`, `insights-{light,dark}.png`) |
| 5.3 | Status pill in normal and stalled states | Release UI | Pending release capture |
| 7.2 | Settings → Outputs with the Torznab address and a key named Prowlarr | Mock fixtures | Present (`outputs-light.png`, `outputs-dark.png`, from `npm run docs:shots`); newer than v0.5.1. The address shows the test server's origin |
| 7.3 | Prowlarr's Generic Torznab form with URL, API Path `/api` and API Key | Owner | Pending capture from Prowlarr, with the key hidden and the version in the caption |
| 7.7 | bitmagnet export section of Settings → Outputs | Mock fixtures | Present (`outputs-export-light.png`, `outputs-export-dark.png`); newer than v0.5.1 |
| 7.7 | bitmagnet export showing Done | Fixture capture | Pending; needs an export-job fixture in `web/e2e/docs/shots.docs.ts` (newer than v0.5.1) |
| 8.5 | Settings → Companions with qBittorrent, its health and the Install dialog (newer than v0.5.1) | Fixture capture | Pending; `companions.routes.test.ts` has no docs shots yet, so add a test to `web/e2e/docs/shots.docs.ts`; until then `images/companions/companions-screen-illustration.svg` (labels from `web/src/lib/features/settings/companions/messages/en.json`) |
| 8.4 | Unraid `flash` share showing `config/plugins/dockerMan/templates-user` | Owner | Pending host capture |
| 8.5 | qBittorrent Tools → Options → Web UI with IP address `127.0.0.1` (Windows) | Owner | Pending capture from qBittorrent, with the version in the caption |

`setup-notice.png`, `setup-screen.png` and `sources-{light,dark}.png` remain development assets. They are not used as v0.5.1 setup or Sources instructions. Sources, webhook setup and safe restart are outside this release guide.
