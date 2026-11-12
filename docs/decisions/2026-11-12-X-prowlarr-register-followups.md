# X-prowlarr-register-followups decisions

- **The test that backs "never downloads a `.torrent` file from any indexer"** (Legal Notice section 2) is `crates/sources-plugins/tests/prowlarr_no_fetch.rs`. It has two parts.
  - **Behavioural, for Prowlarr and for Jackett.** A fake manager and a trap listener run on 127.0.0.1, both on `std::net` threads; the crate's tokio has no `net` feature, and this slice adds no dependency.
    - Every URL in the recorded indexer list and RSS pages is pointed at the trap: `link`, `enclosure`, `guid`, `comments` and the indexers' own addresses.
    - The test's `Fetch` sends each request the plugin makes over a real connection to the port in its URL, always on 127.0.0.1, and refuses any host but `127.0.0.1` and `localhost`.
    - The trap is addressed as `localhost`, so it doesn't share the manager's per-host pacing, and a stray request reaches it at once.
    - Once the plugin asks for the indexer list a second time, the trap must have had no connection, and every URL asked for must be at the manager's address. The second list comes only after the first cycle's pages are all polled and their finds sent, so the first cycle is over (X-prowlarr-no-fetch-wait, which corrects "after one full cycle": the earlier wait stopped at the expected requests and finds, so a fetch after the last emit could be cut off by the stop). The clock is paused, so the 15-minute poll interval between the two lists passes at once.
    - The wait ends early on a trap hit, so a regression fails by name, not by its 10 s deadline.
  - **Source text.** In `src/prowlarr.rs` outside comments, three things must hold:
    - every `Request::` is `Request::get(format!("{base}/…`, and there are exactly four (the list and a page, for each manager); the only `Request {` are the two builders' signatures, so no request is built field by field;
    - `base` is only ever the configured address;
    - every `.http` (any use of the client handle, including one passed to a helper), `.send(`, `.poll(` and `.poll_stream(` is one of four known lines: `http: kit.http,`, the two sends of `list_request()` and `page_request(ix)`, and `Job::poll`'s call (X-prowlarr-no-fetch-wait). A client reached by another name isn't seen, and the behavioural tests cover that;
    - the cut that leaves out unit tests matches any `#[cfg(test)] mod …`, not only `mod tests`; `src/prowlarr.rs` has none today.
  - **The source check is exact on purpose.** A rustfmt reflow or a rename fails it, and its message says what to re-check against the notice. That's the point of a backstop.
  - **Checked against mutants.** Temporary edits that fetch the first item's `<link>` before emitting the finds, after emitting them, from a detached task, or through a helper each failed every test. A helper given `&self.http` that sends an already-built request with `Client::send(c, &r)` fails the source check alone (X-prowlarr-no-fetch-wait).
- **No seam in `src/prowlarr.rs`:** the test drives the plugin through `FakeCtx::with_fetch`, as the plugin's other tests do.
