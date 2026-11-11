# PK-exc (Opus, core): Insights exceptions for a risky data filesystem and Windows ephemeral-port pressure

Assigned by the runner on 2026-11-11. Deps met (`ext:v051-soak-verdict` holds).

## Read first
- The `PK-exc` entry in `runner/slices.yaml` (its notes are part of this brief).
- `docs/plan/2026-11-06-m6-gap-audit.md`: §2.2 row "Insights exception FUSE/network path" (~line 40), §2.3 "Ephemeral-port check" (~line 51), §3 (store and contract impact), §4.2 shared files (~line 181) and §4.3 "PK-exc" (~line 188). That paragraph is the spec.
- `docs/architecture.md` §10.9.2 (Insights exceptions) and §11 (Windows; filesystems).
- `crates/lodestar/src/fsdetect.rs`, `crates/lodestar/src/windows/ports.rs`, `crates/api/src/insights.rs`, `crates/api/src/dto/insights.rs` (`InsightsException.kind`), `crates/api/src/insights/stages.rs:~509`, and `docs/decisions/2026-11-07-WK-9c.md:43` (a helper line in `crates/api/tests/insights.rs`).

## Owns
`crates/api/src/insights.rs`, `crates/api/src/dto/insights.rs`, `crates/api/tests/insights.rs`, `crates/lodestar/src/insights.rs`, `crates/lodestar/src/fsdetect.rs`, `crates/lodestar/src/windows/ports.rs`, and `docs/decisions/2026-11-11-PK-exc.md`.
Regenerate `web/src/lib/api/openapi.json` and `schema.d.ts` with the repo's tooling (never by hand); that is an approved shared edit. If another open PR has also changed them, the later one regenerates after bringing main in.
Not yours: `crates/lodestar/src/windows/mod.rs` (don't edit it), `stages.rs`'s `ephemeral_port_pressure` KPI (it stays `None`), any web screen (PK-ui renders the new kinds later).

## Build
- **`data_filesystem`:** raised when `fsdetect::detect(data_dir).is_risky()`. The fix says to move the data folder to a pool path, with a link to the guide.
- **`ephemeral_ports`, on Windows:** raised when `ports.rs`'s threshold trips, with the `netsh int ipv4 set dynamicport` remedy (§11). The port result comes from a plain function in `ports.rs`, testable on every OS.
- **Both are re-checked at most hourly,** never per request. The cache must be safe under concurrent Insights requests (no thundering herd, no lock held across I/O).
- **Additive only:** two new `kind` values, no migration, no schema bump. Older clients must still parse the response.
- Wording is plain and for a non-expert (CLAUDE.md "User documentation"), with no raw codes in user text.

## Checks
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (at least `-p lodestar-api -p lodestar`).
- The openapi drift check passes after regeneration.
- **Tests:**
  - Linux tests feed `fsdetect`'s parser a `fuse.shfs` mount table, plus a network filesystem (`nfs4`, `cifs`) and a safe one;
  - the Windows threshold function is tested on every OS, at the boundary;
  - the hourly cache is tested with a fake clock, with no fixed sleeps;
  - both kinds serialise as documented.
- Mutation-check every guard you add.

## Process
- Follow `.claude/skills/steward/SKILL.md`. Branch `slice/PK-exc` from the latest `origin/main`; PR title `PK-exc: <title>`.
- Push once per round of fixes. About 450 lines.
- Anything beyond this brief goes to the runner first.
- When CI is green, message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg): `PK-exc: PR #<n> ready for review, head <sha>`. If blocked: `PK-exc: blocked on <what>`. The runner reviews and merges; you never merge.
