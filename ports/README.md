# Ported old-lineage work

The history swap of 2026-10-30 (owner-decisions §2.25) left three paused slice branches on the old lineage. Each is saved here as one diff from its old merge-base, so the work survives the eventual deletion of the old branches (master plan Part B3). Apply with `git apply -3` on a branch from the new `main` and resolve the conflicts. Don't merge the old branches.

| Patch | Old branch, tip | Old base | Commits | State |
|---|---|---|---|---|
| `B-update-5.patch` | `slice/B-update`, `d7f6c8be` | `f19681d3` (old main at #157) | `B-update-5.commits` | WIP of B-update 5/5: download, overlay staging, the install pipeline, `POST /updates/install`, the 5a decisions. **It conflicts with F-updater (#198)** in `update/engine.rs`, `update/log_events.rs` and the logs golden. It needs an Opus session with `/security-review`. Resume after v0.5.1; the install path isn't in v0.5.1. |
| `M-settings-form-3.patch` | `slice/M-settings-form-3`, `9b84c6ca` | `c7b9d71b` | `M-settings-form-3.commits` | The save pipeline loaded on demand. Superseded by S-settings-reconcile, which bases on `848e10a0` (`work/soak-settings`, already on the new lineage). Kept as reference. |
| `S-settings-1.patch` | `slice/S-settings-1`, `4fc138d7` | `cc2ebcea^2` (old main at that merge) | `S-settings-1.commits` | The Settings screen. Superseded by S-settings-reconcile. Kept as reference for its route measurements and on-demand gating. |

`work/soak-settings` (`848e10a0`) is already on the new lineage (its merge-base with `main` is `5e1a53c0`), so it needs no port.

The secret and personal-path scan on these patches found nothing.
