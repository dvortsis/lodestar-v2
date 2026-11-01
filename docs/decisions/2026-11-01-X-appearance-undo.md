# X-appearance-undo decisions (Appearance Undo fixes)

Found while reviewing FX-3 (#346); follows `2026-11-01-FX-3.md`, which fixed the same gaps on Keyboard.

- **Undo belongs to the latest change.** A new change hides any Undo on screen, and Undo is offered only once nothing later is still saving. Before, change A finishing while B was still saving showed "Saved. Undo", and Undo reverted A. Undoing is a change of its own and isn't itself undoable (replaces the old `.then(() => (undo = null))`).
- **Undo is hidden when the field it would change is locked** (the locks re-read after a stale revision).
- **A later save that succeeds clears the error banner** left by an earlier failed one.
- **A 2xx reply that can't be read** (no numeric `revision`) re-reads the section and shows no error, since the save may have landed. `prefs.save` throws `Unreadable` for it.
- **A re-read keeps queued changes.** A stale revision or an unreadable 2xx re-reads the section mid-queue; changes still waiting to save are laid back over it, so the screen doesn't show the old value of a change that then saves.
- **Focus after Undo** moves to the control the change was made with: the checked segment for mode, density and motion, the checked theme tile, the Dark theme select (or the "different dark theme" switch), or the illustrations switch. If that control isn't shown (a filtered-out tile), it falls back to the picker's filter.
- **Tests:** the two 18+ dialog tests get a 20 s test budget, since their 10 s waits outlasted vitest's 5 s default on a cold run.
- **Undo restores only what the server had** (runner's review). The screen keeps `saved`, what the server last confirmed, apart from `prefs`, as Keyboard does. Undo offers the change's fields as `saved` held them, and a failed save puts back `saved`'s value, so an earlier change that failed is never brought back. This bug predates the slice.
- **ThemePicker's "different theme in dark mode" switch follows `theme_dark`** as it changes, not only on mount, so undoing turning it off turns it back on and focus lands on the Dark theme select.
