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

const open = new Set<{ app: object; resolve: (id: string | null) => void }>();

/** `signal` closes the question unanswered (null), e.g. when the form that asked unmounts. */
export async function ask(q: Question, signal?: AbortSignal): Promise<string | null> {
	if (signal?.aborted) return null;
	const { default: ConfirmDialog } = await import('./ConfirmDialog.svelte');
	if (signal?.aborted) return null;
	return new Promise((resolve) => {
		const entry = { app: {}, resolve };
		entry.app = mount(ConfirmDialog, {
			target: document.body,
			props: {
				...q,
				onclose: (id: string | null) => {
					resolve(id);
					setTimeout(() => {
						if (open.delete(entry)) void unmount(entry.app);
					}, UNMOUNT_MS);
				}
			}
		});
		open.add(entry);
		signal?.addEventListener(
			'abort',
			() => {
				if (!open.delete(entry)) return;
				resolve(null);
				void unmount(entry.app);
			},
			{ once: true }
		);
	});
}

/** Closes every open question unanswered (tests, and a screen torn down mid-question). */
export function dismissAll(): void {
	for (const entry of open) {
		open.delete(entry);
		entry.resolve(null);
		void unmount(entry.app);
	}
}
