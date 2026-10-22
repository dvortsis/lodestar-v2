# M6-cli: CLI on clap (backup, rollback, vacuum, reindex, stats) and FUSE/network-filesystem detection
**Model:** Opus (data safety, on-disk operations). **Branch:** `slice/M6-cli`.

**Spec:**
- `docs/build-plan.md` P3 [M];
- `docs/architecture.md`:
  - §7 (storage, snapshots);
  - §8 (index rebuild);
  - §11 (deployment: FUSE warnings, e.g. Unraid's `/mnt/user`);
  - §12 (operations), §12.2 (the store snapshots and the rollback journal: don't conflict with B-update's future `<data>/bin` design);
  - §16 and `docs/decisions/`.
- The code:
  - `crates/lodestar/src/main.rs` (current argument handling; D-puid added the privilege drop right after argument parsing, so keep that order);
  - `crates/store` (`VACUUM INTO` snapshots, `schema.rs` migrations and the `.pre-vN.db` snapshots);
  - `crates/search` (`rebuild.rs`, Serving).

## Own
- `crates/lodestar/src/cli/**` (new) and `crates/lodestar/src/main.rs` (argument parsing only, delegating to cli).
- `crates/lodestar/src/fsdetect.rs` (new).
- `crates/lodestar/Cargo.toml`: clap (derive, minimal features). That dependency is approved.
- Tests in `crates/lodestar/tests/cli*.rs`.
- **Don't edit** `crates/store` or `crates/search` internals. Use their public APIs, and if one is missing, name it in the PR.

## Build
1. **The clap CLI.** `lodestar serve` stays the default, so existing Docker, Unraid and systemd invocations keep working, including the env vars.
2. **`lodestar backup [--to PATH]`:** a consistent online backup (`VACUUM INTO` while running is fine) of the store, `config.toml`, `seal.key` and the trust files. It doesn't include the search index (rebuildable): say so in the output. It must work while the server runs.
3. **`lodestar rollback`:** list the `*.pre-vN.db` migration snapshots and restore one, only while the server is stopped (take a lock: refuse if the store is open). Confirm interactively unless `--yes`. Keep a copy of the current DB. Respect B-update's future rollback journal: if `<data>/bin/rollback.json` exists, refuse.
4. **`lodestar vacuum`** (offline `VACUUM` with a size-before/after report) and **`lodestar reindex`** (trigger the background rebuild through the running server's API if it's up; otherwise mark the index for rebuild at next start).
5. **`lodestar stats`:** the counts, sizes and index state, read-only, safe while running.
6. **FUSE and network-filesystem detection** at start for the data dir (`statfs` magic on Linux, Unraid `/mnt/user` shfs, NFS, SMB/CIFS; Windows network drives). Log a clear warning with the safe alternative (§11), and expose it for `/stats` and the wizard (a pub function; wiring is B14's or later).

**Security and data safety:** never overwrite without a copy; refuse on a locked store; paths canonicalised; no shell-outs.

## Common rules
- **Bar and precedence:** `runner/standards.md` is the bar. Read CLAUDE.md, `docs/owner-decisions.md`, and the architecture sections cited.
- **Decisions** go in a new `docs/decisions/<YYYY-MM-DD>-<slice-id>.md` (see `docs/decisions/README.md`), not in §16.
- **Before pushing,** merge `origin/main`; never rebase. Run every check CLAUDE.md lists for the files you touch.
- **Scope:** stay inside **Own**. Shared-file edits needed elsewhere go in the PR description.
- **PRs** stay at or under about 1,500 changed lines, or state the reason in the description.

## Done when
- `cargo fmt --check`, workspace clippy `-D warnings` and `cargo test --workspace` pass after merging main.
- Tests cover each subcommand, including refusal cases.
- The PR is titled "M6-cli: clap CLI with backup, rollback, vacuum, reindex, stats; filesystem detection".
