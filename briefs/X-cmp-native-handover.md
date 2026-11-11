# X-cmp-native-handover: hand a natively installed companion to Integrations (Opus, core, /security-review)

**Why:** #665's review, B1. On main, `Service::install` (`crates/lodestar/src/companions.rs:~1085`) and `manager::Api::configure` (`crates/companions/src/manager.rs:~1790-1811`) only plan and apply. Only the guided path (`Guided::finish`, `companion_wiring.rs:~1117`) calls `configure::hand_over`. So a qBittorrent that Lodestar installs never becomes the user's download client, and the wizard's success message (CMP-9c) can't be true.

## Build
1. **After a successful native install and configure of qBittorrent,** hand it to `integrations.clients` as `MANAGED_CLIENT` through the same sink and `configure::hand_over` the guided path uses:
   - its loopback address and port from the launch profile;
   - the generated WebUI credential, sealed in the store exactly as guided does.

   Do it in the same step as the install, so a failed hand-over reports as a failure, not as "installed". Define the partial state, installed but not handed over, and say what the user sees and how they retry.
2. **Regenerate redoes the hand-over in the same step,** as CMP-5g-3's decisions file requires ("When that hand-over lands, regenerate must redo it in the same step").
3. **Remove takes the managed client entry out again.** It never removes a client the user added.
4. **Prowlarr:** only if the existing `hand_over_prowlarr` applies cleanly to the native path. Otherwise leave it out and note it; X-prowlarr-register handles the source later.
5. **No new HTTP route or response shape** unless one is unavoidable. If the wizard needs to know the hand-over happened, prefer a field already on the companion row. Ask the runner before you change the API.
6. **Tests:**
   - install, then the client appears;
   - a hand-over failure is reported, and the install is not reported as a success;
   - regenerate updates the stored credential;
   - remove deletes only the managed entry;
   - the credential never appears in logs or API replies.

   No fixed sleeps.
7. **Decisions file:** `docs/decisions/2026-11-11-X-cmp-native-handover.md`.

## Owns
- `crates/lodestar/src/companions.rs`
- `crates/lodestar/src/companion_wiring.rs`
- `crates/companions/src/manager.rs` (only if needed)
- `crates/companions/src/configure/**` (only if needed)
- their tests
- its decisions file

## Deps
X-cmp-egress-prelaunch, which owns `companions.rs`, `manager.rs` and `configure/prowlarr.rs`. Run it in the same session after that slice merges.

Branch `slice/X-cmp-native-handover`; PR title "X-cmp-native-handover: <title>". Follow the steward skill.
