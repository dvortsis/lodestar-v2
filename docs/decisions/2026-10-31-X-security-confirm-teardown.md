# X-security-confirm-teardown decisions

A lifecycle fix with no change to any owner decision.

- **A closed dialog's pending unmount is now reachable.** `security/confirm.ts` and `security/reveal.ts` removed a dialog from `open` on close, then unmounted it on a 400 ms timer that `dismissPasswordDialogs()` / `dismissReveals()` could not see. Under load the timer fired after the screen, or jsdom, was gone (`ReferenceError: document is not defined` on #298, run 41248623418). Each now keeps a `closing` set of flush functions; dismissing clears the timer and unmounts at once.
- **The 400 ms delay is unchanged** on the normal path, so the closing animation and focus restore still work.
- **`reveal.ts` had the same copy of the pattern** and is fixed the same way. `form/dialogs.ts` was already safe: its timer only unmounts entries still in `open`, which `dismissAll()` empties (#296).
- **Tests:** `teardown.test.ts` checks that dismissing leaves no timer and no dialog; `security.test.ts` dismisses all three kinds in `afterEach`.
