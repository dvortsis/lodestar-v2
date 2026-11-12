# X-companion-pins-polish decisions

Follow-ups #720 (X-cmp13-installer-pins) named as outside its paths (runner, 2026-11-12). Nothing here changes a rule. The workflow's `permissions:`, secrets, triggers and environments are unchanged.

- **A concurrency group per companion:** `<workflow name>-<companion>` (`${{ github.workflow }}-${{ inputs.companion }}`), with `cancel-in-progress: false`.
  - GitHub keeps one pending run per group. With one group for the whole workflow, a third dispatch cancelled the pending second one: the qBittorrent dry run was cancelled when gluetun queued behind it (runner, 2026-11-11).
  - Two dispatches of one companion still queue one after the other.
  - Different companions now run side by side. They write nothing shared: each run checks out `main` and uploads its own artifact.
- **The `artifacts` input text** no longer says installers are skipped by default. Since #720 the default is every artifact the tool's recipe covers, qBittorrent's installer included.
- **`crates/companions/README.md`** documents the installer's own `release` field. It's optional, on an `INSTALLER_SOURCES` host, never a fetch host and never read at run time, and a pin with it is checked against that folder alone. The pin-URL rule also names the exception.
