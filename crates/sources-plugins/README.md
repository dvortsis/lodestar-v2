# lodestar-sources-plugins

Source plugins that poll outside services (architecture §18.1, §18.2). Prowlarr/Jackett (`src/prowlarr.rs`, SRC-7) is the first. This file is the pattern every later plugin here copies (SRC-10a–e, SRC-11–13): how a plugin is built, and how its contract tests run on recorded fixtures with no network.

## A plugin

One module per plugin, `src/<plugin>.rs`, with one `pub mod` line in `src/lib.rs`. It holds:

1. **A manifest and a policy.**
   - The manifest gives the id (never renamed), the kinds, the consent class (§18.7, SRC-4) and the endpoints. A plugin that talks only to an address the user enters lists no endpoints, and offers an `endpoints()` that narrows its egress to that address's origin, scheme, host and port (`Endpoints::user_entered(..).only_origins(..)`).
   - Write the policy as `Policy { caps: …, ..Policy::STRICT }`. Set the caps from §18.1 or the plan's Q4: the poll interval, the per-host rate, the back-off.
   - The config schema marks every secret `"writeOnly": true`.
2. **A config parser** that checks every field and names the rule it broke, never the value. Credentials are never config: they come from the [`Credentials`](src/lib.rs) trait (SRC-4's store), read once at start.
3. **The mapping** from a parser's items (`lodestar_sources_feeds`) to `Found`, as a public pure function, so tests and the fuzz target can call it. Every field is a claim. The key comes only from the parser's hash fields. Sizes and dates outside sane ranges are dropped.
4. **`run`**, which:
   - sends HTTP only through `ctx.kit.http`: each poll's first request through `Client::poll(unit, …)` (usually the feed or indexer request; for a client that logs in each poll, the login), so the poll gate spaces it, and follow-ups through `Client::send`. A plugin whose cycle can fail before that request still waits a poll interval before the next cycle (SRC-11's `clients.rs`); It never builds a client: the host gives it one from SRC-5's `EgressFactory`, with the plugin's route and kill switch;
   - waits on every await inside `tokio::select!` with `shutdown.requested()`, and drops any spawned work on return, so it stops within a second (`STOP_WITHIN`);
   - returns `Err` with a plain message (no URL, key or server text) when it can't go on: not set up, or the key was refused. It doesn't retry those every poll;
   - sets `private_source` on everything found through a private tracker, a Usenet account or the user's own client (§18.7). Such finds carry claims only (Q15): no automatic `.torrent` fetch through a private indexer, no trackers, and no external reference that could hold a passkey. A `.torrent` exported from the user's own client never contacts the tracker, so `clients.rs` reads it automatically, and still emits claims only (runner, SRC-11).

## Fixtures

Each case is a directory under `fixtures/<plugin>/`. Its files are replayed by `lodestar_sources::testing::Replayer`; the module docs in `crates/sources/src/testing.rs` give the full format.

```text
fixtures/prowlarr/prowlarr_rss/
  001.http     GET http://prowlarr.example:9696/api/v1/indexer
               X-Api-Key: REDACTED
               (blank line)
               HTTP 200
               Content-Type: application/json; charset=utf-8
  001.body     the response body, byte for byte
  002.http …
```

- **One case per behaviour:** the normal poll (two cycles, so poll spacing is tested), a refused key, an error on one unit, and each manager or format variant.
- **Recording.** Run the real service once, save each request and response as a `.http`/`.body` pair, then scrub them:
  - every API key, passkey, cookie and token becomes `REDACTED`, in URLs, headers and bodies, and the test's credentials use `REDACTED` too;
  - hostnames become `.example` names (`prowlarr.example`, `public.example`);
  - titles become open-licence ones (Big Buck Bunny, Sintel), and hashes are made up;
  - user names, e-mail addresses and IP addresses are removed.

  Before committing, run `grep -rniE 'apikey=[^R&]|passkey|@|[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\.' fixtures/` and read what it finds.
- **Prowlarr's fixtures are written in its recorded shapes, not captured** (field names and order, `prowlarrindexer` and `jackettindexer` elements, `apikey` links), as SRC-6's were. This session had no manager to record from. Replace them with real recordings, scrubbed as above, when one is available. The tests don't change.

## Contract tests

`tests/<plugin>.rs` runs the plugin on `FakeCtx::with_fetch` with a `Replayer`, on tokio's paused clock (`#[tokio::test(start_paused = true)]`), so a 15-minute poll takes no real time. Every plugin's test file covers these:

| Test | How |
|---|---|
| Discoveries | Wait for the request count (`wait_for(|| replayer.requests().len() >= n)`), then drain the discoveries and check keys, claims, provenance and `private_source`. |
| No extra requests | `replayer.finish()` fails on any request the fixtures don't have, or any fixture never requested. A `.torrent`, a disabled unit or an aggregate endpoint is caught this way. Use `Replayer::load_unordered` when units are polled in parallel. |
| Poll spacing | From `replayer.requests()`: each URL's requests are at least the policy's poll interval apart. |
| Cancellation | End every run with `running.stop_within()`. |
| Refusal and setup | A 401, or the service's own refusal, ends the plugin with an error that holds no secret. A missing address or key fails before any request (run on `NoEgress`). |
| Caps | Manifest, policy limits within the floors, the config schema's `writeOnly` secrets, and the config parser's refusals. |
| Parser bounds | Size, count, depth, DTD and entity tests for every format the plugin parses itself, property tests that it never panics, and a `cargo fuzz` target in `fuzz/fuzz_targets/<plugin>.rs`. |

Two rules apply to all of them:
- **No fixed sleeps.** Wait on a condition with `wait_for` or `FakeCtx::next`, which give up after 10 s of real time.
- **Don't call `FakeCtx::next` while the plugin is between polls.** On a paused clock its waits move time on and fire the next poll, which the fixtures don't have. Wait on the request count and drain instead.

The blocklist test (a blocklisted hash through every plugin) and the private-source isolation tests across Torznab, Stremio, exports and peer finding are SRC-G's, in `tests/safety.rs`.
