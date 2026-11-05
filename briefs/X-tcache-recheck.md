# X-tcache-recheck: re-check a lookup right before it's sent, and stop a source the moment it's turned off
**Model:** Opus, with `/security-review` (content safety, consent). The runner reviews the result with a fresh Opus reviewer. Runner brief, 2026-11-05T23:55Z, from the runner's security pre-audit of main at 63a2c6df. Both changes tighten behaviour, so they are the runner's call; they are flagged to the owner under §2.19.

## Findings
**M1. A queued torrent-cache lookup is sent without re-checking holds or the floor list.**
- **Where:** `crates/lodestar/src/torrent_cache.rs:157` and `:186` check keys once, when they're queued. `crates/sources-plugins/src/torrent_cache.rs` `run_inner` (around lines 945-975) pops a key and sends it, with no store or engine access. In `crates/pipeline/src/torrent_cache.rs`, `Cursor::next` (lines 67-104) and `interest()` (lines 284-319) check neither `held_among` nor `engine.blocked_hash`.
- **Scenario:**
  1. An unverified key K is queued in a lane (each lane holds up to 256 keys).
  2. K then joins the floor `cs_hashes`, a dropping list, or a tombstone.
  3. Up to about 4 h later, the queued lookup still sends `GET …/K.torrent` to a third party.
- **Contrast:** ENR-1's `adult_enrich::check` re-reads every never-condition just before each send. Do the same here.

**M2. Turning a source off, or removing an Account credential, doesn't stop its running plugin until a restart.**
- **Where:** `crates/pipeline/src/control.rs:1193` only writes the new state on disable, and `:1302-1306` sets Disabled on credential removal without stopping anything. `begin_purge` (from line 1328) does stop the plugin.
- **Scenario:** a user turns the torrent cache off. Lookups keep going out until the next restart.
- **Supersedes:** "applies at the next start" in the SRC-11c, SRC-13b and SRC-14a decisions files. Say so in your decisions file.

## Do
1. **M1:** right before each `TorrentCache::request`, call a host-supplied predicate. It refuses the send if the key is held, on the floor or any blocking list, hidden, private, or its source is disabled. Use one reader read per lookup, with no store transaction held across the send. Re-check the same conditions in `Cursor::next` and `interest()`.
   - Test: a key floored after it's queued is never sent. Assert it at the transport.
2. **M2:** on any transition to Disabled (the toggle, credential removal, consent no longer met), stop the plugin the way `begin_purge` does, without the purge. Prefer also having `PluginEgress` refuse sends while the store says the source is off, as defence in depth.
   - Tests: disable while running means no further sends, waited for with a deadline of 10 s or more. Re-enabling starts it again. The Prowlarr pattern (a key read once at start) is covered by stopping the plugin.
3. **Hybrid keys (checked as unconfirmed by the audit):** confirm whether an unverified item under one swarm hash of a hybrid torrent can co-exist with a stored torrent under its other hash. If it can, `Cursor::next` must see the stored torrent's BEP 27 private flag. Fix and test it, or state why it can't happen.
4. **Decisions file:** `docs/decisions/2026-11-06-X-tcache-recheck.md`.

## Own
- `crates/lodestar/src/torrent_cache.rs`;
- `crates/sources-plugins/src/torrent_cache.rs` (the send path only);
- `crates/pipeline/src/torrent_cache.rs`;
- `crates/pipeline/src/control.rs` (the disable paths);
- `crates/sources/src/egress.rs`, only the `PluginEgress` source-enabled check. Coordinate with X-sources-low through the runner;
- their tests;
- the decisions file;
- your own `runner/slices.yaml` entry.

## Done when
These pass: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (per crate if the disk is short; say so), and the torrent-cache fuzz target for 10 minutes.

Branch `slice/X-tcache-recheck`. Open the PR titled "X-tcache-recheck: Re-check lookups at send, stop a source when it's turned off". Follow `.claude/skills/steward/SKILL.md`. Never merge. Push once per round. No fixed sleeps. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server.
