# X-egress-state-watch decisions

Runner brief, 2026-11-06T04:10Z, from the fresh review of #525 (X-tcache-recheck). An availability fix that keeps #525's fail-closed rule, so it's the runner's call, flagged to the owner under `docs/owner-decisions.md` §2.19. It refines the "Defence in depth in the egress" bullet of `2026-11-06-X-tcache-recheck.md`: the check no longer takes the shared store's lock.

## The source-on check reads a connection of its own (design a)
- **What changed.** `SourceCredentials::source_enabled` used to read the source's row under the shared `Mutex<Store>` that the API and Control also hold. A long hold of that lock (a large import step, a purge, a slow disk) past `STATE_WAIT` (2 s) refused every HTTP plugin request for that time: fail-closed, but lost availability and noise in `policy_refusals`. The host now opens a read-only connection (`Store::open_reader`) beside the writer when it builds the egress factory, and hands it to `SourceCredentials::reading_states_from`. Only the egress check uses it. Credentials (`socks5`, `get`) are still read from the shared store: they're read once, when a plugin's client is built.
- **Why it can't go stale.** The reader is a plain SQLite connection on the same WAL database, and `sources()` is one autocommit `SELECT`: every read starts a fresh snapshot, and no transaction is held between reads. A state any writer commits (the toggle, a credential lapse, a purge, the startup disables in `crates/pipeline/src/lib.rs`) is therefore seen by the next request. There's no second copy of the states, so there's nothing to publish and no writer to miss. That's why design (b), an in-memory watch, wasn't needed.
- **What it can't see: a write not yet committed.** A request whose check runs while a disable is mid-transaction reads the old state and may go out. That request is concurrent with the toggle, which hasn't taken effect until it commits, so it orders before the toggle. Under the shared lock the same request waited for the commit (or for `STATE_WAIT`). #525's disable commits the state first and only then stops the plugin, so by the time the API call returns the reader already answers "off".
- **Fail-closed is unchanged.** `source_on` and `STATE_WAIT` are untouched. No answer within 2 s, a read error (including a poisoned reader lock or a failed blocking task) and a missing row all refuse with `HttpError::NoEgress`.
- **Its cost.** One more read-only connection, opened only when plugins get egress (never under a fixture). The reader's own lock is held only for one small query, so concurrent plugin checks queue behind each other for microseconds, not behind the API.
- **The constructors keep their old default.** `SourceCredentials::new` and `with_keys` read states from the shared store until `reading_states_from` is called, so `Ops`'s instance (which never checks source states) and the pipeline tests are unchanged.

## API doc fix
- **`restart_required` and `PATCH /sources/{id}`** said turning a source off takes effect at the next restart. Since #525 a stop is immediate. The docs now say so, and `restart_required` stays true for a source turned off only until it has stopped (as `crates/lodestar/src/ops.rs` computes it). `openapi.json` and `schema.d.ts` are regenerated with `cargo run -p lodestar --bin openapi` and `npm run api:types`.

## Tests
- `crates/sources/src/egress_tests.rs`, over a real database file and the host's wiring:
  - with the shared store's lock held past `STATE_WAIT` by another thread, an enabled source's requests still go out, while the old wiring (the check on the shared lock) refuses;
  - each state a writer commits (Disabled, Paused, Purged, then Enabled again) is read by the very next request, refused at the transport with nothing connecting;
  - a plugin with no row, and a states reader that can't be read, refuse.
- The #525 egress test (`a_source_switched_off_sends_nothing`) and `consent::tests::source_enabled_reads_the_plugins_own_row` are unchanged and pass.
