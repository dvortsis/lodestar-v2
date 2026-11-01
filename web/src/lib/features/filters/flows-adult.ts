// What the flows door's `loadAdult` loads (X-flows-door-2): Appearance's 18+ confirmation and the
// switch's save, behind one dynamic import. Nothing imports this module except the door.
export { createAdultConfirm, setShowAdult } from './flows/adult.svelte';
export { default as AdultConfirmDialog } from './flows/AdultConfirmDialog.svelte';
