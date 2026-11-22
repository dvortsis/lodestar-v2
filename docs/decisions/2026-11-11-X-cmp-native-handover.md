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
  - **Before anything is downloaded,** an install asks whether it can be handed over at all (`ApiConfigure::can_hand_over`). It's refused when the sink isn't there (`not_wired`). A first install is also refused when a `lodestar-qbittorrent` entry already exists (`managed_client_in_use`). That entry isn't this install's: it's one you kept from a guided qBittorrent you forgot (CMP-5g-2's L3), which is then your own working client, or one a crash left. You remove it in Integrations first. An update isn't asked about it, since the entry is its own (self-review B1 and I5).
  - **A companion you added and confirmed isn't installed over** (`already_added`, the guided steps' own code): forget it first, as the guided steps refuse one Lodestar installed (self-review B1).
  - **Install:** after CMP-8b's configuration and before the journal's `configured` step. A failed hand-over fails the install as any failed step does (`step: handover`): it's undone, so a first install leaves nothing behind and an update starts the previous version again. It's never reported as installed. Its refusals reach the API as `409` with their code (`sink_refused`, `managed_client_in_use`, `not_wired`, `timed_out`), not as an internal error. No route or shape changes.
  - **Regenerate:** in the same step, with the new credential, after the rotation and whether or not the companion then starts. So Integrations, the companion's row and its own file always hold the same password. This supersedes the "Not handed over" note in `2026-11-11-CMP-5g-3.md`.
  - **Restart:** too. A restart is the way to retry a hand-over that failed.
  - **Serve's own start of an installed companion:** no. Each start would otherwise rewrite `integrations` on every boot.
- **Bounded by `LIVE_DEADLINE`**, as every configuration is.

## The partial state, and the retry
- **Install:** there is none. The install fails and is undone, and you see the failed install (`companions.install_failed`, step `handover`). You install again.
  - A first install undone after its hand-over was tried (one that saved and then failed counts) takes the managed entry out again (`ApiConfigure::release`). It can do that safely, because the check before it began found no entry there. So no managed entry points at nothing. A failed release is logged (step `release`, reason `rollback_failed`).
  - A first install that fails before its hand-over leaves the settings as they are.
- **Regenerate or restart whose hand-over fails:** the companion has its credential, and Integrations doesn't yet have the new one. Your download-client source then fails to sign in until the hand-over succeeds. It's logged as `companions.install_failed`, step `regenerate` or `restart`, reason `sink_refused` (or `not_wired`), never with a value. **Retry:** restart the companion.
- **A crash between the hand-over and the journal's `configured` step** is undone by recovery at the next start, which can't reach the sink yet (it's built later in serve). So the managed entry stays, pointing at a port nothing listens on.
  - **The gate is fooled by it** (self-review I2): with no qBittorrent of Lodestar's, the managed entry counts as your own client (CMP-5g-2's L3), so the download-client source is released against a client that isn't there. Its own requests then fail, as for any client that's down.
  - The next install refuses it (`managed_client_in_use`) until you remove it in Integrations.
  - **Accepted residual, for the runner:** one crash in a window of one settings write. A journalled "handed over" marker, released by recovery once the sink is wired, would close it; it isn't queued.

## Removal
- **The managed entry is taken out first**, then the companion is removed (`ApiConfigure::release`, before the removal's journal, bounded by `LIVE_DEADLINE`). Only `lodestar-qbittorrent` goes. Every other client stays, without its password re-sent, which keeps the saved one (the registry's rule).
- **Only for a companion Lodestar installed:** the journal is checked first (`not_installed`). An install or update a crash left in it refuses (`busy`) before anything is released, while a removal it left is taken up again (self-review P3). With an installed qBittorrent there's no guided record: a guided step refuses one Lodestar installed, and an install refuses one you added. So the entry is this install's.
- **A release that fails refuses the removal** (`step: release`) before anything is deleted, so you can try again. If the removal fails after a successful release, the companion is still installed without its entry, and a restart hands it over again.
- **Without the sink,** a qBittorrent install, restart, regenerate or removal is refused at the hand-over or the release (`not_wired`): it never counts as done without being the download client. The sink is wired first in `companion_wiring::serve`, before the guarded client or the watch can fail, so it's missing only when companions are off (self-review I3).

## Saving the list (self-review I1, I4)
- **Every write of `integrations.clients`** (the hand-over's and the release's; **superseded by X-cmp-handover-followups**, which reads both in one read) reads the section's revision, then the list, and saves with `if_revision`. A save of yours in between makes it stale, and it's made again from the list as it is then, at most three times. The revision is read first because the registry bumps it before it publishes the new list. So the window left is the instant between those two steps inside one save, not the whole read-then-save. The guided path's hand-over gets the same protection.
- **A restart's hand-over keeps what you changed on the managed entry,** its name and category, unless the hand-over sets its own category. It replaces only the address, the user name and the password.
- **Not shared:** a restart's hand-over and its start each have their own `LIVE_DEADLINE`, so one restart can take up to twice that (self-review P2).

