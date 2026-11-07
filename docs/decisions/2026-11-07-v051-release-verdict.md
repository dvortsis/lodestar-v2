# v0.5.1 release verdict (runner, 2026-11-07)

**Verdict: ready to tag.** Every leg of the v0.5.1 gate (build-plan §0 Track R) has passed. The owner tags `v0.5.1` on **`12cd25ca38f8eac1a930316623dec742c1ba585d`** (`release/v0.5.1` after #558), approves the release environment, and publishes. **Not on `a0eb0a90`:** that commit lacks the operator-guidance fix below.

## The legs
| Leg | Build | Result | Record |
|---|---|---|---|
| R1″ Linux Docker soak r5 (memory gate) | rc.4 `dba1a5c0` | **PASS** | `2026-11-06-soak-r5-verdict.md` |
| R2 gluetun leg | rc.4 `dba1a5c0`, the same image digest as r5 | **PASS** | below |
| R-win Windows MSI service smoke | rc.4 | **PASS** | build-plan §0 row R-win |
| R3‴ rc.5: Legal Notice v3, text only | `a0eb0a90` (#540) | merged; Release dry run and CI green | build-plan §0 row R3‴ |
| R-legal (§2.48) legal documents against the build | `a0eb0a90` → `12cd25ca` | **PASS after one docs fix** (#558) | below |

## R2: the gluetun leg (runner's verdict, 2026-11-06T23:55Z)
**Command line:** `--leg gluetun --require-gluetun`, with a 2 h baseline. The analyzer reports gate evidence yes and `gluetun_network_parent: true`.

**Pinned:**
- the collector's SHA-256 matches r5's (`fa718caa…`);
- analyzer SHA-256 `5260624808fbb4fe17f413e1439867ed02b2692089d881df7ec84f5585d618ab`;
- report `gluetun-final-20261106_235127Z.md`, which stays private on the owner's host.

**Results:**
- **Coverage:** 2.00 h of baseline, 100.09 % covered.
- **Stability:** 0 restarts and no VPN drops. The tunnel's exit address differed from the home address, as the leg requires; neither address is recorded here.
- **Crawling:** 1,316 torrents stored in the 2 h, with 0 writer errors and 0 search errors.
- **Memory:** private memory fell from 231 to 212 MiB, and the working set stayed under 1 GiB.

**Expected and not failures:**
- the 12–24 h memory rule doesn't apply to a 2 h leg (`too_few_samples`);
- "clean shutdown: no" follows from running without `--stop` (`left_running`, so `completion_unknown`), as the README says;
- the one error in the log was gluetun's own GitHub 504, not Lodestar's.

**Why it carries over:** rc.5 (#540) changed only the Legal Notice's text, and #558 changed only a docs page that isn't in the binary. So the rc.4 evidence carries over to the tagged build. The binary at `12cd25ca` differs from rc.5's only in its embedded commit hash.

## R-legal: the §2.48 check
- **The Legal Notice:** current. It is version 3, "Applies to Lodestar v0.5.1", and the embedded copy matches. `NOTICE_VERSION` stays 3.
- **The operator guidance:** every factual claim and every "(planned)" marker is true of v0.5.1. Its status line didn't name the release baseline, so R-legal-status (#558, merged into `release/v0.5.1` at 2026-11-07T00:18Z) changed three things:
  - the line now names "Lodestar v0.5.1, the same software as version 3 of the Legal Notice";
  - "or an integration" was removed, because v0.5.1 has no inbound integration endpoint;
  - the filters sentence now says API keys and feed tokens only read and can't reach it.

  The PR touched `docs/legal/operator-guidance.md` and its decisions file only.
- **For LEGAL-3:** `POST /settings/test` takes download-client credentials typed in for a test and doesn't store them. Describe that when the notice next changes.

## The tagged commit's checks
`12cd25ca` = `a0eb0a90` + #558 (docs only). Dispatched on it at 2026-11-07T00:19Z:
- **CI:** run 42317461111;
- **Release dry run:** run 42317463756;
- **E2E:** run 42317466554.

The runner sends the owner the go only once all three are green.

## After the tag
- **Build-plan R4:** bring `docs/releases/v0.5.1.md` to `main`, mark v0.5.0 as never released, and update the README.
- **R5:** the v0.5.2 cut (P-v052-cut), once its dependencies have merged. X-soak-screen (§2.50) lands before the v0.5.2 soak.

## Also in this runner PR
- **Owner answers recorded:** `docs/owner-decisions.md` §2.50 (the soak screen; the gate is unchanged) and §2.51 (the Unraid experiment host, DHT port not forwarded, one network setup for every crawler run).
- **build-plan §0:**
  - row C2 and the §0.5 run rules name the host and the fixed network setup;
  - rows R2, R-legal and R3b are updated as above.
- **`runner/slices.yaml`:**
  - the new slice X-soak-screen;
  - `owns` additions the runner approved during builds on 2026-11-07:
    - **X-sources-api-spec (00:29Z–00:30Z):** the seven plugin files, `tests/credential_spec.rs`, `store/src/unverified.rs`, `ops/tests.rs`, `api/tests/ops.rs` and the web fixtures;
    - **WK-5 (00:26Z):** `cluster/src/server.rs`, for `WorkerStatus.external_ips` only.
- **`runner/dashboard.yaml`:**
  - X-soak-screen's placement;
  - the experiment-host item now reads "book each run".
