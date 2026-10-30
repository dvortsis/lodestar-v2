// The edit chunk: what only an edit, a save, a stored draft or a change elsewhere needs. The save
// pipeline (Undo, the merge, reading and restoring drafts, validation), the form's questions and
// the UI they show. `form.svelte.ts` loads it once per page (owner, 2026-10-24).
export * from './saving';
export { sectionValues } from './merge';
export { validate } from './validate';
export { askDisruptive, askToLeave } from './questions';
export { default as DraftBanner } from './DraftBanner.svelte';
export { default as ErrorSummary } from './ErrorSummary.svelte';
export { default as FieldConflict } from './FieldConflict.svelte';
export { default as FieldSaved } from './FieldSaved.svelte';
export { default as FormNotice } from './FormNotice.svelte';
export { default as SaveBar } from './SaveBar.svelte';
