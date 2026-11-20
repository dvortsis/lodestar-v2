// Where Settings drafts live, and the sign-in that may keep one (§10.10.5, X-session-draft-keep).
// The shell's session (`auth/session.svelte.ts`) and the form share only `sessionStorage` keys
// and one window event, never an import, so neither adds a shared chunk to the other's first
// view; `draft-keep.test.ts` checks the keys match.
//
// A draft is kept for one sign-in only when a save fails because the session ended: the form
// writes a "kept" marker naming the screen (`signed-out.ts`, on demand), and the same screen
// restores the draft once the sign-in returns to it (`saving.ts`, in the edit chunk). Sign out,
// or an instance that needs its first-run setup again, drops every draft and the marker.

export const DRAFT_PREFIX = 'lodestar.settings.draft.v1.';
export const KEPT_KEY = 'lodestar.settings.kept.v1';
export function storage(): Storage | undefined {
	try {
		return globalThis.sessionStorage ?? undefined;
	} catch {
		return undefined; // blocked (sandboxed frame, privacy settings)
	}
}

// --- the sign-in on its way ----------------------------------------------------------------
// Written by the shell's 401 handler, read by the form's leave guard (the same names as
// `SIGN_IN_KEY` and `REARM_EVENT` in `auth/session.svelte.ts`).

export const SIGN_IN_KEY = 'lodestar.signin.v1';
export const REARM_EVENT = 'lodestar:signin-rearm';

/**
 * True after an explicit Sign out (`signedout`) or on an instance that needs its setup again
 * (`setup`), until the next sign-in: nothing may write a draft then (`writeDraft`), so a pending
 * debounce, a form torn down later or a save refused afterwards can't bring one back.
 */
export function draftsBlocked(): boolean {
	const v = signInPending();
	return v === 'signedout' || v === 'setup';
}

/**
 * Why the app is going to sign in (`expired`, `notice`), set up (`setup`), or the user signed out
 * (`signedout`), if it is.
 */
export function signInPending(): string | null {
	try {
		return storage()?.getItem(SIGN_IN_KEY) ?? null;
	} catch {
		return null;
	}
}
