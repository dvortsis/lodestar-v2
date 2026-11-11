# CMP-13: Companion pin-refresh tool and workflow: upstream OpenPGP and SHA-256 checked at pin time, catalogue.json written
**Model:** Opus, with `/security-review` (supply chain). A fresh Opus reviewer follows.

Runner brief, 2026-11-11T20:20Z (UTC). Slot 4. Epic E-v06-companions, on the companion gate's path: CMP-G depends on CMP-13. Windows configuration (CMP-11 and CMP-11b) is refused as `awaiting_pin` until a pin exists. Deps: CMP-0, X-cmp0-polish, X-cmp-egress-prelaunch and `ext:owner-cq2`, all met.

## Read first
- `docs/plan/2026-11-06-v06-companions.md`: row CMP-13 (~60), Q2 (~178), the dependency rules (~84), and the sections on CMP-1 and CMP-G.
- `docs/decisions/2026-11-06-runner-planning-wave-answers.md` row cq2 (line 13): the owner's answer as the runner applied it. The SHA-256 pin is always the run-time gate. Upstream OpenPGP signatures are checked **once, at pin time**, by your tool, against upstream keys pinned by fingerprint. There's no OpenPGP code in the binary.
- `docs/decisions/2026-11-06-CMP-0.md` (the catalogue format: `pin`, `upstream_sig` as `none` or `verified_at_pin` with the fingerprint, and "waiting for a pin is the fail-safe state") and the CMP-1, CMP-11 and X-cmp-egress-prelaunch decisions files.
- `crates/companions/data/catalogue.json`, its loader and validator in `crates/companions/src/`, and `crates/companions/tests/catalogue.rs`.
- `crates/lodestar/src/update/download.rs`: the download discipline (host allowlist, redirect hops, byte cap, streaming SHA-256). Apply the same rules in the tool.
- CMP-13's `notes` in `runner/slices.yaml`. They are requirements:
  - At pin time, re-check Prowlarr's ProxyEnabled default and .NET's DefaultProxy environment behaviour for the pinned version.
  - A framework-dependent build whose runtimeconfig lists only Microsoft.AspNetCore.App is refused on SOCKS5 (fails closed).
  - Installers get a per-artifact release folder.
  - SourceForge and FossHub are never a fetch host.

## Do
1. **`tools/companion-pins/`**: a tool, in Python (stdlib only) or a small Rust bin in the tool folder, your call. It:
   - reads the catalogue and a pin request (companion, version, which artifacts);
   - downloads each artifact only from the entry's `hosts` allowlist, with HTTPS, capped redirects and a byte cap, and computes its SHA-256 while streaming;
   - where upstream publishes an OpenPGP signature, verifies it with the runner image's `gpg`, in a temporary `GNUPGHOME`. It imports only keys pinned by **full fingerprint** in the tool, and refuses a valid signature by any other key;
   - records `upstream_sig: verified_at_pin` with the fingerprint, or `none` where upstream publishes nothing;
   - for containers, resolves and pins the image digest;
   - writes `catalogue.json` deterministically (stable order and formatting, so a re-run with the same inputs gives a byte-identical file);
   - **fails closed:** any mismatch, missing signature where one is expected, unknown key, host outside the allowlist, or oversized download aborts with no file written.
2. **`.github/workflows/companion-pins.yml`**: `workflow_dispatch` only (inputs: companion, version). Use `permissions: contents: read` only. It runs the tool and uploads the proposed `catalogue.json` and a verification report as an artifact and job summary. **It never pushes, commits or opens a PR.** A person opens the pin PR from the artifact, and the runner reviews it. Any write permission or secret here needs the owner, so ask the runner first if you think you need one.
3. **Tests** in `tools/companion-pins/`, run offline on fixtures:
   - a good artifact and signature; a wrong SHA; a valid signature from an unpinned key; a bad signature;
   - a host outside the allowlist; an oversized body; a redirect to a disallowed host; a determinism re-run.

   Generate the fixture keys in the test with gpg into a temporary home; commit no private keys. Add a CI job or step that runs the tests on changes under `tools/companion-pins/**`, inside `ci-ok`. That's a shared edit to `.github/workflows/ci.yml`: one job.
4. **No real pins in this PR.** `catalogue.json` and `tests/catalogue.rs` change only if the tool needs a format field the loader doesn't have yet. If so, tell the runner first: CMP-11b may be editing near them. The first real pin run is a separate, reviewed PR after this merges.
5. **Decisions file** `docs/decisions/2026-11-11-CMP-13.md`:
   - each companion's upstream signing story (who signs what, the fingerprints you pinned and where you got them);
   - the pin-time checks from the notes above;
   - how a maintainer runs a pin;
   - what the tool refuses.

## Owns
- `tools/companion-pins/**`, `.github/workflows/companion-pins.yml`, the decisions file, and one test job in `.github/workflows/ci.yml`.
- `crates/companions/data/catalogue.json` and `crates/companions/tests/catalogue.rs` only for a format change approved by the runner (see Do 4).
- **Don't touch:** `crates/companions/src/**` (CMP-11b is starting there), `web/**`, `docs/user/**`, `runner/`, and every other workflow.

## Done when
- The tool's tests pass offline. One real dry run against current upstream (no catalogue change committed) is described in the PR, with the SHA-256s and fingerprints it found.
- **Checks:** the repo's Python lint, if any, and the tool's tests; `cargo fmt --check`, clippy and tests if you write Rust.
- **Size:** about 450 lines (plan). If it heads past ~1,000, tell the runner.
- **Process:** follow `.claude/skills/steward/SKILL.md`, push once per round, never merge. Branch `slice/CMP-13`; PR title `CMP-13: Companion pin-refresh tool and workflow`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and full head SHA once ci-ok is green, using the Claude Code Remote MCP `send_message` tool.
