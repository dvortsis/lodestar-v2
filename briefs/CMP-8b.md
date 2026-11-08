# CMP-8b: Companion auto-configuration through each companion's API (credentials, categories, save paths, binding or proxy; Lodestar's own settings through an injected sink)

**Model:** Opus. **Review:** a fresh Opus reviewer plus `/security-review`: it writes generated credentials into third-party APIs. **Branch:** `slice/CMP-8b`, cut from `origin/main` (CMP-3 #598 and CMP-8a #610 are in, at 9e567a21). **PR title:** `CMP-8b: <title>`. **Steward:** follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Read first
- `docs/plan/2026-11-06-v06-companions.md`:
  - §4's **CMP-8b** bullet. It's the spec.
  - §3's table row and the Q14 note (Opus).
  - §5 **Q8** (route kinds a companion can follow), **Q18** (each companion's own egress), **Q19** (guided credentials: paste once, seal, replace qBittorrent's temporary password) and **Q21** (FlareSolverr is CMP-12's, never wired here).
- `docs/architecture.md` §18.7 and §18.10.
- `crates/companions/src/adapter.rs` (CMP-0): `Configure`, `ConfigureInput` (non-exhaustive; its doc says CMP-8b adds fields), `ConfigPlan`, `CheckedPlan`, `SettingKey`, `Credentials`, `GuardedHttp`, `ProbeRequest`, `ProbeResponse`, `AdapterError`.
- `crates/companions/src/credentials.rs` and `config_files.rs` (CMP-3): generated, sealed credentials and the config writers.
- `crates/companions/src/detect/*` (CMP-8a): `CompanionHttp`, the probe pattern, the fixtures layout under `crates/companions/fixtures/`, and `docs/decisions/2026-11-08-CMP-8a.md`.
- `crates/integrations/src/clients/qbittorrent.rs`: how Lodestar already logs in to qBittorrent (`auth/login`, the session cookie taken from the response).
- **CMP-4b's route vocabulary:** read `crates/companions/src/supervise/egress.rs` on PR #608 (`slice/CMP-4b`). It's still in review, so **don't depend on it**.
- `runner/standards.md` is the bar.

## Owns
- `crates/companions/src/configure/mod.rs`
- `crates/companions/src/configure/qbittorrent.rs`
- `crates/companions/src/configure/prowlarr.rs`
- `crates/companions/tests/configure.rs`
- `crates/companions/fixtures/qbittorrent/configure/**` and `crates/companions/fixtures/prowlarr/configure/**`

**Shared edits the runner allows:**
- one `mod configure;` line (and its doc bullet) in `crates/companions/src/lib.rs`;
- **`adapter.rs`, additive only (runner-approved under §2.33):**
  - new fields on `ConfigureInput` and its constructor or builder, for what configuration needs (the route setting, save paths, the install's origin and confirmation). Keep each field a closed or validated type;
  - **qBittorrent's session cookie.** `ProbeResponse` carries no headers. You may add one narrow, additive way to get a named session cookie from a response: for example, a request option naming the cookie, with its value returned as a `Secret`. **Never expose all headers or free text.**
  - CMP-8a's adapters and tests must keep passing unchanged.

  Anything else in `adapter.rs` goes to the runner first.
- `crates/companions/src/detect/mod.rs`: only what `CompanionHttp` needs to support that cookie option, or to send the `Cookie` header on later requests. No change to detection's scope, guard mode or limits.
- **New events** in `crates/companions/src/log_events.rs`, with their registrations: the `crates/lodestar` logging catalogue and golden, `event-names.ts`, and the en/zh messages, as CMP-0a did. Events carry the companion id, the setting key's name and codes, **never a value, URL, body or credential**. If the web size budget moves, say so in the PR.
- `crates/companions/Cargo.toml` and `Cargo.lock`, for crates already in the lockfile only.
- One `docs/decisions/2026-11-08-CMP-8b.md`.

**Not yours:**
- implementing `ConfigSink` in `crates/lodestar`, and the guided endpoints (CMP-5g's);
- the install state machine that calls you (CMP-5's);
- the route gate (CMP-4b's);
- FlareSolverr (CMP-12's);
- the catalogue (CMP-0's and CMP-13's).

## What to build
1. **`Configure` for qBittorrent and Prowlarr**, using CMP-0's traits.
   - `plan` lists exactly the settings that differ from the wanted state, without changing anything.
   - `apply_plan` changes only what the plan lists.
   - Each change is idempotent: applying a plan, then planning again, gives an empty plan.
   - Each change is logged by setting name, never by value.
2. **qBittorrent** (WebAPI v2; check the endpoints and preference keys against upstream for the version the catalogue pins, and record the sources):
   - **login:** with the stored credential, through the new session-cookie option;
   - **WebUI bound** to localhost or the internal network;
   - **categories and the save path;**
   - **the network interface or bind address** per the egress route;
   - **guided installs (Q19):** start from the temporary password the user pasted, then replace it with CMP-3's generated password. Never leave the temporary one in place after success.
3. **Prowlarr** (API v1, `X-Api-Key` sent as a sensitive header): its SOCKS5 proxy per the route. Check the endpoint and fields upstream, and record them.
4. **The route input.** Use a closed type in `ConfigureInput`, for example `Route::{Direct, Interface(name), BindAddress(ip), Socks5{..}}`, so you don't depend on CMP-4b. CMP-5 maps CMP-4b's route onto it. A route kind the companion can't follow returns a typed refusal, never a silent direct route (Q8).
5. **`ConfigSink`** (in `configure/mod.rs`): the trait `crates/lodestar` implements in CMP-5g.
   - It carries Lodestar's own writes: the Prowlarr plugin's address and sealed key, and a qBittorrent entry in `integrations.clients`.
   - It takes closed types (`Target`, `Secret`, ids), never raw strings from a response.
   - `crates/companions` depends on neither `crates/pipeline` nor the settings code. Check with `cargo tree`.
6. **Consent.** It never configures a companion Lodestar didn't install without the user's confirmation. Make that a required input for detected or guided targets, so it can't be forgotten.
7. **Untrusted input.** Responses are bounded and parsed defensively (CMP-8a's `Limits` and bounded JSON).
   - No `unwrap` or `expect` outside tests.
   - Errors are typed codes, with no URL, body or credential.
   - Credentials go only in sensitive headers or request bodies, never in a URL or query string.

## Tests and gates
- **Fixtures and fake servers** (CMP-8a's pattern, local test servers) for each adapter:
  - first configuration;
  - already configured (an empty plan, no writes);
  - partly configured (only the differing keys are written);
  - auth refused;
  - the temporary password replaced (guided);
  - a route kind the companion can't follow;
  - hostile responses: oversized, malformed, slow, a wrong content type.
- **Behaviour:**
  - `apply` refuses another companion's plan;
  - no credential appears in any error, log line or request URL (assert on what the fake server saw);
  - an unconfirmed external target is refused before anything is sent.
- **A property test** for each response parser.
- **No fixed sleeps.** Waits are conditions with ≥ 10 s deadlines. Tests run on Windows CI.
- **Before pushing:**
  - `cargo fmt --check`;
  - `cargo clippy --workspace --all-targets -- -D warnings`;
  - `cargo test -p lodestar-companions`, plus `-p lodestar` if you touch its logging catalogue.
- **Size:** about 700 lines per the plan; stay under 1,500, or state why.

## Decisions you may make (§2.33; record each in the decisions file)
- the endpoints and keys each adapter uses, with upstream sources;
- the shapes of `Route`, `ConfigSink` and the new `ConfigureInput` fields;
- the session-cookie option's shape, within the limits above;
- the fixture layout;
- which settings count as "wanted state" for each companion.

**These go to the runner:** anything that sends a credential somewhere other than the companion's own API on its target, configures a companion without consent, widens detection's scope or the SSRF guard, changes `adapter.rs` beyond the additive items above, or starts or wires FlareSolverr.

**Carried from #610's security review (for CMP-5g, not you):** container-name targets are trusted only under Docker's embedded DNS. Your consent input is what stops credentials going to an unconfirmed detected target, so make it impossible to skip.

When CI is green, Windows included, message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA, using the Claude Code Remote MCP `send_message` tool.
