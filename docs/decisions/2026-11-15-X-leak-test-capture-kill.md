# X-leak-test-capture-kill decisions

The runner's slice, 2026-11-15: three robustness fixes to the gluetun leak test, so that its failures always mean a leak, never a busy host. All three are in the test harness, none in Lodestar. This PR is part 1, the CI flake. Parts 2 and 3 follow in a second PR (runner's call: part 1 reds unrelated PRs, so it goes first). Calls not marked as the runner's are this PR's, and the runner flags them to the owner (`docs/owner-decisions.md` §2.19).

## 1. The CI flake: the test's capture stub, not the script
- **The symptom:** under CPU load, `infra/fleet/tests/test_steps.py::VMScripts::test_the_vpn_interface_comes_from_gluetuns_config` failed `stop_window_exercised` and then `leak_test`. The runner reproduced it on `main` (8b596aa9), and it came in with #881. Here it failed 2 runs in 20 with four busy loops on four cores.
- **The cause is in the test's `tcpdump` stub:**
  - The stub wrote its capture file first and set its TERM trap second.
  - The file existing is what `leak-test.sh` waits for before it treats the capture as running (`start_inbound_capture`).
  - Under test, the observation window is near-instant, so the script's TERM could arrive between the two steps.
  - The default TERM action then ended the stub before its trap wrote the fixture's packets. The capture counted 0 packets, and the window looked idle.
- **The fix:** both capture branches of the stub (inbound and host ICMP) set their trap before they write the file. A real `tcpdump` does the same: it sets its signal handling before it writes the capture file.
- **The script is unchanged.** On a real host it judges a capture only after it has stopped it and waited for it to exit. `tcpdump -U` writes each packet as it arrives, and it flushes on TERM. So no packet the window drew can be missing from the count.

## Tests
- **The flaky test under load:** 20 passes out of 20 with four busy loops on four cores. Unchanged `main` failed 2 out of 20 under the same load.
- **The rest:** `./infra/fleet/run_tests.sh` passes all three suites (373, 45 and 290 tests). That includes `test_an_idle_stop_window_proves_nothing_and_fails` and the other leak-test negative tests, which still fail a window with no endpoint packets.
