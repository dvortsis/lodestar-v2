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
