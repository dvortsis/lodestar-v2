# X-companions-mount-check

Runner brief, 2026-11-13. Opus, core: privdrop and companions. Cloud testing first (§2.62). Run `/security-review` before ready. Follow-up to #825 (X-gluetun-templates-hostfile, merged 426a4159).

## Why
#825's decisions file records the gap. privdrop judges `/companions` from inside the container only, so it can't see that a host folder mounted at `/companions` sits inside the host data folder, or inside another container's folder. Close that gap, plus the review's smaller items.

## Must have
1. **Mount comparison.** In privdrop, read `/proc/self/mountinfo`. Find the mounts for `/companions` and `/data`, and for any known companion folder mounts. Refuse `/companions`, writing nothing, when its source is on the same device as one of them and either root path contains the other (nested either way, or equal).
   - Parse mountinfo strictly: octal escapes (`\040` and so on), optional fields up to `-`, a bounded line count and length.
   - A parse failure or a missing entry means: write nothing, and log why. Never assume safe.
   - A bind mount of a subfolder shows as a root path under the same device. Test that case.
2. **Known companion folders count as taken.** Pass the companion folders Lodestar knows into the same check (the settings and config folders from `companions.rs` and the templates), so `/companions` inside any of them is refused too.
3. **Natively, open parent components with `openat2` and `RESOLVE_NO_SYMLINKS`** where the kernel has it (ENOSYS falls back to today's checks). Keep the existing link refusals.
4. **Make rv-825's conditional container test assertion unconditional.** If it depends on running as root, use a test path that doesn't, or split the case. Never skip or `#[ignore]` it.
5. **Guide**, `docs/user/03-keep-lodestar-safe.md`:
   - say Lodestar now refuses a nested folder, and what the user sees;
   - change the `rm -r …/post-rules.txt` line to `rmdir` (Docker leaves an empty folder; `rmdir` fails safely if it isn't empty).
6. **Log events and refusal codes:** reuse `system.companions_dir_left` with a new reason (for example `nested`). If you add a reason or code, list it in the PR body so the runner can pass it to X-gluetun-hostfile-ui. A new event name needs the web event-names file, which you don't own, so ask first.
7. **Decisions file** `docs/decisions/2026-11-13-X-companions-mount-check.md`: what's detected now, what still isn't (for example a host path reached through a symlink on the host), and the fallback. The runner flags it to the owner (§2.19).

## Owns
`crates/lodestar/src/privdrop.rs`, `crates/lodestar/src/companions.rs`, `docs/user/03-keep-lodestar-safe.md`, `docs/decisions/*-X-companions-mount-check.md`.

## Tests
- Unit tests over mountinfo fixtures: nested, equal, sibling, a different device, escaped spaces, a bind of a subfolder, malformed input, and an over-long line.
- No fixed sleeps. Waits on conditions with deadlines of at least 10 s.

## Process
Branch `slice/X-companions-mount-check`; PR title `X-companions-mount-check: Companion files: refuse a host companions folder nested inside the data folder or another companion's folder`. Follow `.claude/skills/steward/SKILL.md`. Run `cargo fmt --check`, clippy with `-D warnings`, and `cargo test -p lodestar` for the touched modules. Disk is tight: use `CARGO_INCREMENTAL=0`. When CI is green, message the runner `ready <PR> <full sha>`. No cloud or AWS calls.
