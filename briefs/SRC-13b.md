# SRC-13b: wire the torrent-cache plugin into the host (consent covers the configured hosts, Queue feeds, writer entry)
**Model:** Opus (consent, egress and writer concurrency). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-05T06:05Z.

## Why
SRC-13 (#458) built the torrent-cache fetch and judge modules, but nothing stores what a cache returns, and the plugin stays unregistered until its External consent covers the configured hosts. This slice wires it in.

## The spec
`docs/decisions/2026-11-05-SRC-13.md`, the section "What the wiring slice (SRC-13b, runner) must do", steps 1–6. Follow it exactly. Also read `docs/decisions/2026-11-05-X-source-adult.md`; #467 has merged.

## Do (branch `slice/SRC-13b` from `origin/main`)
1. **Consent covers the configured hosts.** Do this before registering: adding or changing a cache host must ask again. Either let a plugin's consent endpoints come from its config, or bind the acknowledgement's version to the config's host list. Test both cases: a new host asks again; an unchanged list doesn't. Then build the plugin from its saved config, register it, and give its egress `TorrentCache::endpoints()`. While that's `None`, nothing is reachable.
2. **Feed the Queue.**
   - From the pipeline's `Cursor`: `Why::Unverified`.
   - From user interest: `Why::Interest`. `private_source` comes **from the store, never the caller**, and a key the store doesn't know isn't looked up.
   - An end-to-end test: an interest lookup for a private torrent or item never reaches the plugin's fetch.
3. **Judge through the always-adult mask.** Each `Verified` goes to `torrent_cache::judge`. That module today calls `judge_classified` with no mask (`pipeline/src/torrent_cache.rs:196`). The #467 review requires it to go through `Writer::judge` / `source_adult`, so an always-adult source's item stays Adult. Test it.
4. **A lease-free writer entry** beside the pool's `Results`:
   - `Judged::Store(Admitted)` commits as verified metadata, with provenance `torrent_cache::PROVENANCE` plus the plugin's source bit, and with `Verified::claims()` and `Verified::provenance()` as that source's claims;
   - it promotes an unverified item holding the key, keeping its id (§18.5);
   - it re-judges if `Admitted::version` is behind the engine;
   - `Judged::Held` goes to the hold path; `Judged::Refused` is counted and dropped.
5. **Close the crash window from the #467 review while you're in `writer.rs`.** The commit writes the always-adult source bit in one transaction, and `adult_by_source` reclassifies in a second. A crash between the two leaves the item non-adult with no retry mark. Set the retry mark (`parser_ver = 0`, or whatever the store uses) for those ids inside the commit's own transaction, and test it with a simulated failure between the two.
6. **Move the blocking SQLite reads** in `Writer::judge` / `source_adult` into `spawn_blocking` (the #467 review's nit), with no behaviour change.
7. **Show the plugin's `Stats` on the status page.**
8. **Confirm SRC-13's politeness numbers or propose changes:** one lookup a minute per cache, one request every 10 s per host, back-off from 1 min to 6 h, 256-lookup lanes. They're §2.26 numbers; the runner flags them to the owner.

## Own
- `crates/pipeline/src/writer.rs`, `crates/pipeline/src/torrent_cache.rs` and its tests;
- the host wiring in `crates/lodestar` (services or plugin registration), plus the status page entry;
- the consent and endpoint change in `crates/sources` (SRC-4's consent digest) or `crates/sources-plugins/src/torrent_cache.rs`, whichever you choose (say which);
- the store-crate change needed for step 5;
- `docs/decisions/2026-11-05-SRC-13b.md`.

**Don't touch** `crates/pipeline/src/{worker.rs,unverified.rs,enrich.rs}`: ENR-1 owns them and runs alongside you. If you need `prepare_one` from `worker.rs`, keep `judge`'s duplicate, as SRC-13 did, and say so. You and ENR-1 both add lines to `pipeline/src/lib.rs`; whichever merges later brings `main` in.

## Done when
These pass: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`. If disk doesn't allow the workspace run, test per crate and say so. Then run `/security-review`.

Open the PR against `main`, titled "SRC-13b: Torrent-cache host wiring: consent covers configured hosts, Queue feeds, lease-free writer entry". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round. No fixed sleeps.
