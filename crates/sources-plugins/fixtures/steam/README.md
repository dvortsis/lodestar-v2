# Steam fixtures

Synthetic answers in the shape of `IStoreService/GetAppList/v1`, written by
hand: no real app list, no key. Each directory is one case replayed in
order (`lodestar_sources::testing::Replayer`); the key header is checked by
the tests, never stored here.

- `steam_list`: two pages; the second starts after the first's `last_appid`.
- `steam_bad_body`: a page cut short.

The failure-path tests in `tests/steam.rs` build their synthetic pages in
code (`page` and `body`), each replayed in order the same way.
