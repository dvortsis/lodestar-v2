// Sign out of this browser, for every place that offers it (the overflow menu, Settings →
// Security → Sessions, `LogoutButton`). Success lands on the sign-in page (`logout`); a failure
// leaves the session as it was and says why in the shell's toast.
import { m } from '$lib/paraglide/messages.js';
import { toast } from '$lib/ui/feedback/toasts.svelte';
import { logout } from './session.svelte';

let pending: Promise<boolean> | undefined;

/**
 * Resolves to whether the session ended. Never rejects. A second call while one is in flight
 * shares it, so a repeated press never sends a second `POST /logout`.
 */
export function signOut(): Promise<boolean> {
	pending ??= logout()
		.then(
			() => true,
			() => {
				toast.push({ kind: 'error', text: m.auth_logout_failed() });
				return false;
			}
		)
		.finally(() => (pending = undefined));
	return pending;
}
