// The Settings form engine (§10.10). Screens and the wizard import from here; components
// (`SettingField.svelte`, `SettingsForm.svelte`) by path.
export { createSettingsForm, UNDO_MS, DEBOUNCE_MS } from './form.svelte';
export type { SettingsForm, SettingsFormOptions } from './form.svelte';
export {
	loadSettingsIndex,
	groupFields,
	fieldAnchor,
	type SettingsIndexView,
	type FieldGroup
} from './schema';
export { settingText, fieldLabel, fieldHelp } from './messages';
export { registerWidget, widgetFor } from './widgets';
export type { WidgetProps, WidgetChoice, BuiltinWidget, FieldControlProps } from './widgets';
export type {
	Danger,
	FieldKey,
	FieldKind,
	FieldSpec,
	FieldState,
	GateContext,
	SaveResult,
	SettingSource
} from './types';
