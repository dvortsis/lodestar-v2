# X-appearance-undo decisions (Appearance Undo fixes)

Found while reviewing FX-3 (#346); follows `2026-11-01-FX-3.md`, which fixed the same gaps on Keyboard.

- **Undo belongs to the latest change.** A new change hides any Undo on screen, and Undo is offered only once nothing later is still saving. Before, change A finishing while B was still saving showed "Saved. Undo", and Undo reverted A. Undoing is a change of its own and isn't itself undoable (replaces the old `.then(() => (undo = null))`).
- **Undo is hidden when the field it would change is locked** (the locks re-read after a stale revision).
- **A later save that succeeds clears the error banner** left by an earlier failed one.
- **A 2xx reply that can't be read** (no numeric `revision`) re-reads the section and shows no error, since the save may have landed. `prefs.save` throws `Unreadable` for it.
- **Focus after Undo** moves to the control the change was made with: the checked segment for mode, density and motion, the checked theme in the picker (or Browse when it's closed), or the illustrations switch.
- **Tests:** the two 18+ dialog tests get a 20 s test budget, since their 10 s waits outlasted vitest's 5 s default on a cold run.
- **Left as is:** Appearance doesn't yet have Keyboard's per-change delta for Undo, so if an earlier change to the same field failed while a later one was queued, Undo can restore the failed value. That needs Keyboard's `saved`/`prefs` split and is out of this slice; flagged to the runner.
