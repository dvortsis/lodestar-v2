// The session as the shell sees it, and the hooks W9's shell installs (see README.md).
// Deliberately free of components and brand assets: the shell imports this on first load.
import { api, onLoginRequired, onSessionOk } from '$lib/api/client';
import { clearDetailCaches } from '$lib/features/detail/cache';
import { toLogin, toLoginFrom, toSetup } from './nav';

export type SessionStatus = 'unknown' | 'signed_in' | 'signed_out' | 'setup_required';

const session = $state<{ status: SessionStatus }>({ status: 'unknown' });

/** What the app last learned about the session. Reactive in runes contexts. */
export function currentSession(): SessionStatus {
	return session.status;
}

/** Records what a request showed about the session (the forms and the hooks below call it). */
export function setSession(status: SessionStatus): void {
	if (session.status === status) return;
	session.status = status;
	clearDetailCaches(status === 'signed_in');
}

/**
 * Wires W5's 401 handler: an expired session goes to `/login?next=<the current URL>`, an instance
 * that still needs its first-run setup to `/setup`. Returns the uninstall.
 */
export function installAuthHandlers(): () => void {
	onLoginRequired((next, { setupRequired }) => {
		setSession(setupRequired ? 'setup_required' : 'signed_out');
		void (setupRequired ? toSetup() : toLoginFrom(next));
	});
	const off = onSessionOk(() => setSession('signed_in'));
	return () => {
		onLoginRequired(undefined);
		off();
	};
}

/** Ends the session and goes to the sign-in page. Rejects (staying put) if the server can't be reached. */
export async function logout(): Promise<void> {
	await api.post('/logout', { body: {} });
	setSession('signed_out');
	await toLogin();
}
