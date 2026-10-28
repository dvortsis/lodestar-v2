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
