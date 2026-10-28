# Bounded validation storage and preservation

Status: **specified and inventoried** (PRE-05–06). No cleanup has been performed.
The historical disk-exhaustion cause remains unproved. The approximately 0.16 GiB
old evidence and less than 1 MiB application logs do not explain it.

## Current workspace ownership

| Worktree under `C:/src/Github/lodestar-v2` | Branch | Recorded head |
| --- | --- | --- |
| Root | `fix/dht-cr0` | `43d054df962098812023fc5d95df139813a429df` |
| `target/worktrees/cr0-soak-tools` | `work/cr0-soak-tools` | `80d7e1988cd6139f2382e48ee4fda334adb4ac0a` |
| `target/worktrees/soak-product` | `work/soak-product` | `07421c3cd11a0a8fd40aca3161f184ae7a742362` |
| `target/worktrees/soak-followups` | `work/soak-followups` | `5e1a53c06abec4fa7be420d84a2be6052c656b11` |
| `target/worktrees/soak-settings` | `work/soak-settings` | `5e1a53c06abec4fa7be420d84a2be6052c656b11` |

Baseline implementation is uncommitted in `soak-followups`; separate Settings
source preparation is uncommitted in `soak-settings`. Its dependency install reused
the existing bounded npm cache. Root plan changes
and untracked review material remain untouched. The inherited support/product
stack is already included; do not cherry-pick it again. Heads identify checkout
bases, not the source of the current built candidate. Before final freeze, record
the committed source identity and executable/UI/collector hashes together.

## Build storage policy

Use only this integration worktree's `target/cargo` for Cargo output, with
`CARGO_INCREMENTAL=0` and at most four build jobs. Use its `target/npm-cache` for
npm downloads. Build the frontend first, copy the successful production output
to a new immutable `target/packaged-web-<revision>`, and point Rust checks and the
release build at it with `LODESTAR_WEB_BUILD`. Clippy invokes the Rust build script
too: it must not read `web/build` while Vite is replacing that directory.

The latest bounded inventory found 25,133,750,682 bytes in `target/cargo` (13,189
files), 148,028,536 bytes in the local npm cache (1,408 files), and about 3.66 MB
in each frozen UI copy. No read failures occurred. Free C: space was 116.6 GiB
at the later launch-readiness check. These are observations, not reserved space.

Before each heavy validation batch, require at least 50 GiB free and inventory
the generated Cargo directory. Stop adding build profiles or worktrees if that
directory exceeds 35 GiB; inspect what grew before deciding which named generated
cache can be discarded. Recheck after each batch. Never run compilation, package
installation or browser suites during the measured same-PC soak. The collector's
independent disk monitor protects collection; it is not a substitute for bounded
build storage or understanding unrelated background growth.

October 28 refresh: C: has 117.4 GiB free; `target/cargo` contains 13,332 generated
files totaling 23.51 GiB, below the 35 GiB inspection ceiling. The r14 executable
was copied to `target/packaged-bin-r14/lodestar.exe` and its hash verified before
any replacement build. `target/packaged-web-r16` is a separate frozen UI input.
No old build, evidence, database, worktree or Docker data was removed.

## Evidence-preserving cleanup procedure

1. Identify the exact generated directory and its creator. Record its resolved
   absolute path, size, file count and reason it can be regenerated.
2. Verify the path is a child of this worktree's `target`, has the expected exact
   directory name, and has no reparse point or symlink in its ancestry or subtree.
   Refuse a path equal to root `target`, any worktree, or any ancestor thereof.
3. Confirm no build, test, collector, application or preview owns files there.
   Preserve all logs/results, recovery fixtures, manifests and frozen candidate
   inputs outside the proposed cache. Unknown ownership means no deletion.
4. Present the exact candidate and lost regeneration cost in the ledger. Never
   use root `cargo clean`, broad globs, age-based deletion, Docker prune, volume
   deletion or a shell pipeline that computes paths and sends them elsewhere.
5. If a cache is selected for removal, use one PowerShell process and
   `Remove-Item -LiteralPath` only after the containment checks above; record actual
   free-space change. No deletion is implied by this procedure or this inventory.

Soak evidence, stopped databases and WAL/SHM siblings, recovery copies, worktrees,
unknown files and Docker volumes are never cleanup candidates in this task.

## Launch environment observation

The latest check found no native Lodestar process and no owner of HTTP 8484 or UDP
6881, but **Docker Desktop and `com.docker.backend` processes were running**.
Earlier stopped-Docker observations are stale. The collector now refuses launch
when these processes or either required port are present, and refuses if process
or listener enumeration fails. No Docker process was stopped or restarted by this
work. Native baseline launch remains blocked until the agreed Docker condition
and background disk-growth review are satisfied.

October 28 final-preparation observation: C: free space was 124,857,860,096 bytes
(116.28 GiB). A read-only inventory of Docker's named storage/log directories
found two VHDX files totaling 87,244,668,928 logical bytes: the data image is
87,144,005,632 bytes (81.16 GiB), and the distribution image is 100,663,296 bytes.
These are file lengths, not a claim about physically allocated disk blocks.
VM logs total 21,420,578 bytes across 26 files; host logs total 3,146,132 bytes
across 14 files. Neither current size nor modification time establishes the
historical exhaustion cause. No raw log content was read or published.
The read needed sandbox escalation and succeeded; no Docker state was changed.
Docker remains an active background storage owner and must be stopped before
the native launch gate can pass. Repeat free-space/size observations after that
handover; do not interpret these point-in-time lengths as a measured growth rate.

At 19:32 UTC on October 28, the approved graceful Docker handover completed.
The CLI was located under the user's `AppData/Local/Programs/DockerDesktop`
installation, rather than the system-wide default. The local engine had no
running containers; its sole listed Lodestar container had exited with code 0
about 40 hours earlier. A second empty-workload check preceded the non-forced
Desktop stop. No container, volume, log or disk image was removed or restarted.
Process/listener checks afterward show no Docker backend or Lodestar process
and no owner of HTTP 8484/UDP 6881. C: free space was 124,831,092,736 bytes at
19:32:39 UTC and 124,831,588,352 bytes at 20:34:39 UTC (net 495,616 bytes more
free over 62 minutes). This quiet interval does not predict future growth;
the independent collector reserves and the same-host build restrictions remain.
The final packaging build's storage is separately bounded and must be rechecked.
