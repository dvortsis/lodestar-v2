# X-detail-headroom decisions

The Detail drawer, `/t/[hash]` and Detail sections sat within bytes of their limits (29 KB, 22.6 KB, 34.5 KB). S-filters-2c needs about 0.3 KB on the drawer and `/t`. Runner ruling, 2026-11-01 (§2.26): move the event client off the first views into on-demand code, and settle the limits on the measured result. The runner reviews this as a performance/UX change to owner-set numbers.

- **The settings subscription loads on demand.** `visibility.ts` keeps `watchDetailVisibility()` (same callers: model, open, prefetch) but now imports `visibility-events.ts`, which holds the `events.on('settings', …)` subscription. The event client and SSE (about 2.3 KB gzip) are no longer in the drawer's or `/t`'s first view. The subscription starts when that chunk loads, right after mount, instead of at module load; the request is issued during mount and nothing waits on it, so first paint is unchanged and no request is added in front of it. If the load fails, the next `watchDetailVisibility()` call retries. The subscription now exists one chunk round trip after mount (not a few milliseconds), and if that load fails, until the next open, load or prefetch (browsers may cache a failed module fetch, so the retry can fail too). A content-settings event in that window does not clear the caches. The safeguard is that every open revalidates `GET /torrents/{hash}` before showing anything (detail README), and prefetched data never authorizes display; resyncs also clear the caches, as before. `.size-limit.mjs` lists `visibility-events.ts` in `detailLazy`, so Detail sections gates it.
- **`detail.css` is split so each entry carries only what it uses:** `host.css` (drawer host only), `page.css` (`/t` only) and `raw.css` (the raw viewer, used only by a dev route). About 35–45 B per entry.
- **Magnet copy/open shortcuts toast on a failed chunk load** (the #308 follow-up): `detail/commands.ts` (`c`, `o`) shows the "Couldn't get the magnet link." error toast, and `RowActions.svelte`'s copy button shows the same "Couldn't get the magnet link." message. About 0.17 KB in sections, and 0.18 KB in Search results (the message joins that chunk; headroom there is now 351 B, limit unchanged).
- **Not done:** `ui/status.ts` loads four icons for the flag badge's single one. `lib/ui` is shared with Alert and Search, so it's a follow-up (runner).
- **Tried, no effect:** importing formatters from their own modules instead of the `$lib/format` barrel (identical bytes, reverted).
- **Limits** (runner, §2.26): drawer 29 → 27.2 KB, `/t` 22.6 → 20.8 KB (never raised: measured + the 0.5 KB headroom target, rounded up to 0.1 KB; measured + 10% of the cut would be 26.9 and 20.5 KB, so the rounding is the looser rule), Detail sections 34.5 → 37.3 KB. The moved code compresses worse on its own, so sections grow by 0.25 KB more than the first views shrink; the toasts add 0.17 KB (this is the measured figure). Headroom is therefore 514 B (drawer), 472 B (`/t`, 28 B under the 0.5 KB target) and 161 B (sections, 39 B under the 0.2 KB target).
- **Measured** (bytes, gzipped; `npx size-limit --json`; local builds; two runs identical):

  | Entry                       | `main` | This branch | Limit before → after |
  | --------------------------- | ------ | ----------- | -------------------- |
  | Detail drawer               | 28,899 | 26,686      | 29 → 27.2 KB         |
  | /t/[hash] route             | 22,539 | 20,328      | 22.6 → 20.8 KB       |
  | Detail sections (on demand) | 34,458 | 37,139      | 34.5 → 37.3 KB       |
  | Drawer + sections           | 63,357 | 63,825      |                      |
  | /t + sections               | 56,997 | 57,467      |                      |
  | Search results (lazy)       | 43,288 | 42,766      | 43.3 KB (unchanged)  |

  First views fall by about 2.2 KB each; opening the sections costs about 2.7 KB more, loaded after paint. The drawer-plus-sections total grows by 468 B.
