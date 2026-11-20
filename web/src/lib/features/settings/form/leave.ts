// The leave guard's decision (§10.10.5), pure so every case is tested without a router.
//   allow   — let the navigation happen
//   prompt  — cancel it and ask Save / Discard / Keep editing in the app
//   browser — cancel it so the browser asks (closing or reloading the tab)
//   signin  — the session ended (or the Legal Notice is due) and the app is going to sign in:
//             cancel it, say so, keep the draft for the sign-in (X-session-draft-keep)
//   setup   — the user signed out, or the instance needs its first-run setup again: nothing
//             staged is kept, so let it go

export interface LeaveInput {
	/** Unsaved changes (dirty fields or conflicts). */
	dirty: boolean;
	/** SvelteKit's navigation type: 'leave' is closing or reloading the tab. */
	type: string;
	from: URL | null;
	to: URL | null;
	/** Set while the guard's own navigation after Save or Discard runs. */
	bypass: boolean;
	/** Why the app is going to sign in or set up, if it is (`signInPending()`). */
	signIn?: string | null;
}

export type LeaveDecision = 'allow' | 'prompt' | 'browser' | 'signin' | 'setup';

const isPage = (to: URL | null, from: URL | null, path: string) =>
	!!to &&
	(!from || to.origin === from.origin) &&
	(to.pathname === path || to.pathname.startsWith(path + '/'));

export function leaveDecision({
	dirty,
	type,
	from,
	to,
	bypass,
	signIn = null
}: LeaveInput): LeaveDecision {
	if (!dirty || bypass) return 'allow';
	if (type === 'leave') return 'browser';
	// Signed out on purpose, or re-claimed: nothing staged is kept, so let it go.
	if (signIn === 'setup' && isPage(to, from, '/setup')) return 'setup';
	if (signIn === 'signedout' && isPage(to, from, '/login')) return 'setup';
	if ((signIn === 'expired' || signIn === 'notice') && isPage(to, from, '/login')) return 'signin';
	// A hash or query change on the same path isn't leaving the screen.
	if (from && to && from.origin === to.origin && from.pathname === to.pathname) return 'allow';
	return 'prompt';
}

/** A full-page navigation, for a destination the router can't take (another site, a reload). */
export function hardNavigate(href: string): void {
	location.assign(href);
}
