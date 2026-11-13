# X-offline-retry-nits decisions (#843)

Date: 2026-11-13Z. Slice: X-offline-retry-nits. Follows `2026-11-13-X-offline-retry.md`; no owner number changes.

- **Tab title:** `+error.svelte` saves the page's title before it sets "Can’t reach Lodestar", and puts it back when that error is replaced, only if nothing has set a title since (the next page sets its own). `ConnectionView` no longer sets a title; one place owns it.
- **Message module:** the error node uses `shell_fallback_offline_title`, as the file's own rule says (its messages stay in one chunk). The text is the same as `shell_state_offline_title`.
- **Tests:** the "nothing while probing" case now hangs the probe and waits for ErrorView to load; the 404-after-outage case uses fake timers and checks the retry timer is gone.
- **Not done:** the optional 16 s e2e wait stays real. Faking the page clock would also freeze SvelteKit's own timers, which makes the spec depend on them.
