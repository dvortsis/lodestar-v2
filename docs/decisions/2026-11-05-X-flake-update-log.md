# X-flake-update-log decisions

- **The update test reads only `update.*` events.** `the_log_over_an_install_names_events_and_never_urls` read every line with `event=` from the test binary's one global capture (`test_log`), so a parallel test's line (RP-4 saw `address.confirm_needed`, which carries an address) could trip its "no URL, host or address" check. The check now covers lines with `event="update.`, which is every event the update engine logs (`update/log_events.rs`), and asserts there is at least one. The credential scan over the whole capture, and the exactly-one `redirect_refused` check, are unchanged.
- **The shared capture stays global.** `test_log.rs` records why: a thread-scoped subscriber races other threads over cached callsite interest and can miss events, and the install runs on spawned tasks. No `#[serial]` or mutex was added.
- Test code only; no owner decision is touched.
