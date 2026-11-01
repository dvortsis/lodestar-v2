// The one door into `flows/**` for every screen that starts a filters change (S-filters-2c):
// Settings → Filters & Safety, a result's More menu, the detail drawer and the wizard's pack step;
// and for Appearance's 18+ confirmation (X-flows-door-2). Import this module with `import()`, then
// call `load()` or `loadAdult()`. Two hops on purpose:
// - this module holds the preload lists of the flows' chunks (the dialogs and Bits UI's overlay
//   code), so a view that can open a flow pays one small dynamic import, not the lists;
// - `flows-root.ts` and `flows-adult.ts` are the flows' only dynamic-import roots, so the bundler
//   keeps them and the overlay code in on-demand chunks instead of regrouping them with a first
//   view.
// Type-only imports from `flows/**` are fine anywhere; values go through here.
export const load = () => import('./flows-root');

/** "Show adult content" (§20.9): the 18+ dialog and the switch's save, loaded on its first change. */
export const loadAdult = () => import('./flows-adult');
