// The Security screen's password confirmation (§10.10.7, B12): one dialog for every risky change.
// `confirmWithPassword` loads it on first use, and `send` gets the typed password once per try.
// Nothing here keeps it: the dialog clears its field before `send` runs, and the result is only
// what `send` returns. Cancel, Esc and ✕ resolve with null and send nothing.
import { mount, unmount } from 'svelte';
import { ApiError } from '$lib/api/client';
import { m } from '$lib/paraglide/messages.js';

export interface PasswordQuestion<T> {
	title: string;
	body: string[];
	/** The confirming button's label, which names the change. */
	action: string;
	/** Runs the change with the password; a thrown `ApiError` is shown in the dialog. */
	send: (password: string) => Promise<T>;
}

/** The login bucket refills one try every 12 s and its 429 has no `Retry-After` (M-auth). */
export const DEFAULT_WAIT_S = 12;
/** A `Retry-After` beyond this is shown as this (M-auth's cap). */
const MAX_WAIT_S = 15 * 60;

/** What a refused try shows. `lockedFor` (seconds) is set for the lock-out (429). */
export interface Refusal {
	message: string;
	lockedFor?: number;
	/** The session ended: the API client is already taking the user to sign in. */
	signedOut?: boolean;
}

export function refusal(e: unknown, fallback: string): Refusal {
	if (!(e instanceof ApiError)) return { message: fallback };
	if (e.status === 401 && e.code === 'login_required') return { message: '', signedOut: true };
	if (e.status === 429)
		return {
			message: e.message || fallback,
			lockedFor: Math.min(MAX_WAIT_S, Math.max(1, e.retryAfter ?? DEFAULT_WAIT_S))
		};
	return { message: e.message || fallback };
}

/** One try as the dialog sees it: the change's result, or why it was refused. */
export type Outcome = { value: unknown } | { refused: Refusal };

/** Long enough for the closing animation and focus restore before the dialog is removed. */
const UNMOUNT_MS = 400;

const open = new Set<() => void>();

/** Closes every open confirmation unanswered, its typed password with it (the screen unmounted). */
export function dismissPasswordDialogs(): void {
	for (const close of [...open]) close();
}

export async function confirmWithPassword<T>(q: PasswordQuestion<T>): Promise<T | null> {
	const { default: PasswordDialog } = await import('./PasswordDialog.svelte');
	return new Promise<T | null>((resolve) => {
		const dismiss = () => {
			open.delete(dismiss);
			resolve(null);
			void unmount(app);
		};
		const app = mount(PasswordDialog, {
			target: document.body,
			props: {
				title: q.title,
				body: q.body,
				action: q.action,
				// Errors become a `Refusal` here, so the dialog's chunk imports nothing of the screen's.
				send: (password: string): Promise<Outcome> =>
					q.send(password).then(
						(value) => ({ value }),
						(e: unknown) => ({ refused: refusal(e, m.settings_security_failed()) })
					),
				onclose: (result: { value: unknown } | null) => {
					if (!open.delete(dismiss)) return;
					resolve(result ? (result.value as T) : null);
					setTimeout(() => void unmount(app), UNMOUNT_MS);
				}
			}
		});
		open.add(dismiss);
	});
}
