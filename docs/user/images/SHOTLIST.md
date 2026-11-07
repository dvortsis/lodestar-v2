# Release screenshots and pending captures

This guide targets v0.5.1. Existing Search, detail, Discover and Insights images were generated from mock fixtures by `npm run docs:shots`. They are examples, not captures of the published binary. Refresh them against v0.5.1 before treating them as exact release evidence.

The setup illustration uses labels from `web/src/lib/features/auth/messages/en.json` and step order from `SetupForm.svelte` at tag v0.5.1. The Windows illustration uses `packaging/windows/main.wxs` at that tag. Both are stamped illustrations. Unraid illustrations use the template fields.

| Chapter | Screenshot | Source | Status |
|---|---|---|---|
| 2.1 | Unraid Add Container with Repository pinned to `0.5.1` and Data on the pool | Owner | Pending host capture; `unraid-template-illustration.svg` shows the main fields |
| 2.3 | PUID 99 and PGID 100 in Advanced View | Owner | Pending host capture; `unraid-puid-pgid-illustration.svg` |
| 2.5 | Container log with setup token redacted | Owner | Pending host capture |
| 2.5 | v0.5.1 setup: token, Legal Notice, then password | Release UI | Pending release capture; `setup-v051-illustration.svg` replaces the development setup capture |
| 4.2 | Checksum and Minisign verification | Owner | Pending host capture |
| 4.3 | SmartScreen More info and Run anyway | Owner | Pending host capture |
| 4.4 | MSI Options with both boxes unchecked | Owner | Pending host capture; `windows-options-illustration.svg` |
| 5.1 | Search with facets and composition bars | Existing mock fixtures | Present; pending v0.5.1 verification (`search-light.png`, `search-dark.png`) |
| 5.2 | Torrent detail drawer | Existing mock fixtures | Present; pending v0.5.1 verification (`detail-light.png`, `detail-dark.png`) |
| 5.3 | Discover and Insights | Existing mock fixtures | Present; pending v0.5.1 verification (`discover-{light,dark}.png`, `insights-{light,dark}.png`) |
| 5.3 | Status pill in normal and stalled states | Release UI | Pending release capture |

`setup-notice.png`, `setup-screen.png` and `sources-{light,dark}.png` remain development assets. They are not used as v0.5.1 setup or Sources instructions. Sources, webhook setup and safe restart are outside this release guide.
