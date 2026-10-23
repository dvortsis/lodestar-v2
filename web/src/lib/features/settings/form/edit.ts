// Everything after a section's first view, as one chunk: the save pipeline, Undo, the merge,
// drafts and validation (`saving.ts`), and the parts of the UI that only an edit, a save or a
// conflict shows. `form.svelte.ts` loads it with the first edit, a stored draft, a `settings`
// event or a save; the components take it from there. One entry keeps it one download.
export * from './saving';
export { sectionValues } from './merge';
export { validate } from './validate';
export { askDisruptive, askLeave } from './questions';
export { default as ErrorSummary } from './ErrorSummary.svelte';
export { default as FieldConflict } from './FieldConflict.svelte';
export { default as FieldSaved } from './FieldSaved.svelte';
export { default as FormNotice } from './FormNotice.svelte';
export { default as SaveBar } from './SaveBar.svelte';
