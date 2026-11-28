# X-vpn-bind-ui (runner brief, 2026-11-28)

**Slice:** X-vpn-bind-ui (slice B of X-vpn-bind). Opus: it adds a Settings API field fed by user
input, then the UI.
**Branch:** `slice/X-vpn-bind-ui` from current `main`.
**PR title (exact):** `X-vpn-bind-ui: Settings can limit the crawl to a VPN interface, and the UI says when it is down`
**Owns:** refined at step 0. Expect the settings schema and field code in `crates/lodestar/src/settings/`
(not `golden/logs_catalog.txt`), `web/src/lib/features/settings/**` (not `logging/**`), the
status-pill and crawler-state UI, `docs/ui-glossary.md`, and `docs/decisions/*-X-vpn-bind-ui.md`.

## Source
`docs/decisions/2026-11-28-X-vpn-bind.md` (slice A, #1167, merged). Read all of it. Slice A
added `dht.interface` in `config.toml` only, the fail-closed binding, the guard, `GET /crawler`
`state: "vpn_down"` with `interface: {interface, state, reason}`, the stats page's
`crawler.reason` `vpn_down`, and the events. Owner §2.85 (D8): the VPN is optional and works
with any provider.

## Do
1. **Step 0.** Send the runner:
   - how a Settings field maps to `config.toml` today, and how restart-required fields are shown;
   - where `crawler.state` and `crawler.reason` would surface (the status pill, Overview);
   - the wording you propose, in en and zh, for the field label, help, the VPN-down state and
     each `reason`;
   - the refined owns, and whether openapi.json, `schema.d.ts` or any `.size-limit.mjs` entry
     changes.

   Then wait for the reply.
2. **The field: `crawler.interface`** in Settings → Crawler (or wherever step 0 shows it
   belongs). It maps to `dht.interface`, is restart-required, and is empty by default, meaning
   any route.
   - Validate with the existing `Interface::parse`. Don't write a second validator; the server
     is the authority, and the UI shows its error.
   - **On Windows, a set value fails the start until X-vpn-bind-win lands.** The field must say
     so on Windows, and saving a value there must be refused with that reason rather than
     leaving a server that won't start. Propose how at step 0.
   - **A remote worker (`lodestar worker`) with the setting refuses to start** until
     X-vpn-bind-worker lands. Say so in the help or Learn more.
3. **Show "VPN down."** Use the term the glossary will carry. Show `crawler.state ==
   "vpn_down"` where the paused state shows today, with the reason in plain words. It must read
   as "the crawl stopped to protect you", not as an error the user caused.
4. **Not covered.** The decisions file's "Not crawl traffic" list includes the torrent cache, and
   the security review flagged it (info). With an interface set, the help must say plainly that
   the torrent cache and the source plugins aren't limited to it, and point to each source's
   own egress setting.
5. **Glossary:** add the VPN-down term and the interface field to `docs/ui-glossary.md`, en
   and zh, with zh machine-drafted and marked as such.
6. **UI rules** (CLAUDE.md): tokens only, spacing on the ramp, en and zh messages, and
   accessible status text.
7. **Tests:** Rust tests for the field round trip, Windows refusal (cfg-gated plus a portable
   unit test of the decision), and validation errors surfacing; web unit tests; and the
   settings and status e2e specs updated. Condition waits only, at least 10 s.
8. **Avoid:** `crates/pipeline/src/log_events.rs`, `crates/lodestar/src/settings/golden/logs_catalog.txt`,
   `web/src/lib/features/settings/logging/**`, and the `pipeline` entry in `web/.size-limit.mjs`
   (X-healthz-count-async is open), plus `web/src/lib/features/search/toolbar/**`
   (X-hidden-count-wide-shift). If you need one of these, say so at step 0.
9. Decisions file, the CLAUDE.md pre-push checks for Rust and `web/`, and
   `.claude/skills/steward/SKILL.md`. Report `ready <PR> <sha>` to the runner.
