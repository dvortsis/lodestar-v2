# X-collector-ntp (slot 2): the collector reads NTP state inside its container

The runner decided this (2026-11-23T10:4xZ). The cloud-testing coordinator (X-test-vm) reported a blocking bug, so the owner's 06:09Z exception applies ("unless the cloud testing environment ... needs something").

## The bug (X-test-vm's report, numbers only)
- `analyze_cr segment` marks every stage-1 segment invalid with the single reason D1a `ntp_synchronized` (`docs/soak/tools/analyze_cr.py:391`).
- The host's clock is fine: chrony reports leap status Normal and an offset of about 1 µs.
- The collector runs in a container from the stock `python` image, which has neither `chronyc` nor `timedatectl`.
- So `ntp_status()` falls to `kernel_ntp_status()` (`docs/soak/tools/linux/collect.py` about line 1270). That calls glibc's `ntp_adjtime`, which current glibc implements as `clock_adjtime(CLOCK_REALTIME)`. Docker's default seccomp profile allows that syscall only with CAP_SYS_TIME, so it fails even read-only, and the result is `source: unavailable`.
- That last point is X-test-vm's lead, not yet confirmed.
- **Every CR segment from this collector image is invalid:** all cloud lanes, and the Unraid experiment host if it uses the same container.

## The fix (minimal)
1. In `kernel_ntp_status()`, read the kernel's NTP state with the raw **`adjtimex(2)` syscall in read-only mode** (`modes = 0`) through `ctypes` `syscall(SYS_adjtimex, ...)`, with the syscall number per architecture (x86_64 and aarch64 at least). Docker's default seccomp profile allows `adjtimex`. Keep `ntp_adjtime` as a fallback, and keep the result format byte-identical.
2. Never ask for CAP_SYS_TIME or a looser seccomp profile, and never mount the chrony socket. The collector stays unprivileged.
3. **Tests in `test_collect.py`:**
   - the parsing of a fake timex: synchronised, unsynchronised, TIME_ERROR, STA_NANO;
   - the order of the fallbacks;
   - an `unavailable` result when both calls fail.

   Don't call the real syscall in a way that could change the clock: `modes` stays 0, asserted.
4. **Check the collector's identity.** If analyze_cr or the fleet's copy-off records a collector hash and requires it to be the same across a block's segments, say so at step 0. The runner needs to know whether a fixed collector can be swapped in between segments of a running block.
5. Add a decisions file `docs/decisions/2026-11-23-X-collector-ntp.md`: the cause, the fix, and how a running lane picks it up.

## Not in scope
- analyze_cr's D1a rule doesn't change.
- Segments already judged invalid are not re-judged here. The runner decides that separately.
- No cloud calls. X-test-vm verifies on a lane, in a no-network test container, after you push.

## Owns
- `docs/soak/tools/linux/collect.py`
- `docs/soak/tools/linux/test_collect.py`
- `docs/soak/tools/linux/README.md`
- `docs/decisions/*-X-collector-ntp.md`

Ask the runner before touching anything else.

## Rules
- The collector feeds the privacy scrubber's inputs. Change nothing about what it records except the clock fields' source.
- Run the soak tools' tests (`python3 -m pytest docs/soak/tools` or the repo's runner for them) before pushing, and push once.
- **Step 0:** before writing code, send the runner your plan (up to 8 lines), including the identity answer from point 4, then wait for "go".
- The runner reviews on Opus and merges. Send `ready <full sha>` when CI is green.
