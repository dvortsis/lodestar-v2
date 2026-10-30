# Bugbot gap-review evidence

These are standalone diagnostic probes against the reviewed production crates, not application fixes or newly installed workspace tests.

- [Storage probe](storage-regressions.rs) and [observed output](storage-regressions-output.txt): three assertions fail as expected, covering snapshot purge, verified name replacement and raw archiving.
- [Search probe](search-cache-cancellation.rs) and [observed output](search-cache-cancellation-output.txt): pre-cancelled cold file queries still retain file-ID caches. Its allocator measures requested live allocation bytes, not OS RSS. The 8 MiB assertion is a diagnostic threshold, not an existing file-cache configuration guarantee. The allocator wrapper uses standalone instrumentation outside the workspace's application crates.
- [API jobs test log](api-jobs-tests.txt): 8 passed.
- [API events test log](api-events-tests.txt): 22 matching tests passed, including log-event tests.

Both probe executables returned exit code **101**, because the expected behavior does not hold. The parent reran them independently after the reviewer produced them. All inputs and databases are synthetic; the storage probe's snapshot copy uses `VACUUM INTO` in an automatically cleaned temporary directory.

## Reproduction on this Windows checkout

Build the reviewed libraries first:

```powershell
cargo test --locked --offline --release -p lodestar-store -p lodestar-search -p lodestar-pipeline
```

Compile the saved source files against that build. Paths below select the most recently compiled dependency artifact, as used during review. Use the same checkout/toolchain for compilation and execution.

```powershell
$probeEvidence = (Resolve-Path 'docs/code-review/evidence/bugbot-v0.5-2026-10-24').Path
$probeDeps = (Resolve-Path 'target/release/deps').Path
$probeScratch = Join-Path ([IO.Path]::GetTempPath()) ('lodestar-review-probes-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $probeScratch | Out-Null
function ReviewLib($pattern) {
    (Get-ChildItem -LiteralPath $probeDeps -Filter $pattern |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1).FullName
}
$probeStore = ReviewLib 'liblodestar_store-*.rlib'
$probeInfo = ReviewLib 'liblodestar_metainfo-*.rlib'
$probeTemp = ReviewLib 'libtempfile-*.rlib'
$probeSearch = ReviewLib 'liblodestar_search-*.rlib'

rustc --edition=2024 --test (Join-Path $probeEvidence 'storage-regressions.rs') `
    -L "dependency=$probeDeps" --extern "lodestar_store=$probeStore" `
    --extern "lodestar_metainfo=$probeInfo" --extern "tempfile=$probeTemp" `
    -o (Join-Path $probeScratch 'storage.exe')
& (Join-Path $probeScratch 'storage.exe') --nocapture

# The standalone Search link also needs the cached Windows import library.
# This is a read-only lookup; it downloads or installs nothing.
$probeNative = (Get-ChildItem (Join-Path $env:USERPROFILE '.cargo/registry/src') `
    -Recurse -Filter 'windows.0.52.0.lib' | Select-Object -First 1).DirectoryName
rustc --edition=2024 (Join-Path $probeEvidence 'search-cache-cancellation.rs') `
    -L "dependency=$probeDeps" -L "native=$probeNative" `
    --extern "lodestar_search=$probeSearch" --extern "lodestar_store=$probeStore" `
    --extern "lodestar_metainfo=$probeInfo" -o (Join-Path $probeScratch 'search.exe')
& (Join-Path $probeScratch 'search.exe')
```

The original Search compilation selected the x64 MSVC `windows_x86_64_msvc-0.52.6/lib` directory. A different target/toolchain may require its matching import-library path. No Linux updater reproduction is included; that finding is explicitly source-verified in the report.
