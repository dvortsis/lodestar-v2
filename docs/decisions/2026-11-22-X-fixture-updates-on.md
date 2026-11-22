# X-fixture-updates-on decisions

Runner brief, 2026-11-22Z, with the runner's calls on step 0. It adds a `serve --fixture` variant so the Updates screen's "on" states run against the real server, offline. Core work: it sits beside update signing, so the security bounds below are the runner's and each one has a test.

## Scope (runner, step 0)
- **Updates only.** The brief also asked for Backups and a failed rebuild.
  - **Backups are out** until that feature exists: the server has no Backups screen or route to serve.
  - **The failed rebuild is out too.**
- **The brief's premise was corrected at step 0.** It read #1043's "rebuild" and "paused" as screen states a fixture could switch on. They are status-pill states. What the fixture can switch on is the Updates screen's: a release available, a recent check, automatic installs paused.

## The variant
- **`lodestar serve --fixture [DIR] --fixture-variant updates-on`.** The value is a closed list (clap `ValueEnum`), so an unknown name is a usage error.
- **`--fixture-variant` requires `--fixture`.** Plain `serve`, and `serve --child`, can't name one. A test covers this from the CLI parse down.
  - **clap `requires` alone wasn't enough.** clap waives a requirement when the missing argument conflicts with a present one, and `--fixture` conflicts with `--child`. So `serve --child --fixture-variant updates-on` parsed, which this slice's first test run caught. The argument now also `conflicts_with = "child"`, and the test checks both orders.
  - **`main.rs` doesn't rely on the parse.** A `Serve` with a variant and no `--fixture` exits 2 before the privilege drop or anything else touches a folder, and the plain-`serve` arm matches only `fixture_variant: None`.
  - Even when it parsed, plain `serve` ignored the variant. Only `fixture::serve` sets the flags that select the sample.
- **The same data.** The variant changes only what `/updates` answers and the default updates mode. The fixture database is built exactly as before.
- **The mode is Notify** unless `--config` (`updates.mode`) or `LODESTAR_UPDATES` sets one. The default fixture leaves it Off, as before.

## Recorded in `fixture.json` (runner)
- **A variant fixture's manifest has `"variant": "updates-on"`.** It's added after the build, through a temporary file and a rename as the builder writes it.
- **The default fixture writes no key.** Its `fixture.json` is the builder's, byte for byte. A test builds one and checks that recording no variant leaves its bytes unchanged. `bench` (the builder) is untouched by this slice, so that file is main's.
  - **What that test proves, and what it doesn't** (#1051 review): it calls `record(None)` directly, so on its own it's close to a tautology. What holds the guarantee is that `run` writes the manifest only through `record`, and `bench` is unchanged. The integration test adds that a default fixture's `/updates` is still the unavailable stub.
