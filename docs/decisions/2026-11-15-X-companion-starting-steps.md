# X-companion-starting-steps decisions

A follow-up to X-cmp09-starting-status (#903), and the runner's UX call. When a companion Lodestar runs is still starting, detection and the guided steps say so, with Try again, instead of "The companion didn't answer". This file says what the code now does (`docs/security/v1/README.md` §5). Calls not marked as the runner's are this PR's, and the runner flags them to the owner (`docs/owner-decisions.md` §2.19).

## A distinct adapter state, at the source

- **Chosen: a new `AdapterError::Starting`**, set where Lodestar knows the port is its own: `CompanionHttp`, for a managed port that isn't ready.
  - **Not chosen: mapping at the guided-steps layer.** That layer can't tell a managed port from a hand-run companion that didn't answer, so it would have to guess.
- **What `CompanionHttp` now answers** for a port a companion Lodestar runs holds (`Bindings::managed`) that isn't ready:
  - while a start is under way (`Bindings::starting`, new): **`Starting`**. A start holds the port, and its child is about to start, starting, running or backing off before its next start;
  - **From the runner's review:** not while the run up now had a readiness attempt refused (its listener isn't its own, or can't be told), since waiting won't change that. A refusal is tied to its run (`refused_run`), so a later run of the same claim isn't marked by it. And not once the supervisor's status channel is closed (its supervisor is gone), as `up()` already does;
  - otherwise: **`NotReady`** (from #903). That covers a stopped or removed companion, a failed start, one waiting for you (after a crash loop, say) and one held while its route is down.
- **Where the answer applies:**
  - **What carries something** (a header, a body or a query) is refused before it's sent, as before. Only the error changed: it used to be `Unreachable`.
  - **A bare probe** still goes to `127.0.0.1`, as before. If it finds nothing there (refused or timed out), it gets the same answer in place of `Unreachable` or `Timeout`. One that's answered is returned as before.
  - **The overall deadline** (`GuardedHttp::send`'s own timeout) gives the same answer at a managed port that isn't ready, so whichever deadline fires first, detection never reads it as "not here" (from the runner's review).
  - **A ready port** is asked as before.
- **Fail-closed is unchanged:**
  - nothing new is sent;
  - the loopback gate still refuses before sending;
  - no ungated client or path is added.
- **A crash loop** reads as starting while it backs off between starts, and as not ready once it gives up and waits for you.

## "Not here" stays "not here"

- **Detection** (`not_found_or`) still reads unreachable as "not here". `Starting` and `NotReady` only come from a managed port, so a loopback or container target with nothing managed there still reads "not here".
- **At a managed port,** detection now says "starting" or "not ready" instead of "not here". This replaces #903's decision to keep that refusal `Unreachable` on purpose, which named this slice as its follow-up.
- **Health and `listening`** read `Starting` as unreachable, as they do `NotReady`.

## What the API and the web app say

- **Detection** (`POST /companions/detect`) answers `starting`, a list of the companions that answered `Starting`.
  - The list is in probe order, with each companion once, and never one that was found.
  - Any other failure is still "not here".
  - `CompanionsGuidedApi::detect` now returns `DetectedCompanions`.
- **Codes:**
  - a guided step refused this way answers `companion_starting`, listed among `/companions/{id}/target`'s 409 codes;
  - the configure step's own code for it is `starting`.
- **The web app** (Settings → Companions):
  - **Detection:** lists each starting companion, for example "qBittorrent is still starting, so it couldn't be looked for yet. Try again in a moment.", with Try again. It shows that list instead of "None found." when nothing was found.
  - **A refusal coded `companion_starting`** shows "Lodestar's copy of this companion is still starting. Try again in a moment." in an info banner. Try again sends the same request again.
  - en and zh.
- **Not changed:** paste doesn't probe, and the plan and confirm steps aren't in the web app yet, so they show nothing new. Their codes come through the same mapping.

## Tests

- **`crates/companions/tests/detect.rs`:**
  - at a managed port held by a start and not ready, what carries something and a bare probe that finds nothing are `Starting`, and detection there answers `Starting`;
  - while it waits for you after a crash loop, and once released, they're `NotReady`;
  - a starting port that accepts but never answers ends at its deadline as `Starting`, and detection there isn't "not here";
  - `Bindings::starting` (unit test, from the review): a run up with a refused attempt isn't starting, a new run of the same claim is, and a closed status channel isn't;
  - `Bindings::starting` (unit test): true for a claimed port and a child that is starting, running, unhealthy or backing off; false once it waits for you, is held or stopped, once released, and for a port never held;
  - with nothing managed there, detection finds nothing ("not here");
  - a ready managed port is asked as before.
- **The supervisor and manager tests** (`tests/supervise.rs`, `tests/supervised.rs`, `tests/manager.rs`) that check a credentialed request is held back now expect the new answers: `Starting` while a run is up or restarting, and `NotReady` while held or after a stop.
- **`crates/companions` unit tests:** health and `listening` read `Starting` as unreachable, and `not_found_or` passes it through at a fixed target.
- **Serve's wiring:**
  - `Starting` is coded `companion_starting`;
  - detection lists companions still starting, once each, and not as found;
  - unreachable everywhere lists none.
- **`crates/api`:** detection answers its starting list in the body.
- **web (vitest):**
  - a starting companion is listed instead of "None found.", and Try again looks again;
  - a `companion_starting` refusal of a detected target (as in use) shows its own words, never "didn't answer", and Try again sends the same request;
  - the code's words differ from unreachable's.
- **No e2e case:** the companions e2e spec doesn't cover detection or adding.
- **Left as is (the review's optional items):**
  - the widened `refused()` helper in `tests/detect.rs`, since the new tests pin exact answers;
  - detection's "never one in `found`" exclusion has no test of its own: under the native runtime a companion is probed at one fixed target, so the fake can't answer both ways for it.
