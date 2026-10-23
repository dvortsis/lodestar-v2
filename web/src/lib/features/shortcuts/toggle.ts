// The palette's "Turn off single-key shortcuts" and the sheet's "Turn on": one field, no
// `if_revision` (a single-field toggle). Loaded when used, so boot.ts stays small.
import { api, isAbort } from '$lib/api/client';
import { keys } from '$lib/keys';
import { m } from '$lib/paraglide/messages.js';
import { toast } from '$lib/ui/feedback/toasts.svelte';
import { SECTION, applyPrefs, fromFields } from './boot';

export async function toggleSingleKeys(): Promise<void> {
	const next = !keys.singleKeys;
	try {
		const res = await api.patch('/settings/{section}', {
			...SECTION,
			body: { shortcuts: next } as unknown as Record<string, never>
		});
		const prefs = fromFields(res.fields);
		if (prefs) applyPrefs(prefs);
		else keys.setSingleKeys(next);
		toast.push({ text: next ? m.shortcuts_toast_on() : m.shortcuts_toast_off() });
	} catch (e) {
		if (!isAbort(e)) toast.push({ kind: 'error', text: m.shortcuts_toast_failed() });
	}
}
