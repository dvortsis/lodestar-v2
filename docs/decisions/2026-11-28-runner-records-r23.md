# runner-records-r23 decisions (#PR)

This file records the runner's calls since r22 (#1164). The rulings are the runner's, under §2.26 and its reading of §2.85–§2.88. A ruling that only grants owns or records a slice isn't flagged. The owner can veto any flagged call.

## The owner's tasks done (2026-11-28)
- **Items 2 and 9** were done by the owner with a PowerShell script the runner wrote.
  - The "Protect main" ruleset now requires `ci-ok`, `web-ok` and `e2e-ok`.
  - The v0.5.1 release page now matches the runner's draft.
  - The runner checked both through GitHub's API.
- **Item 8** needed nothing: the branch was already gone.
- All three leave the owner queue.
- **Effect of item 2:** a known flake inside `web-ok` or `e2e-ok` now blocks a merge, as one inside `ci-ok` always did. The §2.18 "not this PR's" rule still applies to checks outside the required three. Recurring flakes in the required checks get fixed first; X-live-banner-flake (#1168) was the first.
- **The DHT router addresses** that X-vpn-bind writes into the code were resolved by the owner on their own machine on 2026-11-28, because this environment has no DNS.
  - Three matched.
  - `dht.transmissionbt.com` had a second address, which was added.

## Rulings since r22
- **X-vpn-bind (#1167)**
  - **DNS fails closed (design (c)), built in this slice rather than split.** With an interface set:
    - nothing goes through the system resolver;
    - the DHT joins from IP-literal `dht.bootstrap` entries and the five checked router addresses (these only when the list is the default one);
    - host-named bootstrap entries and trackers are skipped and logged once per start;
    - the HTTP tracker client refuses every name, including on redirects.

    Building it in the slice keeps any commit on `main` from carrying the setting with the leak. The size (about 2,600 hand-written lines) is stated in the PR. The pinned-resolver design (a) stays a later way to get hostname trackers back.
  - **The setting lives in `config.toml` only** (`dht.interface`, restart to apply). The Settings field, its strings and `schema.test.ts` move to X-vpn-bind-ui, because a settings Field can't be API-only.
  - **Outside owns, granted, each limited to the change it needs:**
    - `crates/utp/src/shared.rs` (`reattachable()`, which the review confirmed is necessary and safe);
    - `web/licenses/rust.json` and `fuzz/Cargo.lock` (the if-addrs entries);
    - the `ops_crawler` api goldens;
    - the logs catalogue, event-name chunks and en/zh names for its events;
    - the dht and pipeline event-name budgets in `web/.size-limit.mjs`, at the measured size rounded up to 64 B.
  - **The review round's calls:**
    - EADDRNOTAVAIL trips the guard on Windows only. On Linux, running out of ephemeral ports gives the same error.
    - Linux refuses a socket family the interface has no address for, as Windows does.
    - Linux refuses a specific `dht.bind` together with an interface.
    - Linux HTTP uses the repo's routed TLS configuration.
- **X-watch-search-wait (#1166):** the cause was most likely the CI test server joining the live DHT and storing real torrents that matched two common words. The test now runs with the DHT off, searches for a unique token and matches the exact hit. The test is stricter, not looser. The decisions file's heading gains its PR number here.
- **X-live-banner-flake (#1168):** the mock recorded the live-search registration before the page had its answer, so an event sent then was dropped. `events.send` now waits for an open stream with the event's topic, and the spec waits until the page has read the answer.
- **X-detail-attributes:** `TorrentDetail.attributes` is the same map `/compare` returns, built by one shared helper. Showing it is the new slice X-detail-attributes-ui, because each key needs a plain label.
- **X-zero-rescue:** §10.4 and the r5 UI decision win over the slice's title. The natural-phrasing chips are offered unasked, but only on zero or few results; the button to ask stays. "Did you mean" only offers a corrected query; applying it takes one click.
- **New slices:**
  - X-upload-screen-flake: a vitest flake seen once in a full run;
  - X-events-send-await: await the remaining unawaited `events.send` calls;
  - X-detail-attributes-ui.

## Flags for the owner (§2.19)
1. **Host-named trackers are skipped while a VPN interface is set** (X-vpn-bind). Most public trackers are named, so tracker health is mostly lost with the setting on. Swarm counts still come from BEP 33 over the DHT.
2. **The open `/metrics` page now shows whether the VPN interface is up, and how often it dropped** (X-vpn-bind). It shows no names or addresses. `/metrics` is unauthenticated by existing policy.
3. **EADDRNOTAVAIL counts as VPN down on Windows only** (X-vpn-bind). On Linux a drop is still caught by the bound socket's own errors and the 2 s check.
4. **Windows weak-host routing is a stated limit for the VPN guide, not code** (X-vpn-bind). Windows may send a packet bound to the VPN's address out through another adapter that forwards traffic. The guide will say to use a full-tunnel VPN and no connection sharing on other adapters.
5. **The test-VM volume launch also requires `Environment=Test` and `LaunchedBy`,** beyond item 31's `Purpose` (X-testvm-iam-volumes, #1169). This only tightens the policy, so the delete can trust a `LaunchedBy` that IAM set.
6. **The zero-result chips appear unasked, as §10.4 says,** not "only when asked" as the slice's title said (X-zero-rescue).
