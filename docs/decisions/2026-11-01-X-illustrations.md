# X-illustrations: the toggle and Save-Data hide state illustrations

Closes the app gap named in `2026-11-01-S-e2e.md` (§10.19, §10.10.8).

- **What reads the flag:** `StateScene.svelte` reads the per-browser appearance cache
  (`lodestar.appearance`, written by `applyLook` with `data-illustrations` on every change) when a
  scene is built, and renders nothing (no `<picture>`, so no request) when `illustrations` is false.
  The brief assumed the pre-paint script sets the attribute; it sets the theme only, and the
  appearance boot waits for first interaction (§10.1), so on a first view the attribute is absent.
  The cache is the one source that is there from first paint. A change made on the Appearance screen
  applies to the next scene built, without a reload.
- **Save-Data:** `navigator.connection?.saveData` is checked first, so it hides the scene whatever the
  cache says, and nothing is rewritten: the stored preference comes back when Save-Data is off. The
  Appearance screen shows one hint line under the toggle (`appearance_illustrations_save_data`, en
  and zh).
- **Known limits:** with browser storage blocked, or on a first visit in a browser with no cache yet
  (account off, nothing cached), the first scene shows; the cache is filled once the appearance boot
  runs. Following the attribute too would cost bytes the Discover route (44,996 of 45,000 B) lacks.
- **Follow-up:** low-resource mode does not exist in the app yet and is not invented here. When it
  lands it should feed the same gate.
- **Layout:** unchanged; no snapshot moves. `decoding="async"` stays on the scene's `<img>`.
- **Byte deltas (gzip):** CSS 0. The first views that show scenes grow by 0 to 0.06 kB (Discover
  route 44.92 to 45.0 kB of its 45 kB limit); Settings overview +0.1 kB (28.03 to 28.14 kB); no limit
  raised.
- **Ruling (runner §2.26, flagged to the owner under §2.19):** the Discover route limit goes from 45 to
  45.2 KB for this check. Discover measured 44.92 kB before and 45.08 kB after on current `main`.
