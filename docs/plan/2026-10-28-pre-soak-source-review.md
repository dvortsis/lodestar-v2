# Final pre-soak source review

Scope: the baseline integration changes above prepared-fix base `5e1a53c0`,
reviewed October 28. This review concerns native Windows baseline preparation;
it does not accept the separate Settings branch or a cross-platform release.

## Reviewed boundaries

- Actor admission: category counters follow the existing admission decisions.
  Caller-closed, paused and capacity use documented precedence; failed
  acknowledgment is separate. Channel rejection returns the previous boolean
  outcome and does not consume an accepted attempt.
- Pool diagnostics: inline flags observe request/fetch service; no cap, retry
  deadline, queue, target or transport changes. Restored entries' historical
  peer/fetch state remains explicitly unknown. Metrics labels are fixed enums.
- Collector: fresh-directory refusal, loopback/port/process checks, hash-bound
  build record, copied signal helper, independent storage cadence, protected
  process cleanup and independently guarded final writes were read together.
  Cleanup verifies PID/start identity. A signal or a missing result never proves
  termination. Existing injected failures and actual Windows smoke cover these
  paths; no real disk was filled.
- Analyzer: same-phase/process intervals, resets/gaps, fixed-label exports,
  missing-result uncertainty and shutdown/finalization reporting were reviewed.
  Reports remain descriptive and never grant acceptance automatically.
- Frontend: authentication loading keeps controls hidden/inert; initial session
  confirmation does not repeat the request establishing it; subsequent session
  transitions invalidate detail work. Deferred magnet/manual UI checks the
  visibility generation. Query destruction cancels pending navigation work.
- Packaging: Windows path normalization, shared labels, deferred module groups,
  root error/layout accounting and required manifest entries were reviewed.
  Route accounting fails closed on missing inputs. Existing bundle limits and
  Linux visual baselines are unchanged.

## Provenance closure

The r16 binary passed packaging checks but was built from uncommitted source.
It remains preserved as historical evidence. The new `prepare_windows.py`
requires clean committed source, records commit/tree/toolchain, rebuilds UI then
Rust, verifies the offline smoke and freezes collector/UI/executable hashes.
Source changes during preparation abort publication; failed packages remain
private diagnostic material. Its tests cover dirty/changed source, output
containment/reuse, disk/cache limits, smoke identity and clean shutdown, exclusive
record creation, and manifest tampering/missing files/path escape.

The source revision and successful package must be recorded after this review;
the existence of the preparation command is not proof that preparation passed.
Docker is still running. Current disk inventory does not prove the historical
exhaustion cause or a background growth rate. These remain operational launch
checks. Unix updater and pinned Linux visual acceptance remain explicit platform
gates; Windows test selection cannot satisfy them.

## Final-build follow-up

The clean-source package attempt at `ce59c3e3` correctly stopped when the rebuilt
Alert page exceeded its 45 KB budget by 14 bytes. No release binary or manifest
was published. Its private logs remain under `target/launch-candidate-r17`.
Grouping channel icons separately made the result worse (45.22 KB); that change
was removed. The retained change shares the existing Badge/Banner status icon,
ink and border maps, preserving their classes, roles, labels and behavior. It
removes duplicated component-local maps without changing any bundle limit.
Focused component and browser validation, followed by a new committed package,
are required before final acceptance of this follow-up.

The local Docker CLI was located beside the observed backend in the user's
DockerDesktop installation. An explicit local named-pipe query found zero
running containers and the older Lodestar container exited cleanly. The workload
check was repeated immediately before `docker desktop stop --timeout 60`, which
completed successfully without force. Subsequent process/listener observations
show no Docker backend, no native Lodestar, and free HTTP 8484/UDP 6881. The
container, volumes and old evidence were preserved. A standard-location/PATH
check in the existing Ubuntu environment found no Cargo, Rust compiler or Node;
the Unix updater and pinned Linux visual gates remain unexecuted release gates.

The retained status-map change passes 128 targeted script/component/Alerts tests;
after sharing border classes too, both primitive suites pass all 28 cases.
The r20 production build passes every unchanged budget (Alert 44.92 KB/45 KB,
detail deferred 33.80 KB/33.9 KB). All 67 boot/error/Alerts Windows browser cases
pass, including keyboard and light/dark accessibility checks. The managed
preview closed normally. Documentation sync and all 1,156 lint checks pass;
all three Unraid template validators pass. These do not stand in for Linux pixels.

The original interrupted evidence was rechecked against its private manifest:
1,565 readable files match; two protected logs initially failed sandbox reads,
then both byte lengths and SHA256 hashes matched with approved read access.
Thus all 1,567 preserved files match. The initial incomplete check remains in
`target/original-evidence-recheck-r20.json`; the two supplemental comparisons are
in `target/original-evidence-protected-hashes-r20.json`. No original file was
opened through a writable application connection or changed by these checks.
