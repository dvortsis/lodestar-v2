# X-claim-trackers: one shared host guard for tracker URLs from claims and caches
**Model:** Opus (untrusted input, egress safety). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-05T13:12Z. It tightens SRC-13's rule, which the runner approved under §2.33 and flags to the owner under §2.19.

## Why
The #491 review (X-fuzz-cache-1105, merged at 4b56fa86) found two latent gaps. Nothing emits or dials claimed trackers today: no magnet link includes them and no endpoint returns them. They become a client-side SSRF into the user's network the moment a feature adds `&tr=` from claims or sends claimed trackers to a client. Close both before that happens:
1. `torrent_cache::public_tracker` (`crates/sources-plugins/src/torrent_cache.rs`) keeps `http://localhost/announce`, `udp://localhost:6969/…` and `http://foo.localhost/`. The reason is that `net::check_url` (`crates/integrations/src/net.rs:~281`) judges IP literals only, and RFC 6761 `localhost` and `*.localhost` resolve to loopback.
2. Feed and plugin claims go through `crates/pipeline/src/unverified.rs` `tracker_url` (~:187). That function checks the text only, so `udp://192.168.1.1:80/announce` or `http://127.0.0.1/` is stored.

## Do (branch `slice/X-claim-trackers` from `origin/main`)
1. **The guard.** Write one shared function, for example `lodestar_integrations::net::public_tracker_url(raw) -> Option<String>`, in its own module or next to `check_url`. It:
   - parses with the url crate and returns the serialised form, which is what gets stored;
   - refuses userinfo;
   - refuses literal private, loopback, link-local, CGNAT, unspecified and multicast addresses, including IPv4-mapped, NAT64 and 6to4 forms (reuse `classify`);
   - re-judges udp's opaque host as `http://{host}/`, as #491 does;
   - refuses `localhost` and `*.localhost`, case-insensitively and after IDNA.

   Accepted schemes are http, https and udp. Port and length caps stay as `tracker_url` has them today.
2. **Use it in both places.**
   - `torrent_cache::public_tracker` delegates to it. #491's canonical storage stays.
   - `unverified.rs` `tracker_url` calls it with a **one-line hunk**. ENR-1b (running) owns `unverified.rs` otherwise, so keep any other edit out of that file.
3. **Existing rows.** Claimed trackers already stored aren't dialed, so no migration is needed. Say so in the decisions file. If a read-side filter is cheap, apply the guard when claims are read for output as well, as defence in depth.
4. **Tests.**
   - A table covering every refused class and the canonical form, including the #491 review's quirk list: backslash, tab and newline, `@`, `%40`, IPv4 shorthand `127.1` and `0x7f.1`, IDNA, `ⓛocalhost`, `[::ffff:127.0.0.1]` and `[::127.0.0.1]`.
   - Property tests: the guard is idempotent; its output never parses to a blocked host; it never panics.
   - Extend the `verified_claims` fuzz target's assertion to re-check kept trackers with `classify`. This was the #491 review's polish item 3.
5. **Correct `docs/decisions/2026-11-05-X-fuzz-cache-1105.md`.** Claim trackers are stored only: they don't go into magnet links and no endpoint returns them. url 2.5.8 leaves `^` unencoded. Then write your own decisions file, `docs/decisions/2026-11-05-X-claim-trackers.md`, and add your `runner/slices.yaml` entry and placement.

## Own
The new guard function and its tests, the delegation in `torrent_cache::public_tracker`, the one-line call in `unverified.rs`, the fuzz target assertion, both decisions files, and your slices.yaml entry.

**Overlaps:**
- ENR-1b owns `unverified.rs`; your hunk there is one line.
- SRC-13c (#493) edits `torrent_cache.rs` for config. Keep to `public_tracker`.

Whichever merges later brings `main` in.

## Done when
`cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`; the fuzz build (`cargo +nightly build --locked --manifest-path fuzz/Cargo.toml --bin verified_claims`). Then run `/security-review`. Open the PR titled "X-claim-trackers: One host guard for tracker URLs from claims and caches", with the label `full-ci`. Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Never merge. Push once per round.
