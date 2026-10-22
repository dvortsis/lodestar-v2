# lib/state

Cross-feature client state (Svelte 5 runes in `.svelte.ts` modules): URL-derived search state, preferences, the SSE event feed.

- `search.svelte.ts`: the URL holds `q` (the server's canonical text), `sort`, `hidden`, `exact`; other parameters are kept. Typing replaces the history entry, submit and option changes push one. Call `search.sync()` from an effect on `page.url`.
- `events.svelte.ts`: one `/events` stream per tab via `events.on(topic, handler, { onResync })`; rules are in the file header.
- `sse.ts`: the `text/event-stream` parser.
