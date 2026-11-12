# X-prowlarr-register-followups decisions

- **The test that backs "never downloads a `.torrent` file from any indexer"** (Legal Notice section 2) is `crates/sources-plugins/tests/prowlarr_no_fetch.rs`. It has two parts.
  - **Behavioural.** A fake Prowlarr and a trap listener run on 127.0.0.1, both on `std::net` threads; the crate's tokio has no `net` feature, and this slice adds no dependency.
    - Every URL in the recorded indexer list and RSS pages is pointed at the trap: `link`, `enclosure`, `guid`, `comments` and the indexers' own addresses.
    - The test's `Fetch` sends each request the plugin makes over a real connection to the host and port in its URL, and it refuses anything that isn't loopback.
    - After one full cycle the trap must have had no connection, and every URL asked for must be at the manager's address.
    - The wait ends early on a trap hit, so a regression fails by name, not by its 10 s deadline.
  - **Source text.** In `src/prowlarr.rs` outside comments, three things must hold:
    - every `Request::` is `Request::get(format!("{base}/…`, and there are exactly four (the list and a page, for each manager);
    - `base` is only ever the configured address;
    - `self.http.` sends only `list_request()` and `page_request(ix)`.
  - **Checked against a mutant.** A temporary edit that fetched the first item's `<link>` failed both tests.
- **No seam in `src/prowlarr.rs`:** the test drives the plugin through `FakeCtx::with_fetch`, as the plugin's other tests do.
