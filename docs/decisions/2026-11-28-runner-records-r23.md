# runner-records-r23 decisions (#1170)

This file records the runner's calls since r22 (#1164). The rulings are the runner's, under §2.26 and its reading of §2.85–§2.88. A ruling that only grants owns or records a slice isn't flagged. The owner can veto any flagged call.

## The owner's tasks done
- **Items 2 and 9 were already done.** GitHub's API dates both changes to 2026-11-07, shortly after the runner's release-text draft, so the queue entries were stale. On 2026-11-28 the owner re-ran them with a PowerShell script the runner wrote, which found nothing left to change. The runner confirmed both through the API.
  - The "Protect main" ruleset requires `ci-ok`, `web-ok` and `e2e-ok`.
  - The v0.5.1 release page matches the runner's draft.
- **Item 8** needed nothing: the branch was already gone.
- All three leave the owner queue.
- **Effect of item 2:** a known flake inside `web-ok` or `e2e-ok` has blocked a merge since 2026-11-07, as one inside `ci-ok` always did. The §2.18 "not this PR's" rule still applies to checks outside the required three. Recurring flakes in the required checks get fixed first; X-live-banner-flake (#1168) was the first.
- **The DHT router addresses** that X-vpn-bind writes into the code were resolved by the owner on their own machine on 2026-11-28, because this environment has no DNS.
  - Three matched.
  - `dht.transmissionbt.com` had a second address, which was added.

## Rulings since r22
- **X-vpn-bind (#1167)**
  - **DNS fails closed: no lookups at all with an interface set.** This is built in this slice rather than split off. With an interface set:
    - nothing goes through the system resolver;
    - the DHT joins from IP-literal `dht.bootstrap` entries and the five checked router addresses (the runner ruled these are added only when the list is the default one);
    - host-named bootstrap entries and trackers are skipped and logged once per start;
    - the HTTP tracker client refuses every name, including on redirects.

    Building it in the slice keeps any commit on `main` from carrying the setting with the leak. The size (about 2,600 hand-written lines) is stated in the PR. A resolver pinned to the interface stays a later way to get hostname trackers back.
  - **The setting covers crawl traffic only,** as §2.85 names it: the DHT and fetch sockets and the trackers they use. The torrent cache, source plugins and companions keep their own egress settings (SRC-5). Settings warns about this when an interface is set (X-vpn-bind-ui).
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
    - The Linux HTTP tracker client uses the same TLS settings as the rest of the app.
    - **Windows weak-host send is closed in code, not left as a stated limit.** Windows may send a packet bound to the VPN's address out through another adapter. Crawl sockets on Windows set `IP_UNICAST_IF` / `IPV6_UNICAST_IF` to the VPN interface, and a failure to set it fails closed. If this can't be done within the repository's rules (no `unsafe` in the lodestar crate), the runner asks the owner before merging rather than ship a documented limit.
- **X-watch-search-wait (#1166):** the cause was most likely the CI test server joining the live DHT and storing real torrents that matched two common words. The test now runs with the DHT off, searches for a unique token and matches the exact hit. The test is stricter, not looser. The decisions files of #1166 and #1169 gain their PR numbers in their headings here.
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
4. **The test-VM volume launch also requires `Environment=Test` and `LaunchedBy`,** beyond item 31's `Purpose` (X-testvm-iam-volumes, #1169). This only tightens the policy, so the delete can trust a `LaunchedBy` that IAM set.
5. **The zero-result chips appear unasked, as §10.4 says,** not "only when asked" as the slice's title said (X-zero-rescue).
6. **The VPN setting covers crawl traffic only** (X-vpn-bind). The torrent cache, source plugins and companions keep their own egress settings, and Settings warns about this when an interface is set.
