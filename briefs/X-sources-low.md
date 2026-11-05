# X-sources-low: three low hardening findings from the runner's security pre-audit
**Model:** Opus, with `/security-review`. The runner reviews the result. Runner brief, 2026-11-05T23:55Z, from the pre-audit of main at 63a2c6df. None of these is reachable today; they are defence in depth before more plugins are wired. Each tightens behaviour, so each is the runner's call.

## Do
1. **L1. The egress host list ignores the port.**
   - `Endpoints::check` (`crates/sources/src/egress.rs:241-254`) compares `host_key`, which drops the port.
   - Prowlarr's `only_hosts(&[host])` runs under `NetPolicy::download_client()`, which allows loopback, so any port on the configured host passes.
   - **Fix:** key the list on (scheme, host, port), with the scheme's default port when none is given.
   - **Tests:** another port on the same host is refused, and the default port matches an explicit one.
2. **L2. `send_following` doesn't strip every sensitive header on a cross-origin redirect.**
   - The strip list (`egress.rs:1179-1188`) misses StashDB's `ApiKey`.
   - **Fix:** strip every header the request itself set (or a per-request sensitive list) on any cross-origin hop.
   - **Test:** a redirect to another origin carries neither `ApiKey` nor any request-set header.
3. **L4. The arr webhook title cleaner** (`crates/sources-plugins/src/arr.rs` `clean_title`/`is_format`, around lines 385-410) uses its own, narrower character set.
   - **Fix:** use the shared `lodestar_metainfo::hidden_char` (or `common::title`), so U+00AD, U+061C, U+180E, U+FFF9-FFFB and the tag characters U+E0001-E007F are removed.
   - **Test:** each of these is stripped.
4. **Decisions file:** `docs/decisions/2026-11-06-X-sources-low.md`.

## Own
- `crates/sources/src/egress.rs`, only `Endpoints::check` and `send_following`. X-tcache-recheck may add a source-enabled check in `PluginEgress`; keep away from that, and bring `main` in if it merges first;
- `crates/sources-plugins/src/arr.rs` (the cleaner only);
- their tests;
- the decisions file;
- your own `runner/slices.yaml` entry.

## Done when
These pass: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test -p lodestar-sources -p lodestar-sources-plugins`.

Branch `slice/X-sources-low`. Open the PR titled "X-sources-low: Egress port check, redirect header strip, shared title cleaner". Follow `.claude/skills/steward/SKILL.md`. Never merge. Push once per round. No fixed sleeps. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server.
