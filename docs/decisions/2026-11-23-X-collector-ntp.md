# X-collector-ntp decisions

The runner launched this fix on 2026-11-23 under the owner's 06:09Z exception for the cloud testing environment. X-test-vm reported that every stage-1 CR segment from the fleet's collector was judged invalid, with the single reason D1a `ntp_synchronized` (`docs/soak/tools/analyze_cr.py`).

## Cause
- **The host's clock was fine:** chrony reported leap status Normal and an offset of about 1 µs.
- **The collector couldn't see it.** The collector runs in the stock `python` image, which has neither `chronyc` nor `timedatectl`, so `ntp_status()` fell to `kernel_ntp_status()`.
- **Why the kernel source read `unavailable` is a hypothesis.** `kernel_ntp_status()` called glibc's `ntp_adjtime`, which current glibc implements as `clock_adjtime(CLOCK_REALTIME)`. X-test-vm's lead is that the container refused that call, because Docker's seccomp profile allows it only with CAP_SYS_TIME. But Docker's default profile has allowed `clock_adjtime` and `clock_adjtime64` unconditionally since moby v20.10.0; only v19.03 gated them. The fleet runs the collector with only `--cap-drop ALL` and `no-new-privileges` (`infra/fleet/steps/vm/block-lib.sh`). So on a current Docker the call may not have been refused at all, and the cause may lie elsewhere (see "Diagnosis pending").

## Fix
- **The raw `adjtimex(2)` syscall is tried first** (`docs/soak/tools/linux/collect.py`, `kernel_ntp_status`), through ctypes `syscall()`. Docker's default profile is expected to allow `adjtimex`; X-test-vm will confirm.
  - The syscall numbers are given per architecture (`ADJTIMEX_SYSCALL`): x86_64 159 and aarch64 171, on 64-bit builds only, where the kernel's struct matches the one the collector declares.
  - On any other architecture, or if the syscall fails, `ntp_adjtime` is tried next, as before. If both fail, the result is `unavailable`.
- **Read-only.** The struct is zeroed (`modes = 0`) on every call, so neither call can change the clock. The tests assert `modes == 0` on every struct passed in.
- **The result format is unchanged.** The same `source: "kernel"` and the same fields come out, and the parsing moved unchanged into `parse_timex()`. Nothing else the collector records changes: the privacy scrubber's inputs are the same.
- **The collector stays unprivileged.** It gets no CAP_SYS_TIME, no looser seccomp profile and no chrony socket mount. The README says not to add them.
- **Tests** (`test_collect.py`, `KernelNtpTests`) use a fake libc: synchronised, STA_UNSYNC, TIME_ERROR, STA_NANO, the estimated error when it's larger, the fallback order, the aarch64 number, an unknown architecture, both calls failing, and a libc with neither call. They also pin the struct layout (`Timex` is at module level: `esterror` at 32, `status` at 40, size at least 208 on 64-bit). A live, read-only test on 64-bit Linux checks that the real call gives `source: kernel` and that the raw syscall and `ntp_adjtime` agree on `status`, `tick` and `tolerance`.

## Diagnosis pending
The runner merges this only after X-test-vm's probe shows that the patched function returns `source: kernel` in the collector image. If the probe shows another cause, such as `rtcsync` or an old Docker, the fix is widened then. X-test-vm records:
- `docker version` on the lane;
- `source`, `synchronized` and `offset_seconds` from a segment's `run.json`;
- from a one-off container of the collector image, for both the raw syscall and `ntp_adjtime`: the return value and `ctypes.get_errno()`;
- whether the host's `chrony.conf` has `rtcsync`. Without it, chrony may leave STA_UNSYNC set, and the kernel source then reads unsynchronised.

## A missing counter in the allow-list (runner, 2026-11-23)
- **`fetch_first_window_timeouts_total{transport}` is now kept** in snapshots (`LABELLED` in `collect.py`, labels `tcp` and `utp` only), so it no longer reads "not collected". X-test-vm found it was missing. The binary at the pin exports it (`docs/metrics.md`; `docs/decisions/2026-11-14-X-fetch-first-window.md`). It isn't required, because older binaries don't export it.
- **No other counter was missing.** I checked every metric name that `analyze_cr.py`, `analyze_cr0.py`, `screen_sweep.py` and the fleet's scripts (`infra/`) read against the allow-list (`KEEP`). Every documented metric they read is already kept.
- **The test** (`test_snapshots_keep_first_window_timeouts_by_transport`) checks that both transports are kept, and that an unknown transport and an unlabelled series are dropped.

## Collector identity, and how a lane picks the fix up
- **What's recorded:** `run.json` records `collector_sha256` per segment, and analyze_cr's `identity()` reports it. `judge_block` doesn't require it to match across a block's segments: it checks `image_digest_differs`, not the collector. Each segment is judged by its own `collector_sha256`.
- **What the fleet pins:** the rules commit gives the collector (`infra/fleet/steps/rules.py`, `TOOLS`). `start-block.sh` stages it once into `$BLOCK/tools/linux/collect.py`, which is mounted read-only into every segment's collector container.
- **New blocks** pick up the fix by using a rules commit at or after this PR's merge (runner, 2026-11-23).
- **Running blocks (runner, 2026-11-23):** the cloud coordinator may swap the collector between segments, on three conditions:
  - the staged `$BLOCK/tools/linux/collect.py` is replaced with **exactly** the file on `main` at this PR's merge commit or later, so its sha256 equals that commit's file;
  - the swap time, the old and new sha256 and the commit are recorded in the block's driver log and in its copy-off record;
  - no segment is ever split across collectors: the swap happens only while no segment is running, since the mid-segment `collect.py note` calls (`block-lib.sh`) run whatever file is staged at that moment.
- **This is the one exception** to "the collector comes from the rules commit", for stage 1 only.

## Not changed
- analyze_cr's D1a rule.
- Segments already judged invalid. The runner decides separately whether they're re-judged.
