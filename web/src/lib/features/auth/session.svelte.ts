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
/** Why the app is on its way to sign in: `expired`, `notice`, `setup`, or `signedout` after an
 * explicit Sign out (which also blocks Settings drafts until the next sign-in); absent otherwise. */
export const SIGN_IN_KEY = 'lodestar.signin.v1';
/** The Settings form stayed on the page instead of signing in: redirect again on the next 401. */
export const REARM_EVENT = 'lodestar:signin-rearm';

export type SignInReason = 'expired' | 'notice' | 'setup' | 'signedout';

function store(): Storage | undefined {
	try {
		return globalThis.sessionStorage ?? undefined;
	} catch {
		return undefined; // blocked: drafts are off, and so is all of this
	}
}

// The block epoch: bumped each time a Sign out or a re-claim sets the block. Only an answer to a
// request sent at the current epoch can show the session is live again (`probe`).
let epoch = 0;
let probing: Promise<void> | undefined;
let probedAt = -1;

function setSignIn(reason: SignInReason | null): void {
	if (reason === 'signedout' || reason === 'setup') epoch++;
	try {
		const s = store();
		if (!s) return;
		if (reason) s.setItem(SIGN_IN_KEY, reason);
		else s.removeItem(SIGN_IN_KEY);
	} catch {
		// full or blocked
	}
}

/** Clears an `expired` or `notice` reason; a `signedout` or `setup` block stays (see `signedIn`). */
function clearPending(): void {
	const v = signInPending();
	if (v === 'expired' || v === 'notice') setSignIn(null);
}

/**
 * While drafts are blocked, a request that worked proves nothing by itself: it may answer one sent
 * before the block. So ask `GET /auth` afresh, at this epoch; a live session there (a sign-in from
 * another tab, then a reload or this tab's next request) lifts the block, unless a newer block was
 * set meanwhile.
 */
export function probe(): Promise<void> {
	const v = signInPending();
	if (v !== 'signedout' && v !== 'setup') return Promise.resolve();
	if (probing && probedAt === epoch) return probing;
	const sent = (probedAt = epoch);
	const p: Promise<void> = api
		.get('/auth')
		.then(
			(a) => {
				if (sent === epoch && a.logged_in && !a.setup_required) signedIn();
			},
			() => {}
		)
		.finally(() => {
			if (probing === p) probing = undefined;
		});
	return (probing = p);
}

/** Why the app is going to sign in, if it is. */
export function signInPending(): SignInReason | null {
	try {
		const v = store()?.getItem(SIGN_IN_KEY);
		return v === 'expired' || v === 'notice' || v === 'setup' || v === 'signedout' ? v : null;
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
	// However it's learned (a 401, GET /auth, a refused sign-in), an instance that needs its
	// first-run setup again may have a new owner: nothing staged on it is kept, and drafts stay
	// blocked until a sign-in or the setup succeeds (`signedIn`).
	if (status === 'setup_required') {
		clearSettingsDrafts();
		setSignIn('setup');
	}
	if (status === 'signed_in') void probe();
	if (session.status === status) return;
	// The first successful request establishes this tab's existing session. It
	// must not invalidate and repeat the very request that established it. Actual
	// sign-out/setup transitions still clear data, and subsequent sign-in reloads.
	const initialConfirmation = session.status === 'unknown' && status === 'signed_in';
	session.status = status;
	// Any request that worked ends a pending expiry, but never a Sign out's or a re-claim's
	// block: a late response to a request sent before them is no sign-in (`probe` decides).
	if (status === 'signed_in') clearPending();
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
		// `setSession('setup_required')` drops the drafts (X-session-draft-keep).
		if (!noticeRequired) setSession(setupRequired ? 'setup_required' : 'signed_out');
		// After an explicit Sign out or a re-claim a refused request is no expiry: nothing is kept.
		const pending = signInPending();
		if (!setupRequired && pending !== 'signedout' && pending !== 'setup')
			setSignIn(noticeRequired ? 'notice' : 'expired');
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
	clearPending();
	if (handler) onLoginRequired(handler);
}

/**
 * A sign-in or the first-run setup succeeded (the sign-in and setup forms call this): Settings
 * drafts may be kept again, and the session is signed in.
 */
export function signedIn(): void {
	setSignIn(null);
	setSession('signed_in');
}

/** Ends the session and goes to the sign-in page. Rejects (staying put) if the server can't be reached. */
export async function logout(): Promise<void> {
	await api.post('/logout', { body: {} });
	// Signing out ends whatever was staged in this tab: nothing is kept for the next sign-in, and
	// no draft can be written again until it (a pending debounce, a form torn down later).
	setSignIn('signedout');
	clearSettingsDrafts();
	setSession('signed_out');
	await toLogin();
}
