# X-web-followups-1101f: Discover keys notice, first-visit scene, QueryBar flake

Runner decision (§2.26), reversing the cost-limited skips in `2026-11-01-X-web-polish-1101e.md` for #317 and #322.

- **#322 Discover `j`/`k`.** If the after-mount import of the key registry or `./commands` fails, an info toast says shortcuts are unavailable and a reload retries. No retry in code (costs more bytes). Test: `DiscoverPage.keys.test.ts` forces the import to reject.
- **#317 Scene on a first visit.** The scene still renders with no cache, so first-run empty states are unchanged. `app.css` now carries one CSS rule (in the component it pushed the index-state banners entry over its limit): once `applyLook` sets `data-illustrations="false"` (the server's answer), the picture is `visibility: hidden` and keeps its space, so nothing jumps and there is no motion to reduce. Covered by an e2e in `illustrations.routes.test.ts`.
- **QueryBar flake** ("Syntax popover > opens on its button, and Try sets and submits"). Did not reproduce in 10 cold runs (21/21 each). Nothing changed.
