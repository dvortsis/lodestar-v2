# X-win-soak-attach-proc-hash decisions

Runner call (`docs/owner-decisions.md` §2.33), from the M8-WIN-1 review's optional item O1. No owner decision changes.

## What the collector now refuses

In service mode, before it creates the run folder, `cr0-soak.ps1` calls `Assert-SoakAttachBinding` and stops with a message and a non-zero exit when any of these holds:

- the SHA-256 of the running service process's program file differs from the hash of the file the service is registered to run (its ImagePath);
- `-Exe`'s hash differs from that ImagePath hash;
- the process's program file can't be read (the run needs an elevated shell).

## Why

An MSI upgrade can replace the file while the old process keeps running (files in use, restart pending). The soak would then bind the new file's hash to `run.service_image_sha256` and every sample while measuring the old binary. A different `-Exe` could likewise leave `run.sha256` and `run.version` describing another file. The upgrade record caught the first case only indirectly.

## Evidence

Unchanged: hashes only, no paths. A refusal writes nothing, because it runs before the run folder exists. The helper takes injectable probes like the lifecycle helpers, and `test_cr0_collector.ps1` covers it with real temp files.

## Limit

If Windows reports the original path for a process whose file was renamed and replaced, the hashes match although the process runs the old binary. The upgrade record's `service_restarted` (the service must have started after the upgrade) is what catches that. If the new file waits for a reboot, the file and process are both the old binary, so the evidence stays consistent and the upgrade check fails because the hash didn't change. The runbook also tells the owner to restart after every upgrade. A start-time or pending-rename check would need a Windows machine to verify and is a runner follow-up.

## Binding window

The helper returns the process id, start ticks and hash it passed on. At attach the collector throws unless the service it then binds has the same pid and start ticks, and the same hash as the check and `-Exe`, so a restart between the check and the binding can't slip through. Test: the helper returns that identity for the collector to compare.
