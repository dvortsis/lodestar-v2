# Offline CR-0 soak analysis

`analyze_cr0.py` reads the aggregate evidence written by `cr0-soak.ps1`.
It uses Python 3.10+ and the standard library; no packages or services are required.
It does not connect to the crawler, inspect its database, read process logs, or
change the collector or evidence. It works during startup and after termination.

From the checkout containing this tool:

```powershell
python docs/soak/tools/analyze_cr0.py C:/path/to/soak-run
python docs/soak/tools/analyze_cr0.py C:/path/to/soak-run --format json --output C:/path/to/new-report.json
python -m unittest discover -s docs/soak/tools -p test_analyze_cr0.py -v
```

Output defaults to Markdown on stdout. An explicit output path must be outside
the evidence directory and must not already exist. Exit status 2 means unreadable
or malformed evidence, or invalid arguments; status 0 means a report was produced,
**not that the soak passed**. Review `issues`, phase coverage, bounds and termination.

## Evidence contract

Required: `run.json` and `samples.jsonl`. Optional: `metrics/00000.prom`, etc.,
and `result.json` after termination. Snapshot filenames correspond to zero-based
JSONL row numbers. Preserve blank/missing records as errors; do not renumber files.
PowerShell UTF-8 BOMs and CRLF are supported. A concurrently appended incomplete
last JSONL record is reported and ignored; malformed complete records are errors.
Missing/invalid Prometheus snapshots are reported. The collector's valid scalar
JSON metrics remain available even when supplemental snapshots are unusable.

Only an allowlist of aggregate counters is read from Prometheus. Label series
are checked individually for resets, then aggregated; labels and unknown metric
names are never exported. The report exports only commit and binary/collector
hashes from run provenance, not paths, configuration, tokens, logs, hashes of
torrents, or peer addresses. Keep original process logs private.

## Interpretation

- Startup suppression, warm-up and baseline are distinct phases taken from the
  collector's recorded rows. Missing baseline data remains `not_started`.
- Time comes from collector monotonic `elapsed_seconds`, not local wall clocks.
  Rates use adjacent same-phase, same-process samples with unchanged server
  start time. A gap over 90 seconds is excluded by default (`--max-gap-seconds`).
  Missing metric intervals are excluded, never bridged or counted as zero.
- Every counter reports its own covered seconds. A reset, changed label set or
  missing counter invalidates that counter's interval. Rates describe covered
  intervals only; they cannot reconstruct work during gaps. Health failures and
  failed fetches are retained, including when valid metrics accompany failed health.
- RSS maximum includes every available process measurement. RSS change is
  first-to-last and is shown only for a continuous phase. Short startup trends
  and host memory pressure cannot establish a leak.
- Capacity equality is reported separately from exceeding a cap. These are
  sampled observations; they cannot prove that between-sample ownership was bounded.
- `writer_stored_total` measures newly stored torrents. It does not prove
  searchable yield, cohort latency, unique sampling coverage, byte efficiency,
  kernel packet drops or any improvement over another build.
- `result.json` records why the collector stopped. A completed collector window
  still requires review. Missing/forced shutdown, short evidence, gaps, resets
  and violated bounds remain visible. The untimed `final.prom` is deliberately
  excluded from rates; the final partial minute is not extrapolated.

The baseline must still be reviewed against the implementation plan and deployment
requirements. This tool does not authorize restarting the older Docker crawler.

## Collector failure rehearsal

Run these from the integration checkout in PowerShell 7:

```powershell
pwsh -NoProfile -File docs/soak/tools/test_cr0_collector.ps1
pwsh -NoProfile -File docs/soak/tools/test_cr0_rehearsal.ps1 -OutputRoot target/rehearsal-new
python -m unittest discover -s docs/soak/tools -p 'test_*.py' -v
```

Choose a new output directory each time. The integrated rehearsal copies the
production collector unchanged and appends test adapters to its private common
dependency. Its executable is an invalid text fixture, process and HTTP calls are
fake, and clock advancement is simulated. Never copy `rehearsal-adapters.ps1` into
a launch packet. Rehearsal evidence is test data, not elapsed soak coverage.

The 21 integrated cases cover preflight refusal; falling space in each phase;
repeated capacity-read failures; redirected-log growth; readable metrics with
unwritable evidence; status, result and informational-log failures; child exit;
signal failure and forced stop; stale process identity; failed cleanup-adapter
creation; health/metrics failures; an exact 24-hour endpoint; an unreadable endpoint
followed by a valid later sample; an unhealthy endpoint with valid metrics; and refusing restart into existing evidence.
The smaller injection suite separately exercises locked atomic replacement,
unique temporary files, independently guarded final writes and bounded waits.
Python fixtures cover malformed/truncated evidence, missing snapshots, resets,
label changes and collection gaps. No test fills the disk or deletes old evidence.

Real Windows Ctrl+C delivery is a separate `smoke_windows.ps1` check against a
fresh offline application fixture. The supervisor clears the inheritable
ignore-Ctrl+C flag before spawning the child. `cr0-signal.ps1` verifies PID/start
time, joins the child's console from a separate helper and records signal
generation; cleanup still requires confirmed process exit. A successful signal
alone does not establish graceful shutdown. Keep the signal helper alongside the
collector and common module in the frozen run directory.

The operator stop interface is a new empty `stop.request` file in the **new run's**
directory. It triggers protected shutdown and a failing collector exit because
collection is interrupted. Preserve the directory afterward; do not restart into
it. A stale identity or unconfirmed cleanup is an unknown process state requiring
operator investigation, never permission to kill a PID without checking its start
identity. Original logs and databases remain private.

## Preparing a source-qualified Windows package

After source review and the applicable validation gates, commit the integration
checkout locally. Keep unrelated root and Settings work separate. Then run:

```powershell
python docs/soak/tools/prepare_windows.py --output target/launch-candidate-r17
python docs/soak/tools/prepare_windows.py --output target/launch-candidate-r17 --verify
```

The output must be a new direct child of this checkout's `target`. Preparation
refuses dirty source, records the commit and tree, builds the frontend before
Rust, checks unchanged bundle budgets, freezes the UI and collector dependencies,
and exercises an isolated offline smoke. It uses existing offline dependencies,
four Cargo jobs and the bounded cache; it performs no cleanup or public crawl.
Source identity is checked again after building and after the smoke. Any failed
directory remains diagnostic evidence and must not be reused.

`build-record.json` binds the executable and index hashes to that source and
toolchain. `manifest.json` is written only after successful preparation and lists
every packaged executable, collector dependency and UI file hash. Verification
detects missing/modified files and inconsistent build identity. Keep the entire
directory private: the smoke's separate fixture logs can contain a setup token.
Only the aggregate manifest and smoke result are suitable for reporting.

The manifest is **packaging evidence**, not launch authorization or soak acceptance.
Recheck Docker stopped, available ports, storage growth and available space before
launching. Choose a separate, nonexistent run directory: the collector refuses
to use the package directory or any previous run as its evidence root. The frozen
collector's arguments are `-Exe <package>/lodestar.exe -Root <new-run> -Commit
<recorded-source-commit> -Ui embedded -BuildRecord <package>/build-record.json`.
Keep the default suppression, warmup, baseline duration and disk guardrails.
