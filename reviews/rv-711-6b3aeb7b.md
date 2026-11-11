# rv-711 @6b3aeb7b (CMP-11b): MERGE
2026-11-11Z, fresh Opus reviewer, read-only. Blocking: none.

**What I verified**
- **Merge and scope:** `git merge-tree --write-tree origin/main 6b3aeb7b` is clean. All 9 files are in CMP-11b's owns list, plus the decisions file. `companion_wiring_guided_tests.rs` is owned, and its change is the approved one. No Cargo.toml changes, no tokio `test-util`, no `testing` feature on `lodestar`.
- **Registration** (`configure/mod.rs:80`): `qbittorrent::ID if cfg!(windows)` returns the gate; elsewhere it's the ungated adapter, unchanged. Both production configure paths go through `configure::configurator`: the guided plan/finish (`companion_wiring.rs:1192,1221`) and the manager's `Api` (`manager.rs:2223`). The manager answers `guided_only` for the Windows installer, so `Api` never configures it there. `detect::qbittorrent::QBittorrent` is a separate detect/health type. Retries and regenerate re-enter `plan_with`/`finish`, so they pass the gate too. The hand-over writes Lodestar's sink, not qBittorrent. I found no path that skips the version check on Windows.
- **Option D:** `CHECKED_RANGE = ("4.6.0", (5,3))` is the only constant. `supported_for` gives the pin's range when pinned, the checked range for `AwaitingPin`, and `None` (refused with `no_platform`, nothing sent) otherwise. The gate runs after sign-in and before `read_prefs` in the plan, and before any write in the apply. `major_minor` can't panic, and an unreadable major is out of range. The unpinned guide shows no SHA, `Get-FileHash` or `CheckSha256` step (tested).
- **I1 loopback:** a probe of 43 spellings is refused: `127.1`, `0x7f.1`, `0X7F.1`, `0177.0.0.1`, `017700000001`, `2130706433`, `127.0.0.01`, `127.256`, `0`, `00`, `0x`, `0x0`, `0xffffffffff`, `0.0x7f`, `[::1]`, `::1`, `localhost.`, `LocalHost.`, `127.0.0.1.`, `LOCALHOST`, `0--1.IPV6-LITERAL.NET`, `fe80--1s4.ipv6-literal.net`, `@SSL`, `@8080`, NUL, `\\.\`. No panics: `l[2..]` follows an ASCII `0x` check, and `octets[0]` is guarded by `len()==4`. The ones that pass are the as-typed class the doc names: `192.168.1.20`, `nas`, `0abc.lan`, `0x7f-1`, plus DNS names (see Nits).
- **I2:** `path_kind` is keyed on `gate.is_some()`. It runs after `consent` and before `wanted` and `auth_bypassed`, so nothing is sent, in both the plan (`qbittorrent.rs:634`) and the apply (`:670`). The plan side is tested, including `seen.is_empty()`.
- **I3 and nits:**
  - `installable` answers `GuidedOnly` before the pin is looked at (`manager.rs:769-777`).
  - `configurator` returns `None` for any id but qBittorrent.
  - `Windows.old` is refused, and the `Users\Default` and share-name limits are documented.
  - `last_refusal` has its doc line, and `mm` is read once.
- **rv-692 N1/N2:** the three new count tests are real. They use `yield_now` loops under `timeout(WAIT=10 s)` and a permit-storing `notify_one`, with no fixed sleeps. I removed each guard in turn (undo `:1593`, pending `:1785`, live `:1965`), and each removal fails its own test. Removing I3's early check fails `refusals_are_typed_and_change_nothing`. All four mutations were reverted. N2 is documented in the decisions file.
- **Security pass:**
  - No new logging of secrets. Refusals log codes only, and `Unsupported` messages carry only Lodestar-parsed digits-only `Version`s.
  - No new request paths, bodies or user-controlled values in qBittorrent API calls.
  - The guard test only reads source files.
- **Privacy:** no model identifiers outside the harness trailers, no personal data, no 12-digit numbers, no local time zones.
- **Gates run** (own worktree and target):
  - `cargo fmt --check` passes.
  - `cargo clippy -p lodestar-companions -p lodestar --all-targets -- -D warnings` exits 0. The only warning is the existing repo-wide unknown-lint `chunks_exact_to_as_chunks`.
  - `cargo test -p lodestar-companions` passes, with 0 ignored: lib 173, configure 22, manager 48, windows_qbittorrent 21, and the rest.
  - `cargo test -p lodestar --bin lodestar companion` passes 70/70.
- **CI on this head:** one run completed green (`fmt + clippy`, `test (ubuntu-latest)`, `test (windows-latest)`). A second run was still in progress and `ci-ok` was queued when I checked.

**Important (non-blocking)**
- `crates/companions/src/configure/qbittorrent.rs:670`: the apply-side `path_kind` is untested. Deleting it leaves every test green. `check_plan` compares setting keys, not values, so an apply whose input has a wrong-kind save path reaches `setPreferences`. Add an apply case to `a_save_path_of_the_wrong_platform_is_refused` (`tests/windows_qbittorrent.rs:855`): plan with a matching path, apply with the wrong kind, and assert `writes().is_empty()`.

**Nits**
- `crates/companions/tests/manager.rs:59-61`: the old `/// qBittorrent pinned to v on Linux…` doc now sits above `shipped_unpinned` (`:62`), so `catalogue` (`:83`) has lost its doc. Move it back.
- `crates/companions/src/windows_qbittorrent.rs:640`: "What this can't see" could also name public DNS names that resolve to loopback (`127.0.0.1.nip.io`, `localtest.me`). They pass, the same class as this machine's own name. `ip6-localhost` and `localhost.localdomain` pass too, which is harmless on Windows by default.
- `crates/companions/tests/windows_qbittorrent.rs:902`: the guard is line-based. A multi-line `use lodestar_companions::configure::{\n qbittorrent::QBittorrent as Q }` followed by `Q::new` gets past it. This is acceptable under the runner's call, but worth a comment.
- `crates/companions/tests/windows_qbittorrent.rs:564`: the reflowed doc line is just over 100 columns.
