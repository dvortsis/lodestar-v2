# X-gluetun-templates-hostfile (runner brief, 2026-11-13)

The spec is the slices.yaml entry `X-gluetun-templates-hostfile` (deps X-gluetun-templates #815, merged 5e24a99d; and X-cmp-egress-api, merged). Read its notes in full.

**Owns:** exactly the slices.yaml list:
- `crates/companions/src/{compose,unraid,inputs}.rs`;
- `crates/companions/tests/templates.rs`;
- `crates/lodestar/src/cli/companion.rs`;
- `crates/lodestar/src/companions.rs`.

**Goal:**
- a `gluetun_paths` field on `ServiceInput`, wired through its callers;
- Lodestar writes `post-rules.txt` from its embedded copy, so an Unraid user doesn't save and check the rules file by hand.

**How the file is written:**
- atomically (temp file in the same folder, then rename);
- with no symlink following;
- mode 0644, under the data folder only;
- refusing a path outside it.

**Rules:**
- **Cloud testing first (§2.62):** check the written file with a gluetun container where the cloud-testing lane allows, and report numbers only.
- **Opus review with /security-review:** it writes files from a path a user can set.
- **Your PR's §2.19 list:** keep your five defaults from #815 (the Unraid path default, the embedded copy, `VPN_INTERFACE=wg0`, no IPv4-only variant, the 30 s start wait) in this PR's §2.19 list unless this slice changes them.

Steward skill; PR title "X-gluetun-templates-hostfile: …".
