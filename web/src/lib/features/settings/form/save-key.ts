// ⌘/Ctrl+S in the shortcut registry for a form (§10.10.5), loaded with the registry itself,
// so neither is in a section's first view (W9). Returns the unregister.
import { m } from '$lib/paraglide/messages.js';

export async function registerSaveKey(id: string, save: () => void): Promise<() => void> {
	const { keys } = await import('$lib/keys');
	const scope = `settings-form:${id}`;
	const pop = keys.pushScope(scope);
	let unregister: (() => void) | undefined;
	try {
		unregister = keys.register({
			id: `settings.save.${id}`,
			title: () => m.settings_form_save(),
			keys: ['mod+s'],
			scope,
			allowInInput: true,
			run: save
		});
	} catch {
		// Another form on the page registered first; this one still saves from its own keys.
	}
	return () => {
		unregister?.();
		pop();
	};
}
