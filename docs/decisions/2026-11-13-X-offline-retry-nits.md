# X-offline-retry-nits decisions (#844)

Date: 2026-11-13Z. Slice: X-offline-retry-nits. Follows `2026-11-13-X-offline-retry.md`; no owner number changes.

- **Tab title:** `+error.svelte` saves the page's title before it sets "Can’t reach Lodestar", and puts it back when that error is replaced (compared against the title it set, so a language change while offline doesn't strand it), only if nothing has set a title since (the next page sets its own). `ConnectionView` no longer sets a title; one place owns it.
- **Message module:** the error node uses `shell_fallback_offline_title`, as the file's own rule says (its messages stay in one chunk). The text is the same as `shell_state_offline_title`.
- **Tests:** the "nothing while probing" case now hangs the probe and waits for ErrorView to load; the 404-after-outage case uses fake timers and checks the retry timer is gone.
- **Not done:** the optional fake-clock version of the 16 s e2e wait. The real wait stays: the backoff interleaves network probes with timers, so a fake clock would need step-by-step `runFor` calls, while the real wait is a bounded condition wait.
