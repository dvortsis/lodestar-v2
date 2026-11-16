# X-release-dead-code decisions

The release build (`--no-default-features --features web`, as `release.yml` builds it) warned about four unused items. Each is now compiled only where something uses it. Nothing is removed, and there's no blanket `allow(dead_code)`.

- **`main.rs` `serve`:** only `serve --fixture` calls it (`fixture.rs`), so it's `#[cfg(feature = "fixture")]`. Its doc comment said it was also for Windows; the Windows service has its own `serve` (`windows/service.rs`), so that part was stale and is corrected.
- **`cli::check_fixture`:** used by `fixture.rs` and the CLI tests, so it's `#[cfg(any(test, feature = "fixture"))]`.
- **`system::Restart::refuse`:** used by a temporary fixture, a paused Windows service and the tests, so it's `#[cfg(any(test, windows, feature = "fixture"))]`. The `refused` field it sets stays, because `available()` reads it on every build.
- **`update::testing::ENV`:** read only by the debug build's override loader, which is already `#[cfg(debug_assertions)]`, so the constant is too. The module's promise that none of the test overrides reach a release build now covers the name as well.
- **Checked on:** Linux and Windows (`x86_64-pc-windows-gnu`), each with the release features and the default features. None of them warns.
