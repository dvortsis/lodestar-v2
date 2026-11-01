// The one door onto the form engine for screens that embed it outside Settings' own routes: the
// Settings section form (`shell/SectionForm.svelte`) and the first-run wizard (§10.10.10) import
// the engine only from here. Rolldown groups modules by the entries that reach them, so a second
// consumer reaching a subset of the engine through other paths would split its chunk and grow every
// Settings first view; going through one module keeps one engine graph (docs/decisions/2026-11-01-S-wizard-embed.md).
export { loadControls } from './controls.svelte';
export { focusSettingField } from './focus';
export { loadMessages, messagesLoaded } from './messages';
export { widgetFor } from './widgets';
export { createSettingsForm, groupFields, settingText } from './index';
export type { FieldKey, FieldSpec } from './index';
export { default as SettingsForm } from './SettingsForm.svelte';
export { default as SettingField } from './SettingField.svelte';
