# X-mimalloc-docs decisions (follow-ups to X-mimalloc-override)

Runner brief X-mimalloc-docs, 2026-11-05. Follows `2026-11-05-X-mimalloc-override.md` (#448), which made mimalloc the C library's `malloc` in musl builds and listed these follow-ups. `docs/metrics.md`, "Memory attribution", already states the two readings; this slice makes the soak analyzer and the code comments match it.

## The soak analyzer's "other heap"
- **The C allocator comes from the build target, which the collector now records.** Snapshots had no field saying which build ran: `/metrics` carries no build or allocator series, on `main` or on rc.4's code. The shipped binary already reports its target triple as `build.target` in `GET /api/v1/system` (on `release/v0.5.1` too). So `docs/soak/tools/linux/collect.py` reads it once at start, through the logged-in session it already holds for the settings checks, and writes `build_target` to `run.json`. Only a well-formed triple is kept; nothing else from `/system` is recorded (not the commit, the version or the host's time zone). If the read fails, the field is left out and `events.log` says so. The shipped binary is unchanged.
- **`analyze_cr0.py` decides who serves C's `malloc` from it** (`c_allocator`):
  - **A `*-linux-musl` target: mimalloc.** SQLite's count is already inside mimalloc's committed bytes, so other heap is `RssAnon − mimalloc`, and the label says SQLite and zstd are in mimalloc.
  - **Any other target** (glibc, Windows): **the C library.** Other heap is `RssAnon − mimalloc − SQLite` when every window point has SQLite, as before.
  - **No target recorded** (older collectors, the Windows collector, a failed read): **unknown.** Nothing beyond mimalloc is subtracted, and the label and a line under the table say SQLite may be inside it. The old default subtracted SQLite here, which was right only for non-musl builds; unknown no longer guesses.
  - **`--c-allocator mimalloc|libc` lets the operator state it.** A musl build made before #448 (on `main` between X-mem-gauges and X-mimalloc-override, or an rc.4 head without #448's hunk) has the musl target but zig's `malloc`, and the target alone can't tell them apart. The report records whether the value came from the target or the operator.
  - The report adds `build_target`, `c_allocator` and `c_allocator_source` to `memory_attribution`. `other_heap_excludes_sqlite` keeps its meaning.
- **The memory gate is unchanged.** `memory_fit` (`private_bytes` over baseline hours 12-24) reads neither field. A new test runs one fixture with every memory gauge under six combinations of target and `--c-allocator`. It checks that the gate's numbers and its Markdown block equal the values pinned from the analyzer before this change, and that every report key except `memory_attribution` is identical across all six. A one-off comparison against `origin/main`'s analyzer gave the same result on three fixtures.

## Comments and the release check
- **`crates/lodestar/src/memstats.rs`:** the module doc now gives both builds. musl: mimalloc is also C's `malloc`, and SQLite is part of its bytes. Elsewhere: C allocates outside mimalloc. No `C_MALLOC_IS_MIMALLOC` constant: nothing in the binary reads it, and the analyzer takes the build from `/system`.
- **`crates/lodestar/src/thp.rs`:** `MIMALLOC_ALLOW_THP` covers mimalloc's arenas. In musl builds those hold the C heaps too. Elsewhere only `PR_SET_THP_DISABLE` covers them.
- **`.github/workflows/release.yml`, "Check malloc is mimalloc's":** the step also fails if `nm` lists any symbol containing `SmpAllocator` or `c.malloc.`, zig's own `malloc` and the allocator under it. That would be a second C heap linked beside mimalloc. It matches fixed strings, dots included, so no `mi_*` or Rust-mangled name can match. It reads `nm`'s output from a file, not a pipe. The comment now names the four symbols the step checks.
- **ring isn't a C allocator.** Its C and assembly code never calls `malloc`. The step comment and `crates/lodestar/Cargo.toml`'s comment drop it from the list of C code moved onto mimalloc.
- **`2026-11-05-X-mimalloc-override.md`:** r4's 128-512 KiB mappings now read "grew from 1,843 to 2,085", as #448's body and the r4 verdict say. It no longer reads "1,843 of them".

## Not changed here
- `crates/lodestar/src/metrics.rs`'s `# HELP` for `mem_mimalloc_committed_bytes` still says "the Rust allocator". It's shipped text, and this slice doesn't change the binary.
- The soak tools' READMEs (`docs/soak/tools/README.md`, `linux/README.md`) don't list `build_target` or `--c-allocator` yet. They're outside this slice's paths, so the PR names them for the runner.
