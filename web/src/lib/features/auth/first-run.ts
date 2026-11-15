// The search page's gate (X-first-run-search-gate). Search is the landing page and, to keep its bare
// first load small (§10.1), makes no request until the first interaction, so a fresh instance, a
// signed-out visitor or a pending Legal Notice met no 401 to redirect on: the page showed a search
// field and nothing else. The page fetches `GET /auth` itself after mount (`checkFirstRun`, no
// imports) and loads this module only to leave: for first-run setup, or for the sign-in page, which
// shows a pending notice. A signed-in visitor's load gains one small response and nothing else.
import { goto } from '$app/navigation';
import type { components } from '$lib/api/schema';
import { toSetup } from './nav';
import { loginUrl } from './next';
import { setSession } from './session.svelte';

type AuthStatus = components['schemas']['AuthStatus'];

/** Where the search page sends the browser for `status`, or null to stay. */
export function firstRunTarget(status: AuthStatus): 'setup' | 'login' | null {
	if (status.setup_required) return 'setup';
	if (!status.logged_in || status.notice_required) return 'login';
	return null;
}

/**
 * Sends the browser on for `status`: setup, or sign-in returning to this page. The entry is
 * replaced, so Back doesn't return to a page that would send it on again.
 */
export async function leaveForFirstRun(status: AuthStatus): Promise<void> {
	const to = firstRunTarget(status);
	if (to === 'setup') {
		setSession('setup_required');
		await toSetup();
	} else if (to === 'login') {
		if (!status.logged_in) setSession('signed_out');
		const next = location.pathname + location.search;
		// eslint-disable-next-line svelte/no-navigation-without-resolve -- loginUrl's own URL
		await goto(loginUrl(next), { replaceState: true });
	}
}
