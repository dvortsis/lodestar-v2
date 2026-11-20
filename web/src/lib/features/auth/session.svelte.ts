// The session as the shell sees it, and the hooks W9's shell installs (see README.md).
// Deliberately free of components and brand assets: the shell imports this on first load.
import { api, onLoginRequired, onSessionOk } from '$lib/api/client';
import { clearDetailCaches } from '$lib/features/detail/cache';
import { toLogin, toLoginFrom, toSetup } from './nav';

export type SessionStatus = 'unknown' | 'signed_in' | 'signed_out' | 'setup_required';

const session = $state<{ status: SessionStatus }>({ status: 'unknown' });

// --- Settings drafts and the sign-in (X-session-draft-keep) ------------------------------------
// The Settings form keeps an unsaved draft in `sessionStorage` across a sign-in that a session
// expiry forced. This module tells it why the app is going to sign in, and drops every draft when
// the session ends on purpose. They share only these storage keys and one window event, not an
// import: either one bundled into the other's chunks would cost every first view a shared chunk.
// `settings/form/draft-scope.ts` reads the same keys (its tests check they match).

/** Every Settings draft key, and the kept-for-sign-in marker, start with this. */
export const SETTINGS_DRAFTS_PREFIX = 'lodestar.settings.';
/** Why the app is on its way to sign in: `expired`, `notice` or `setup`; absent otherwise. */
export const SIGN_IN_KEY = 'lodestar.signin.v1';
/** The Settings form stayed on the page instead of signing in: redirect again on the next 401. */
export const REARM_EVENT = 'lodestar:signin-rearm';

export type SignInReason = 'expired' | 'notice' | 'setup';

function store(): Storage | undefined {
	try {
		return globalThis.sessionStorage ?? undefined;
	} catch {
		return undefined; // blocked: drafts are off, and so is all of this
	}
}

function setSignIn(reason: SignInReason | null): void {
	try {
		const s = store();
		if (!s) return;
		if (reason) s.setItem(SIGN_IN_KEY, reason);
		else s.removeItem(SIGN_IN_KEY);
	} catch {
		// full or blocked
	}
}

/** Why the app is going to sign in, if it is. */
export function signInPending(): SignInReason | null {
	try {
		const v = store()?.getItem(SIGN_IN_KEY);
		return v === 'expired' || v === 'notice' || v === 'setup' ? v : null;
	} catch {
		return null;
	}
}

/** Drops every Settings draft and the kept marker (Sign out, a re-claimed instance). */
export function clearSettingsDrafts(): void {
	try {
		const s = store();
		if (!s) return;
		const keys: string[] = [];
		for (let i = 0; i < s.length; i++) {
			const k = s.key(i);
			if (k?.startsWith(SETTINGS_DRAFTS_PREFIX)) keys.push(k);
		}
		for (const k of keys) s.removeItem(k);
	} catch {
		// blocked: nothing is stored either
	}
}

/** What the app last learned about the session. Reactive in runes contexts. */
export function currentSession(): SessionStatus {
	return session.status;
}

/** Records what a request showed about the session (the forms and the hooks below call it). */
export function setSession(status: SessionStatus): void {
	if (session.status === status) return;
	// The first successful request establishes this tab's existing session. It
	// must not invalidate and repeat the very request that established it. Actual
	// sign-out/setup transitions still clear data, and subsequent sign-in reloads.
	const initialConfirmation = session.status === 'unknown' && status === 'signed_in';
	session.status = status;
	if (status === 'signed_in') setSignIn(null);
	if (!initialConfirmation) clearDetailCaches(status === 'signed_in');
	// Signing in: the account's language, zone and week replace the cached hint (lib/i18n/boot.ts).
	// A normal load's first confirmation is skipped (it would add a request to the first load), but
	// one on the login or setup page counts: a failed GET /auth there leaves the status unknown
	// until the user signs in.
	if (
		status === 'signed_in' &&
		(!initialConfirmation || /^\/(login|setup)\b/.test(location.pathname))
	)
		void import('$lib/i18n/boot').then(
			(m) => m.refetchRegion(),
			() => {}
		);
}

/**
 * Wires W5's 401 handler: an expired session goes to `/login?next=<the current URL>`, an instance
 * that still needs its first-run setup to `/setup`, and a session that hasn't accepted the current
 * Legal Notice (409 `notice_required`) to `/login?next=…` too, where the notice is shown. Returns
 * the uninstall.
 */
export function installAuthHandlers(): () => void {
	handler = (next, { setupRequired, noticeRequired }) => {
		// A pending Legal Notice keeps the session: the sign-in page shows the notice.
		if (!noticeRequired) setSession(setupRequired ? 'setup_required' : 'signed_out');
		// An instance that needs its first-run setup again may have a new owner: nothing staged
		// on it is kept (X-session-draft-keep).
		if (setupRequired) clearSettingsDrafts();
		setSignIn(setupRequired ? 'setup' : noticeRequired ? 'notice' : 'expired');
		void (setupRequired ? toSetup() : toLoginFrom(next));
	};
	onLoginRequired(handler);
	const rearm = () => rearmSignIn();
	globalThis.addEventListener?.(REARM_EVENT, rearm);
	const off = onSessionOk(() => setSession('signed_in'));
	return () => {
		handler = undefined;
		globalThis.removeEventListener?.(REARM_EVENT, rearm);
		onLoginRequired(undefined);
		off();
	};
}

let handler: Parameters<typeof onLoginRequired>[0];

/**
 * The user chose to stay on the page instead of signing in: the next request that finds the
 * session ended redirects again (W5's handler redirects once until a request succeeds).
 */
export function rearmSignIn(): void {
	setSignIn(null);
	if (handler) onLoginRequired(handler);
}

/** Ends the session and goes to the sign-in page. Rejects (staying put) if the server can't be reached. */
export async function logout(): Promise<void> {
	await api.post('/logout', { body: {} });
	// Signing out ends whatever was staged in this tab: nothing is kept for the next sign-in.
	clearSettingsDrafts();
	setSignIn(null);
	setSession('signed_out');
	await toLogin();
}