## Prowlarr
Not handed over here. CMP-5g-1's `hand_over_prowlarr` writes to the Prowlarr source, which isn't registered yet (`X-prowlarr-settings`, `X-prowlarr-register`). Through the native path its first step, finding the source, would fail, and with it every Prowlarr install. So `Wiring::hand_over` and `release` do nothing for it, and X-prowlarr-register adds it with the source.

## Credentials
- **The password goes only to the registry's save,** which seals it. It's never in a log line: CMP-8b logs `companions.config_handed_over` with the companion and the entry's kind only, and the failures log codes only.
- **It's never in an API reply.** A companion's row has no credential field, and errors carry codes.
- **Tested:** the captured log lines of an install and a restart don't contain the stored password, and they do name the hand-over event (so the check isn't vacuous: self-review P6). A companion's row and a failed hand-over's or release's error body don't contain it either.

## No API change
No route or response shape changes. The wizard can tell the hand-over happened from what's already there: the install answers only once it's handed over, and the managed entry is in `integrations.clients`.

## From #672's review (runner, folded in here)
- **The runtime config is read once:** opened, checked through the same handle (a regular file, at most 64 KiB) and read through a cap one byte over it, so what's read is what was checked. It's read in `spawn_blocking`, off the async runtime, and every error, a failed task included, fails closed (`route_not_expressible`).
- **On start, Prowlarr may use the proxy saved in its own database** (an earlier SOCKS5 route) for the seconds before CMP-8b sets the new one. It never goes direct in that time: with its own proxy saved it uses that one, and with none it uses the launch variables.
- **The deadline tests shorten the deadline only after the start,** which writes the credential's PBKDF2 hash and can take more than a second on a slow runner. The configuration-timeout test does it through a restart held at its health check, so only the follower's configuration runs under the short deadline.

## Self-review (fresh reviewer)
2 blocking, 5 important and 6 polish findings.
- **Fixed:** B1 (an entry you're using is never overwritten or deleted, above), B2 (the reply tests), I1, I3 to I5, P1, P3, P4 (the wording is "before `configured`"), P5 (`Wiring`'s check and release are tested over a fake sink) and P6.
- **Left, recorded above:** I2, the crash residual with the decisions file corrected, and P2.

## Tests
- **`crates/companions/tests/manager.rs`, over a fake that runs CMP-8b's real `hand_over` on the manager's receipt:**
  - an install hands qBittorrent over with the password its row holds, and a restart again;
  - the log lines carry no password;
  - a refused hand-over fails the install (`handover`), leaves nothing on disk and no rows, releases what it may have saved, and a second try succeeds;
  - regenerate hands the new password over, a refused one is reported, and a restart retries it;
  - a removal of nothing installed releases nothing;
  - a refused release keeps everything installed, and then the removal goes through.
- **`crates/lodestar/src/companion_wiring.rs`:** the managed entry is taken out through a real registry, yours kept with its saved password, and nothing is saved when there's none.
- **`crates/lodestar/src/companions.rs`:**
  - without the sink, qBittorrent's check and release are refused, and Prowlarr's are nothing;
  - with a sink and an entry there, a first install is refused and an update isn't;
  - a release forgets through the sink;
  - an added companion isn't installed over;
  - no reply carries the password.
- **`crates/lodestar/src/companion_wiring.rs` also:**
  - a save made between the read and the hand-over's own is kept, and the hand-over is made again;
  - your name and category survive it.
- **`crates/companions/tests/manager.rs` also:**
  - an install that fails before its hand-over releases nothing;
  - one refused by the check never downloads or starts;
  - only a first install is asked about an existing entry.
- **Mutation-checked:** removing each of these fails a test:
  - the install's hand-over;
  - the removal's release and its journal check before it;
  - the regenerate and restart hand-over;
  - the undone install's release, and its "was it tried" condition;
  - the removal's journal-operation check: **corrected by X-cmp-handover-followups**. #679's review found that this one survived, since no test asserted that nothing was released. That test now does (`2026-11-11-X-cmp-handover-followups.md`);
  - `if_revision`;
  - keeping your name;
  - the existing-entry refusal;
  - the added-companion refusal.
- **Not tested here:** the binary's `Wiring::hand_over` with a real receipt. Only the manager can mint one, and the binary can't use the `testing` feature. Its two halves are tested apart: CMP-8b's `hand_over` with an install receipt (above and CMP-8b's own tests), and the sink's save through a real registry (`the_sink_adds_the_managed_client_and_keeps_yours`).
