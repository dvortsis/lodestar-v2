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

The check hashes the file at the running process's reported program path. If Windows keeps the original path for a process whose file was renamed and replaced, or an in-use file is replaced only at the next reboot, the hashes can still match. The runbook therefore also tells the owner to restart the service after an upgrade; the upgrade record's `image_matches_installed` stays the second line of defence. A start-time or pending-rename check would need a Windows machine to verify and is left for a follow-up.
