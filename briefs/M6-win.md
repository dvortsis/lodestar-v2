# M6-win: Windows runtime
Covers the service entrypoint, below-normal priority, the ephemeral-port warning, mmap delete-retry and SO_LINGER(0).

**Model:** Opus (platform runtime, concurrency). **Branch:** `slice/M6-win`.

**Spec:**
- `docs/build-plan.md` P3 [M] Windows bullet;
- `docs/architecture.md` §11 (Windows), §6 (sockets and fetch), §8 (index files on Windows: rename retries), §12.2 (the Windows update helper will later run as part of this service; don't preclude it);
- `docs/decisions/` and §16, for existing Windows notes (e.g. the index directory rename retry).

## Own
- `crates/lodestar/src/windows/**` (new, `#[cfg(windows)]`).
- **A minimal hook in `crates/lodestar/src/main.rs`.** M6-cli is reworking argument parsing there in parallel. Keep your change to one call (e.g. `if windows::service::launched() { ... }`), and merge main if M6-cli lands first.
- `windows-service` (and `windows-sys` if needed) as `[target.'cfg(windows)'.dependencies]`. These dependencies are approved.
- **Narrow, justified edits:**
  - for SO_LINGER(0): the fetch connection code in `crates/peer-wire` or `crates/pipeline`, wherever outbound TCP sockets are created;
  - for the ephemeral-port warning and mmap delete-retry: a startup check, plus the index file deletion path in `crates/search`, but only if it isn't already covered (B5b added rename retries). If `crates/search` needs it and B6 is still open, wait for B6 to merge.

## Build
1. **Service entrypoint:** run as a Windows service (start, stop and shutdown map to graceful shutdown), logging to the configured log file, with data in ProgramData by default. `lodestar service install|uninstall` is optional; the MSI (P-release) registers the service, so coordinate the service name in your PR.
2. **Below-normal process priority** on Windows by default (configurable).
3. **Ephemeral-port warning:** at start, read the dynamic port range. If it's narrow relative to the configured fetch intensity, log the warning and the fix (`netsh int ipv4 set dynamicport`), plainly.
4. **mmap delete-retry:** Windows can't delete files that are mapped. Retry with backoff, and defer to the next start where needed. Check what B5b already does, and don't duplicate it.
5. **SO_LINGER(0)** on outbound peer TCP sockets, so thousands of short fetches don't exhaust ports in TIME_WAIT. Gate it to Windows unless measurements justify more, and say why.

**Tests:** `#[cfg(windows)]` unit and integration tests run on CI's `windows-latest`, plus a no-op compile path on other OSes.

## Common rules
- **Bar and precedence:** `runner/standards.md` is the bar. Read CLAUDE.md, `docs/owner-decisions.md`, and the architecture sections cited.
- **Decisions** go in a new `docs/decisions/<YYYY-MM-DD>-<slice-id>.md` (see `docs/decisions/README.md`), not in §16.
- **Before pushing,** merge `origin/main`; never rebase. Run every check CLAUDE.md lists for the files you touch.
- **Scope:** stay inside **Own**. Shared-file edits needed elsewhere go in the PR description.
- **PRs** stay at or under about 1,500 changed lines, or state the reason in the description.

## Done when
- Workspace checks pass on Linux, and the Windows CI job is green.
- The PR is titled "M6-win: Windows service, priority, port and socket handling".
