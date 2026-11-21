# X-web-build-determinism (brief, runner 2026-11-21)

## Problem (runner measurements, 2026-11-21Z)
The web build is not byte-stable. Identical source gives size-limit numbers that differ between
builds by 5–20 B gzip:
- Search results (lazy): 40,821 / 40,827 / 40,832 B
- Search cards (lazy): 7,992 / 7,998 / 8,012 B
- Filters pack panels: 7,908 / 7,910 / 7,913 / 7,916 B

The measurements come from the CI flow: `npm run build`, then
`LODESTAR_BUILD_VERSION=0000000 npx vite build`, then `npx size-limit`. Limits sitting at their
measured size flake: #1028, #1031 and #1033 each hit a few-byte failure on a chunk they didn't touch.

## Do
1. Find the source. Build the same tree several times (clean `.svelte-kit`/`build` and output each
   time) and diff the output files byte for byte: which chunks differ, and how (chunk order, hash
   names, module order inside a chunk, CSS order, a timestamp or version string, minifier name
   mangling, parallelism in the Svelte/Vite plugins). Vite 8.3.5, SvelteKit 2.70.3; see
   web/vite.config.ts and web/svelte.config.js.
2. Make the output byte-stable with the smallest config change that does it, with the reason in
   your decisions file. No new dependency without telling the runner first. Don't change chunking
   strategy in a way that grows any route (the 45 kB route cap and the owner's smallest-UI priority
   hold). Send the runner before/after numbers for every size-limit entry before pushing.
3. Add a CI guard: build twice in the web job and fail if any output file differs. Keep it cheap
   (reuse the second build CI already does if you can).
4. Don't edit web/.size-limit.mjs; #1033 owns it right now. If stable output lets limits come
   down, list the proposed numbers in the decisions file; the runner lands them later.
5. Owns: web/vite.config.ts, web/svelte.config.js, web/scripts/check-build-stable.* (new),
   .github/workflows/web.yml (the build/size steps only), your decisions file, your own
   slices.yaml entry and a dashboard.yaml placement rule (ws-web). Run
   `cd tools/status-connector && npm test`.
6. Gates: npm run check, npx vitest run, npm run build, npx size-limit, and playwright core for
   boot-shell and a route smoke, to show the built app still loads.
