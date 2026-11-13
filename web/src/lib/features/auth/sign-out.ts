// Sign out of this browser, for every place that offers it (the overflow menu, Settings →
// Security → Sessions, `LogoutButton`). Success lands on the sign-in page (`logout`); a failure
// leaves the session as it was and says why in the shell's toast.
import { m } from '$lib/paraglide/messages.js';
import { toast } from '$lib/ui/feedback/toasts.svelte';
import { logout } from './session.svelte';

/** Resolves to whether the session ended. Never rejects. */
export async function signOut(): Promise<boolean> {
	try {
		await logout();
		return true;
	} catch {
		toast.push({ kind: 'error', text: m.auth_logout_failed() });
		return false;
	}
}