- **An interrupted variant build** (a crash between the build and `record`) leaves a default fixture. A later `updates-on` run then refuses it with exit 3. That's safe, and the refusal says to delete the folder to rebuild it, as the interrupted-build message does.
- **The refusal quotes the manifest's `variant`** (Rust debug quoting for a string, JSON for any other value), so a crafted `fixture.json` can't put terminal escape codes on stderr (#1051 review). A test feeds it an escape sequence.
- **A folder serves only the variant it was built as.**
  - A default fixture asked for `updates-on` is refused, and so is the reverse.
  - So is any other `variant` value: an unknown name, an empty string, `null`, a number or an array.
  - A refusal is exit 3 with nothing written, as for any other foreign folder. The DIR safety rules are unchanged: an empty or new folder is built, a matching fixture is reused, anything else is refused, and a temporary folder is used by default.

## What `/updates` answers (`fixture/updates.rs`)
- **On:** `unavailable` is null and the last check is `ok` at the fixture's anchor.
- **One release:** the next minor version after the running one, dated two days before the anchor. It has fixed notes saying it's a sample, and no notes link.
  - **Its level is `feature`,** a release level word (`fix`, `feature`, `major`) as the engine sends it, so the screen shows its level badge. It is not the policy's `features` (#1051 review).
  - **It's never eligible.** Its `blocked_by` is the real policy's (`update::policy::plan`) for the saved settings and the pause, followed by `capability` (`fixture`). With the defaults (Notify, Fixes, a 3-day wait) that's `mode`, `level`, `wait` and `capability`. The policy is a pure function, so it reaches nothing.
- **Pausing works:** `POST /updates/pause` holds `paused_until` in memory. The release then shows `blocked_by: paused`. `days: 0` resumes, and a restart forgets the pause. The route already allows only 0, 1, 7 or 30 days.
  - **This departs from DUR-26,** which acknowledges a write only once it's durable. Here a pause answers 200 although it's held only in memory. That's approved scope: the fixture is sample data that installs nothing, so a forgotten pause can't let anything install. Saving the pause would mean writing update state into the fixture folder for no gain.
- **The policy is the saved settings,** as the engine's is: mode, level, security fixes, wait, window and reindex. A change saved in Settings shows at once. The window's zone is `UTC`, so the answer doesn't depend on the machine.
- **`/system`'s Updates row** is on unless the mode is Off, checked at the anchor.

## Security bounds (runner; each is tested)
1. **Nothing reaches out.** The provider holds no `Fetch`, no `Source` and no client. `update::start` returns it before any client is built, so none is built under the fixture.
2. **No key and no verifier change.** The trust core (`crates/update`) isn't touched, and no key is added behind the `fixture` feature. That feature is in the default set, so a key behind it would ship (runner: that route is withdrawn).
3. **Install is never offered.** `capability` is `{can_install: false, reason: "fixture"}` and rollback is unavailable for `fixture`.
4. **No new error** (runner). Check, install, skip, rollback and clearing history answer the existing `409 updates_unavailable` (`fixture`), as a default fixture's do. Only pause answers 200.
5. **Only a variant fixture selects it.** `update::start` uses the sample only while both flags are set: serving a fixture, and that fixture's variant is `updates-on`. Only `fixture::serve` sets them, and plain `serve` never reaches it.

## The screen's wording
- **No web change.** The Updates screen has no message for the reason `fixture`, so it shows its generic "This installation can't update itself." That's true here, and the screen's fixture banner doesn't show, because `unavailable` is null.
- A fixture-specific line can come with a web slice if the screens want one.

## Tests
- `fixture/updates.rs`:
  - the status shape;
  - the reason `fixture` on capability and rollback;
  - every write but pause is the `fixture` 409;
  - pause and resume, with an injected clock (no timing);
  - the policy and the `/system` row follow the settings;
  - the release version, including an unparseable running version;
  - the release's level word and its `blocked_by` from the policy, with the defaults and when paused under settings that would otherwise install it.
- `fixture.rs`:
  - the sample needs both flags;
  - a folder serves only its own variant, the default included, and odd `variant` values are refused, with an escape sequence never written raw;
  - the default manifest is the builder's, byte for byte, and the variant only adds its key;
  - Notify unless the config file or the environment sets a mode;
  - the `Variant` name matches its CLI value.
- `cli/mod.rs`:
  - `--fixture-variant` parses only with `--fixture`;
  - it's refused with `--child` alone or with no flag;
  - an unknown variant is refused.
- `tests/fixture.rs`, through the binary:
  - a default fixture's `/updates` is still `unavailable: "fixture"`, which catches a sample switched on for every fixture;
  - serve the variant, check `/updates` (including the release's level) and `/system`, pause and resume;
  - each other write answers the `fixture` 409;
  - restarting the same folder without the variant exits 3 and leaves the manifest unchanged;
  - refused starts run with a 30 s deadline and the child is killed on timeout, so a start that serves fails the test instead of hanging it.
