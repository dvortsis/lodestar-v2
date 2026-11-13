# X-gluetun-templates (cloud testing, §2.62; Opus; runner brief 2026-11-13)

Make the generated gluetun companion templates (compose and Unraid) carry the leak-test post-rules, so a user who installs them gets the same protection the fleet tests. Follows #800 (X-gluetun-leak) and #808 (X-gluetun-leak-cmp), both merged.

## Read first
- `docs/decisions/2026-11-12-X-gluetun-leak.md`: the sections "X-gluetun-leak-cmp (the runner's follow-up, Opus): brief notes", "Not done here (for the runner)" and "Follow-ups (X-gluetun-leak-cmp)". They are the spec for this slice.
- `crates/companions/src/compose.rs` (the compose and Unraid generators, the gluetun overview at about line 457), `crates/companions/src/unraid.rs`, and `crates/companions/tests/templates.rs` (goldens, the property test, and the no-extra-files rule).
- `packaging/companions/gluetun-post-rules.txt`, `infra/fleet/steps/vm/compose.yaml` (the `configs:` pattern), and `docs/user/03-keep-lodestar-safe.md` (the manual steps and the **(planned)** note).
- `.claude/skills/steward/SKILL.md`, and `.claude/skills/user-docs/SKILL.md` for the guide edit.

## Do
1. **Compose:** gluetun gets a `configs:` entry whose inline `content:` is the rules file embedded at build time (`include_str!` of `packaging/companions/gluetun-post-rules.txt`), plus `VPN_INTERFACE` matching the VPN type: `wg0` for WireGuard, `tun0` for OpenVPN (the rules' interface name must match; see the OpenVPN note). The routed companion starts only once gluetun is healthy (`depends_on: gluetun: condition: service_healthy`, O3).
2. **Unraid:** a read-only Path, host `<gluetun folder>/post-rules.txt` → `/iptables/post-rules.txt`, and the `VPN_INTERFACE` Variable. A missing host file fails closed (the decisions file checked this). Work out who writes the host file. If writing it needs code outside your owns (for example `crates/companions/src/manager.rs`, which #805 X-cmp-egress-api has open), stop and message the runner with the smallest proposal. Don't edit it.
3. **Startup ordering on Unraid:** use the autostart wait, or say in the template's Overview what the user must do.
4. **Hosts without ip6tables:** decide whether Lodestar emits an IPv4-only rules variant there, or keeps one file and the guide's troubleshooting row. Record the call. It's a §2.19 flag to the owner.
5. **Correct the Overview** ("Its firewall blocks all traffic while the VPN is down") so it says what is true.
6. **Regenerate the goldens** with `UPDATE_GOLDEN=1 cargo test -p lodestar-companions --test templates`, plus the `packaging/companions/{compose,unraid}` examples. Add a test that the embedded text equals the rules file byte for byte, and that every generated rules interface name matches the template's `VPN_INTERFACE`.
7. **Guide:** drop the **(planned)** note in `docs/user/03-keep-lodestar-safe.md` and describe what the templates now do. Keep the manual steps for an existing gluetun that Lodestar routes through by name (out of scope).
8. **Decisions:** add a section "X-gluetun-templates" to `docs/decisions/2026-11-12-X-gluetun-leak.md` with the defaults you chose. The defaults are a runner §2.19 flag, so name them in the PR description.

## Owns
- `crates/companions/src/compose.rs`, `crates/companions/src/unraid.rs`
- `crates/companions/tests/templates.rs`, `crates/companions/tests/golden/**`
- `packaging/companions/**`
- `docs/user/03-keep-lodestar-safe.md`, `docs/decisions/2026-11-12-X-gluetun-leak.md`
- `runner/slices.yaml` (your own entry only: replace its owns with this list, and drop `infra/akamai/terraform/pia-example/compose.vpn.yaml`, which #808 already did; #632 is the owner's and also edits that file)

## Don't
- Don't touch the rules file's content. If it must change, ask the runner first; its SHA is pinned in the docs.
- Don't touch `crates/companions/src/manager.rs` (#805), `infra/**` or `compose.vpn.yaml`.
- Don't add fixed sleeps. Condition waits need deadlines of at least 10 s.
- No model identifiers, no 12-digit numbers, UTC only.

## Checks before every push
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (at least `-p lodestar-companions`), and `cd tools/status-connector && npm test` after the slices.yaml edit. Run /security-review on the diff: gluetun runs the rules as root.

## PR
Branch `slice/X-gluetun-templates`, title `X-gluetun-templates: <title>`. Bring main in first: #799 (CMP-12) touches compose.rs and lib.rs, and #809 adds your slices.yaml entry; the runner tells you when both have merged. Message the runner when the PR is ready or blocked.
