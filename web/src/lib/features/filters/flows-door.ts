// The one door into `flows/**` for every screen that starts a filters change (S-filters-2c):
// Settings → Filters & Safety, a result's More menu and the detail drawer. Import this module with
// `import()`, then call `load()`. Two hops on purpose:
// - this module holds the preload list of the flows' chunk (the dialogs and Bits UI's overlay
//   code), so a view that can open a flow pays one small dynamic import, not the list;
// - `flows-root.ts` is the single dynamic-import root of the flows, so the bundler keeps them and
//   the overlay code in one on-demand chunk instead of regrouping them with a first view.
// Type-only imports from `flows/**` are fine anywhere; values go through here.
export const load = () => import('./flows-root');
