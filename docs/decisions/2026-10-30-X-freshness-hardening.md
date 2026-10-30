# Freshness re-sign hardening

Runner, 2026-10-30. Follows #211 (the generation-0 freshness key and `.github/workflows/freshness.yml`), from that PR's review.

- **One writer at a time for `update-manifest`.** The re-sign job uses the concurrency group `update-manifest` with `cancel-in-progress: false`. **Contract for E-M6:** any workflow that uploads to the `update-manifest` release joins the same group. As a second guard, the job re-downloads each envelope just before uploading and skips a channel whose manifest changed while it signed. Without both, a re-sign could put an older manifest back, and clients that had already accepted the newer one would refuse it as older (`Error::Older`). A concurrency group holds one pending run, and a newer pending run cancels it. So a publish queued behind a re-sign can be cancelled by a later freshness dispatch: the publisher must check its run finished and re-run it if it was cancelled, and freshness is dispatched by hand only rarely.
- **A channel that can't be refreshed fails the run but doesn't block the others.** This covers a failed download, a missing public key (Nightly, until the owner creates its key) and a published manifest that doesn't verify. The other channels are still uploaded, then the run exits 1.
- **Failing loudly instead of passing silently** (owner-decisions §2.21: Stable manifests never lapse):
  - If envelopes are published and `FRESHNESS_SIGNING_KEY` is empty, the run is an error.
  - Release-listing and download errors (auth, network, rate limit) fail the run. Only a missing `update-manifest` release counts as "nothing published".
- **No SIGPIPE on a passwordless key.** The password is piped to `minisign` only when one is set.
- A failed run leaves the envelopes it didn't reach as they were. Re-run it by dispatch.
- The `minisign` package stays unpinned, as in `release.yml`. If it's pinned later, pin both workflows together.
