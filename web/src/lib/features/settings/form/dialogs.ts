// Asks one question in a modal (ConfirmDialog.svelte, loaded on first use) and resolves with
// the answer's id, or null when it's closed with Esc or ✕.
import { mount, unmount } from 'svelte';
import type { ConfirmAction } from './ConfirmDialog.svelte';

export interface Question {
	title: string;
	body: string[];
	actions: ConfirmAction[];
	initial?: string;
}

/** Long enough for the closing animation and focus restore before the dialog is removed. */
const UNMOUNT_MS = 400;

export async function ask(q: Question): Promise<string | null> {
	const { default: ConfirmDialog } = await import('./ConfirmDialog.svelte');
	return new Promise((resolve) => {
		const app = mount(ConfirmDialog, {
			target: document.body,
			props: {
				...q,
				onclose: (id: string | null) => {
					resolve(id);
					setTimeout(() => void unmount(app), UNMOUNT_MS);
				}
			}
		});
	});
}
