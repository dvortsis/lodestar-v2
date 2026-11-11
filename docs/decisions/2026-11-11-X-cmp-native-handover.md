# X-cmp-native-handover decisions

A qBittorrent that Lodestar installs becomes the user's download client, as a guided one already does (#665's review, B1; architecture §18.10; plan CMP-5g, Q19). These are the slice's decisions under §2.33. None loosens an owner decision or a security decision. The runner flags them to the owner (§2.19).

## What was missing
Only the guided path (`Guided::finish`) called CMP-8b's `configure::hand_over`. A native install planned and applied qBittorrent's configuration but never wrote it into `integrations.clients`, so the wizard's "It's now your download client" (CMP-9c) couldn't be true.

## How it's handed over
- **Through the same sink and the same `hand_over` as the guided path.** The binary's `Wiring` (the manager's `ApiConfigure` at serve) now holds CMP-5g-1's `Sink`, which `companion_wiring` builds once and also hands to `Guided`. It calls `configure::hand_over` with:
  - `ConfigureInput::installed(receipt)`, on the receipt only the manager mints for this install, so CMP-8b's consent check is the same as for configuring it;
  - its loopback address and port, from the catalogue's health port, as its launch profile uses;
  - its generated WebUI password, opened from the store right before.

  It becomes the `lodestar-qbittorrent` entry ("qBittorrent (Lodestar)"), saved through the settings registry, which seals the password under its own context, exactly as for a guided one. The user name is CMP-3's default, the one its config file was written with.
- **The manager decides when** (`ApiConfigure::hand_over`, a new trait method; the default hands nothing over, as for the CLI's `Unwired`):
  - **Install:** after CMP-8b's configuration and before the commit point (`configured`). A failed hand-over fails the install as any failed step does (`step: handover`): it's undone, so a first install leaves nothing behind and an update starts the previous version again. It's never reported as installed.
  - **Regenerate:** in the same step, with the new credential, after the rotation and whether or not the companion then starts. So Integrations, the companion's row and its own file always hold the same password. This supersedes the "Not handed over" note in `2026-11-11-CMP-5g-3.md`.
  - **Restart:** too. A restart is the way to retry a hand-over that failed.
  - **Serve's own start of an installed companion:** no. Each start would otherwise rewrite `integrations` on every boot.
- **Bounded by `LIVE_DEADLINE`**, as every configuration is.

## The partial state, and the retry
- **Install:** there is none. The install fails and is undone, and you see the failed install (`companions.install_failed`, step `handover`). You install again. A first install that's undone after its hand-over had saved (a later step failing) takes the managed entry out again (`ApiConfigure::release`), so no managed entry points at nothing. A failed release is logged (step `release`, reason `rollback_failed`).
- **Regenerate or restart whose hand-over fails:** the companion has its credential, and Integrations doesn't yet have the new one. Your download-client source then fails to sign in until the hand-over succeeds. It's logged as `companions.install_failed`, step `regenerate` or `restart`, reason `sink_refused` (or `not_wired`), never with a value. **Retry:** restart the companion.
- **A crash between the hand-over and the commit point** is undone by recovery at the next start, which can't reach the sink yet (it's built later in serve). So the managed entry stays, pointing at nothing, until the next install's hand-over replaces it or you remove it in Integrations. It's a residual: one crash in a window of one settings write. The gate isn't fooled meanwhile: with no qBittorrent of Lodestar's, the managed entry counts as your own client (CMP-5g-2's L3), whose health isn't judged.

## Removal
- **The managed entry is taken out first**, then the companion is removed (`ApiConfigure::release`, before the removal's journal). Only `lodestar-qbittorrent` goes. Every other client stays, without its password re-sent, which keeps the saved one (the registry's rule).
- **Only for a companion Lodestar installed:** the journal is checked first (`not_installed`), so a guided qBittorrent's entry is never removed by this.
- **A release that fails refuses the removal** (`step: release`) before anything is deleted, so you can try again. If the removal fails after a successful release, the companion is still installed without its entry, and a restart hands it over again.
- **Without the sink** (`companion_wiring` couldn't start), a qBittorrent install, restart, regenerate or removal is refused at the hand-over or the release (`not_wired`): it never counts as done without being the download client.

## Prowlarr
Not handed over here. CMP-5g-1's `hand_over_prowlarr` writes to the Prowlarr source, which isn't registered yet (`X-prowlarr-settings`, `X-prowlarr-register`). Through the native path its first step, finding the source, would fail, and with it every Prowlarr install. So `Wiring::hand_over` and `release` do nothing for it, and X-prowlarr-register adds it with the source.

## Credentials
- **The password goes only to the registry's save,** which seals it. It's never in a log line: CMP-8b logs `companions.config_handed_over` with the companion and the entry's kind only, and the failures log codes only.
- **It's never in an API reply.** A companion's row has no credential field, and errors carry codes.
- **Tested:** the captured log lines of an install and a restart don't contain the stored password.

## No API change
No route or response shape changes. The wizard can tell the hand-over happened from what's already there: the install answers only once it's handed over, and the managed entry is in `integrations.clients`.

## Tests
- **`crates/companions/tests/manager.rs`, over a fake that runs CMP-8b's real `hand_over` on the manager's receipt:**
  - an install hands qBittorrent over with the password its row holds, and a restart again;
  - the log lines carry no password;
  - a refused hand-over fails the install (`handover`), leaves nothing on disk and no rows, releases what it may have saved, and a second try succeeds;
  - regenerate hands the new password over, a refused one is reported, and a restart retries it;
  - a removal of nothing installed releases nothing;
  - a refused release keeps everything installed, and then the removal goes through.
- **`crates/lodestar/src/companion_wiring.rs`:** the managed entry is taken out through a real registry, yours kept with its saved password, and nothing is saved when there's none.
- **`crates/lodestar/src/companions.rs`:** without the sink, qBittorrent's release is refused, and Prowlarr's is nothing.
- **Mutation-checked:** removing each of these fails a test: the install's hand-over, the removal's release, its journal check before it, the regenerate and restart hand-over, and the undone install's release.
- **Not tested here:** the binary's `Wiring::hand_over` with a real receipt. Only the manager can mint one, and the binary can't use the `testing` feature. Its two halves are tested apart: CMP-8b's `hand_over` with an install receipt (above and CMP-8b's own tests), and the sink's save through a real registry (`the_sink_adds_the_managed_client_and_keeps_yours`).
