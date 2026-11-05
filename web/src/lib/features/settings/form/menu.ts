import type { SettingsForm } from './form.svelte';
import type { FieldKey } from './types';
// The field menu and what it shows, as one chunk loaded when a menu is first opened.
export { default as FieldKeys } from './FieldKeys.svelte';
export { default as FieldMenu } from './FieldMenu.svelte';

/** What a menu choice does: stage a reset, copy the field's link, or show its config keys. */
export function selectMenu(
	id: string,
	form: SettingsForm,
	key: FieldKey,
	copy: (text: string, what: 'link') => void,
	toggleKeys: () => void
): void {
	if (id === 'reset') form.reset(key);
	else if (id === 'link') copy(new URL(form.href(key), location.href).href, 'link');
	else if (id === 'keys') toggleKeys();
}
