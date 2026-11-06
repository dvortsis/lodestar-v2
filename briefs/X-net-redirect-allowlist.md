# X-net-redirect-allowlist: drop every non-allow-listed header on a cross-origin redirect in the integrations client
**Model:** Opus, with `/security-review`. The runner reviews the result with a fresh Opus reviewer. Runner brief, 2026-11-06T04:16Z, from the review of #521 (X-sources-low). Low severity, and it tightens behaviour, so it's the runner's call.

## Finding
- **Where:** `crates/integrations/src/net.rs`, `GuardedClient::send_following` (around lines 823-850). On a cross-origin 307/308 it removes only `Authorization` and `Cookie`, a deny list.
- **Why it matters:** the callers today (ntfy, webhooks) set no other credential header. But a webhook or notifier that later carries `X-Api-Key`, `X-Auth-Token`, `Proxy-Authorization`, ntfy's `X-Tags` with a secret, or any user-configured header would forward it to whatever origin the redirect names.
- **Contrast:** the sources egress (`crates/sources/src/egress.rs`, `CROSS_ORIGIN_HEADERS` and `send_following`, around lines 1223-1280) already uses an **allow list**: only `accept`, `accept-language`, `content-type`, `content-length` and `user-agent` survive a change of origin.

## Do
1. In `GuardedClient::send_following`, on a hop whose origin differs (scheme, host or port), keep only an allow list of headers and drop everything else.
   - Start from the sources list. Add a header only if a current integrations caller needs it across origins **and** it can't carry a credential. Name each addition and why in the decisions file.
   - `crates/integrations` shouldn't gain a dependency on `crates/sources` for this. A local `const` with a comment pointing at the sources list is fine.
2. **Keep everything else as it is:** 307/308 only, the https→http refusal, `MAX_REDIRECTS`, the per-hop SSRF checks, and same-origin hops keeping their headers.
3. **Tests**, in net.rs's tests or `crates/integrations/tests`, using the existing local test servers, with no fixed sleeps:
   - a cross-origin 307 and 308 carry none of `Authorization`, `Cookie`, `Proxy-Authorization`, `X-Api-Key` or an arbitrary custom header, asserted at the second server;
   - the allow-listed headers and the body survive;
   - a same-origin hop keeps every header;
   - a change of port alone counts as cross-origin.
4. **Docs:** update `crates/integrations/README.md` where it describes `send_following`, and the doc comment.
5. **Decisions file:** `docs/decisions/2026-11-06-X-net-redirect-allowlist.md`.

## Own
- `crates/integrations/src/net.rs` (`send_following` and its tests);
- `crates/integrations/README.md`;
- the decisions file;
- your own `runner/slices.yaml` entry, one line in flow style with `deps: []`. Add a `runner/dashboard.yaml` placement only if no rule covers it.

## Done when
These pass: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p lodestar-integrations` plus every crate that depends on it (per crate if the disk is short; say so), `cd tools/status-connector && npm test`, and `/security-review` with no open findings.

Branch `slice/X-net-redirect-allowlist`. Open the PR titled "X-net-redirect-allowlist: Keep only allow-listed headers on a cross-origin redirect". Follow `.claude/skills/steward/SKILL.md`. Never merge. Push once per round. No fixed sleeps. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server.
