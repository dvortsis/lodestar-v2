# X-win-clippy decisions

Linux CI never lints `cfg(windows)` code, so Windows-only warnings reached `main`.

- **The `test` job lints on Windows.** A step with `if: runner.os == 'Windows'` runs `cargo clippy --workspace --all-targets -- -D warnings` before `cargo test`, so it shares the build cache. No change to `permissions:`, secrets, triggers, `needs` or the required checks' names.
- **Fixes are lint-only, no behaviour change.**
  - Unused `File` import in `companions/config_files.rs`: the one use is Unix-only, so it is written `fs::File`.
  - `let mut` on `DirBuilder` in four companions files: only the Unix `mode()` call needs it, so `#[cfg_attr(windows, allow(unused_mut))]`.
  - Collapsible `if` in `main.rs` (Windows probation path), rewritten as one `if let` over `and_then`.
  - Two dead-code cases that only show on Windows: `parse_df` in `bitmagnet_check.rs` (no `df` there, tests still use it) and `Server::peers` in `sources/egress_tests.rs` (read only by the Linux bind test). Each gets a narrow `cfg_attr(…, allow(dead_code))`.
- **How it was checked:** `cargo +1.99.0 clippy --workspace --all-targets --target x86_64-pc-windows-gnu -- -D warnings` with `gcc-mingw-w64-x86-64`, clean. CI's `test (windows-latest)` is the final word.
