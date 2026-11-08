# CMP-8a: Companion detection and health adapters (qBittorrent, Prowlarr, gluetun) on fixed targets and a URL you enter

**Model:** Opus. **Review:** a fresh Opus reviewer plus `/security-review`: it probes private addresses and reads untrusted responses. **Branch:** `slice/CMP-8a`, cut from `origin/main` (CMP-0 through CMP-4a are in, and PK-win-restart at 87d4906f). **PR title:** `CMP-8a: <title>`. **Steward:** follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Read first
- `docs/plan/2026-11-06-v06-companions.md`:
  - §4's **CMP-8a** bullet (line ~164). It's the spec.
  - §3's table row and the Q14 note (moved to Opus).
  - §5 **Q12:** loopback on the catalogue's ports, the catalogue's container names, and a URL you enter. **Never scan the LAN.**
  - §5 **Q19** (guided credentials). It's context for CMP-8b; here, detection never needs a secret to say "found".
- `docs/architecture.md` §12.2 (the SSRF guard), §18.9 and §18.10.
- `crates/companions/src/adapter.rs` (CMP-0): `Target`, `EnteredUrl`, `Limits`, `ProbeRequest`, `ProbeResponse`, `GuardedHttp`, `AdapterError`, `Detected`, `HealthStatus`, and the `Detect` and `Health` traits. **You implement these traits; don't change them.** If one can't express something you need, tell the runner first.
- `crates/companions/src/catalogue.rs` and `data/catalogue.json`: ports, health endpoints, container names.
- `crates/integrations/src/net.rs`: `NetPolicy`, `GuardedClient`, and the download-client policy that allows loopback and private ranges. `crates/integrations/src/clients/qbittorrent.rs` shows how Lodestar already talks to qBittorrent.
- `crates/sources-feeds/src/json.rs`: the bounded JSON reading to reuse.
- **SRC-7's recorded-fixture pattern** (find it with `git log --grep SRC-7` and look under `crates/sources-*/fixtures`).
- `runner/standards.md` is the bar.

## Owns
- `crates/companions/src/detect/mod.rs`
- `crates/companions/src/detect/qbittorrent.rs`
- `crates/companions/src/detect/prowlarr.rs`
- `crates/companions/src/detect/gluetun.rs`
- `crates/companions/fixtures/**`
- `crates/companions/tests/detect.rs`

**Shared edits the runner allows:**
- one `mod` line in `crates/companions/src/lib.rs`;
- `crates/companions/Cargo.toml` and `Cargo.lock`, for crates already in the lockfile only (`lodestar-sources-feeds` for `json.rs` if that adds no cycle; check it). A crate outside the lock goes to the runner first;
- new events in `crates/companions/src/log_events.rs`. Events carry the companion id and codes, never URLs, response bodies or credentials;
- one `docs/decisions/2026-11-08-CMP-8a.md`.

**Not yours:**
- wiring into the binary and the endpoints (CMP-5g's);
- configuration (CMP-8b's);
- FlareSolverr (CMP-12's, which adds `fixtures/flaresolverr/`);
- `adapter.rs` and the catalogue (CMP-0's and CMP-13's).

## What to build
1. **Detection targets, per Q12 only:**
   - loopback (`127.0.0.1` and `::1`) on each catalogue port;
   - each catalogue container name on Docker's network (`qbittorrent`, `prowlarr`, `gluetun`);
   - one URL the user enters (`EnteredUrl`).

   Never a range, a subnet or a port sweep. A test asserts that the probe set built for "auto-detect" is exactly the fixed list.
2. **Every probe goes through the SSRF guard (`GuardedHttp`).** Loopback is allowed only for these fixed targets; a URL the user enters gets the download-client policy (private ranges and loopback; never link-local, metadata or reserved).
   - No redirects, unless the guard re-checks each hop.
   - A timeout and a byte cap (`Limits`) on every probe.
   - Resolve once per connection; rebinding is the guard's job, so don't bypass it.
3. **Adapters** (`Detect` and `Health` for each):
   - **qBittorrent:** the WebUI's unauthenticated version endpoint, or the login page, recognised by a stable marker. Health is the same probe. Never send credentials in detection.
   - **Prowlarr:** the unauthenticated `/ping` or `/initialize.json`-style marker (check the actual upstream behaviour and record it). Health is `/ping`.
   - **gluetun:** its control server (port 8000) public-IP or status endpoint. Health reports whether the VPN is up, without exposing the IP beyond a boolean or closed enum (the IP is personal data: never log it or return it raw).
   - **The answer:** `Detected` and `HealthStatus` carry closed, bounded values only (the version as a parsed, length-capped value, not raw text). An untrusted response can't put free text into anything Lodestar stores or shows.
4. **Untrusted input.** Responses are bounded and parsed defensively: malformed JSON, a huge body, a wrong content type, a slow drip (the timeout covers the whole body), a non-UTF-8 body, a hostile `Server` header. No `unwrap` or `expect` outside tests.
5. **Errors** are typed codes and carry no URL (the guard's `redact` if a log needs one), no body and no credential.

## Tests and gates
- **Recorded fixtures per adapter** (the SRC-7 pattern), for the versions the catalogue pins:
  - the real responses: found, wrong service on the port, auth required, down;
  - each against a local test server. Loopback is fine in tests.
- **Guard and scope:**
  - auto-detect builds only the fixed probe set;
  - an entered URL that points at link-local or metadata (169.254.169.254, fe80::) is refused before connecting;
  - a redirect to a disallowed address is refused.
- **Hostile responses:** oversized, slow, malformed, and a version string with control characters or 10 KB of text.
- **A property or fuzz test** of each response parser (the plan's parser rule).
- **No fixed sleeps.** Waits are conditions with ≥ 10 s deadlines. Tests run on Windows CI too.
- **Before pushing:**
  - `cargo fmt --check`;
  - `cargo clippy --workspace --all-targets -- -D warnings`;
  - `cargo test -p lodestar-companions`.
- **Size:** about 600 lines per the plan; stay under 1,500, or state why.

## Decisions you may make (§2.33; record each in the decisions file)
- the endpoint and marker each adapter uses, with upstream sources;
- the fixture layout;
- the timeouts and caps within `Limits`;
- the closed shapes of the version and health detail.

**These go to the runner:** anything that widens Q12's scope, changes `adapter.rs`, sends a credential during detection, or loosens the SSRF guard.

When CI is green, Windows included, message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA, using the Claude Code Remote MCP `send_message` tool.
