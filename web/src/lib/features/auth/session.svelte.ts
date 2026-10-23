// The session as the shell sees it, and the hooks W9's shell installs (see README.md).
// Deliberately free of components and brand assets: the shell imports this on first load.
import { api, onLoginRequired, onSessionOk } from '$lib/api/client';
import { toLogin, toLoginFrom, toSetup } from './nav';

export type SessionStatus = 'unknown' | 'signed_in' | 'signed_out' | 'setup_required';

const session = $state<{ status: SessionStatus }>({ status: 'unknown' });

/** What the app last learned about the session. Reactive in runes contexts. */
export function currentSession(): SessionStatus {
	return session.status;
}

/** Records what a request showed about the session (the forms and the hooks below call it). */
export function setSession(status: SessionStatus): void {
	session.status = status;
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

// Staged Settings edits wait in sessionStorage for a sign-in redirect to bring them back. Only a
// deliberate logout clears them (the 401 handler must not). The prefix is DRAFT_PREFIX in
// features/settings/form/draft.ts, a literal here because this file is in the first load and must
// not import from features/settings; a test keeps the two equal.
const SETTINGS_DRAFT_PREFIX = 'lodestar.settings.draft.v1.';

function clearSettingsDrafts(): void {
	try {
		for (const key of Object.keys(sessionStorage)) {
			if (key.startsWith(SETTINGS_DRAFT_PREFIX)) sessionStorage.removeItem(key);
		}
	} catch {
		// Storage may be blocked; logging out must not depend on it.
	}
}

/** Ends the session and goes to the sign-in page. Rejects (staying put) if the server can't be reached. */
export async function logout(): Promise<void> {
	await api.post('/logout', { body: {} });
	clearSettingsDrafts();
	setSession('signed_out');
	await toLogin();
}
