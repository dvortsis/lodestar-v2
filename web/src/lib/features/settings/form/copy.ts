// The toast after a copy to the clipboard (the field's ⋯ menu and status line). Loaded on
// first use; the write itself runs in the click, where browsers allow it.
import { toast } from '$lib/ui/feedback/toasts.svelte';
import { m } from '$lib/paraglide/messages.js';

export async function toastCopy(done: Promise<void>, what: 'value' | 'link'): Promise<void> {
	try {
		await done;
		toast.push({
			kind: 'success',
			text: what === 'link' ? m.settings_form_link_copied() : m.settings_form_copied(),
			timeout: 3000
		});
	} catch {
		toast.push({ kind: 'error', text: m.settings_form_copy_failed() });
	}
}
