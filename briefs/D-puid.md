# D-puid: PUID/PGID privilege drop in the binary
**Model:** Opus (security: process privileges, start-up). **Branch:** `slice/D-puid`.

**Spec:**
- `runner/standards.md` (§5.7 means this slice also gets `/security-review`);
- architecture §11 (Docker supports `PUID`/`PGID`/`TZ`; distroless has no shell, so the binary handles PUID/PGID) and §12;
- §16, the D-docker entries (the image runs as `nonroot` 65532 today; `/data` is owned by 65532);
- `packaging/docker/{Dockerfile,README.md}`, `packaging/unraid/lodestar.xml` (it currently sets `--user 99:100` as a stopgap, plus PUID=99/PGID=100);
- `crates/lodestar/src/main.rs` and `services.rs`.

## Own
- A new `crates/lodestar/src/privdrop.rs`, plus the minimal call in `main.rs`, which runs before any file under the data dir is opened, any socket is bound, or any thread is spawned.
- `packaging/docker/Dockerfile` and `packaging/docker/README.md`.
- `packaging/unraid/lodestar.xml`: remove the `--user 99:100` stopgap and fix the PUID/PGID descriptions, once the binary handles them.
- `crates/lodestar/Cargo.toml`: a dependency only if `std` and the existing `libc` aren't enough. Prefer `libc`, if it's already in the lock; say so.
- A §16 entry.

## Build
- **On Unix, when started as root (euid 0):**
  - Read `PUID`/`PGID`. Defaults are 65532/65532. Validate them: numeric, no 0 unless `LODESTAR_ALLOW_ROOT=1`.
  - `chown` the data dir (not recursively over a large existing tree unless ownership is wrong; check the top-level owner and fix recursively only when it differs, and log what was done).
  - Then `setgroups([])`, `setgid` and `setuid`, in that order. Verify the drop is irreversible: `setuid(0)` must fail afterwards. Abort start-up if any step fails.
- **When not root:** ignore PUID/PGID. If they're set and differ from the current ids, log one line explaining.
- **Docker image:** run as root by default so the drop can happen. Keep a safe default: no PUID gives 65532. Document that `--user` still works and skips the drop.
- **Windows:** a no-op. Keep it `cfg(unix)`.
- **Also:**
  - `healthcheck` must not need root;
  - `TZ` is untouched (Docker sets it);
  - nothing runs as root after the drop, including spawned threads; that's why it happens first.

## Tests
- Unit-test the parsing and validation.
- An integration test that runs only as root in CI (skip otherwise, with a clear message): start the binary with PUID/PGID and check the process ids, the data dir ownership, and that `setuid(0)` fails.
- A CI step in the Docker smoke job: run the image with `PUID=99 PGID=100`, then check `/data` ownership and the process user via `/proc/1/status`, without a shell. Read it with the binary's own healthcheck or a tiny helper, or say how.

## Done when
- After merging `origin/main`, these pass: fmt, clippy -D warnings, `cargo test --workspace`, and the Docker smoke job in CI.
- The PR has a threat table: running as root, a symlinked data dir, a hostile PUID, and a failed drop.
