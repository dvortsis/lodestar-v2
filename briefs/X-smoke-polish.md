# X-smoke-polish (Sonnet, helper, /security-review on the release.yml diff): the PK-smoke review nits

Assigned by the runner on 2026-11-11 to the session that built PK-smoke (#683, merged as 503a1575). The review is `reviews/rv-683-df67406e.md`.

## Do
1. **The password wording.** Fix the "password kept" claims: only the session cookie is kept. They're in flow.py:14, release.yml:181, the smoke.sh comment and the PK-smoke decisions file.
2. **Cleanup.** Remove Docker's `$SMOKE_STATE` (log.txt holds the raw token) and the fixture folder after the Container-logs step, with `if: always()` semantics inside the smoke job.
3. **Localhost only.** flow.py uses `urllib.request.build_opener(urllib.request.ProxyHandler({}))`, so requests never follow HTTP_PROXY.
4. **Redaction on Windows.** Apply the same token redaction to the older Windows log dumps: smoke.ps1:229, ~268, and Test-Serve's early-exit message at :64.
5. **Small cleanups:**
   - Test-Flow sets `LODESTAR_DHT_ENABLED='true'` explicitly;
   - drop the unused argument to smoke.sh's `stop_server`;
   - rename package-linux's step "Smoke (unpack, serve, /healthz, stop)" to what it now does;
   - add a line to packaging/windows/README.md saying the smoke needs Python.

## Owns
- `.github/workflows/release.yml`: smoke job and the package-linux smoke step name only. The same rule as PK-smoke: no build, sign, minisign, attest, publish, permissions, environment, secrets or `on:` changes.
- `packaging/linux/smoke.sh`, `packaging/windows/smoke.ps1`, `packaging/smoke/**`, `packaging/windows/README.md`.
- `docs/decisions/2026-11-11-PK-smoke.md` (the wording fix).

## Bar
- bash -n, shellcheck -S warning, py_compile.
- The smoke jobs and ci-ok green.
- Branch `slice/X-smoke-polish`, PR `X-smoke-polish: <title>`, steward skill.
- Reply "ready" with the head when CI is green.
