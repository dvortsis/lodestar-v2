# X-retry-typed-flake decisions

- **The cause is in the test, not the product.** `chunk-load.routes.test.ts`'s "Retry pressed within 100 ms of typing" failed with `q=lanternharbor`. A probe logged keydown events, selection changes, editor mutations and the moment `Live.svelte` mounted.
  - The results alert appeared at 460 ms, and `failSearch` returned there.
  - Ctrl+A ran at 916 ms on a bar that was still empty: the static bar (§10.1) has no text until the lazy editor mounts.
  - The editor mounted at 926 ms and wrote the URL's query, with the caret at the end. The typing then went after it.
  - Nothing reset a selection, and the chrome's arrival played no part.
- **The fix: `failSearch` waits for the bar to show the URL's query** (`toHaveText('lantern')`, 10 s deadline) before it returns. Every Ctrl+A test after it (four in the file) then starts with the editor live. No sleep, no retry.
- **Why the product is fine:** the user sees an empty bar, then the query appear. Text typed before the editor loads already wins over the URL's query (`Live.svelte` onMount: `editor.text !== s.text` → `setText`). A new test pins this: it holds the editor's chunk, types `harbor` over `?q=lantern`, releases the chunk, and expects `harbor` in the bar and the URL. Only a select-all made on the still-empty bar is lost. It selected nothing visible.
- **Proof (local, `--workers=4 --retries=0`, repeat 60):**
  - with the wait: 60/60 and 60/60;
  - with it removed: 2/60 failed (`q=lanternharbor`), then 0/60.
  - The local rate is lower than CI's 10/30, which runs on slower machines.
- **Follow-up (runner, §2.26): X-querybar-static-q.** It writes the URL's `q` into the static bar at mount, so a pasted link shows its query before the editor loads. Measured at +16 B, which puts the Search route at 45,002 / 45,000. It must pay for those bytes within the route, and handle a `q` that contains a newline.
