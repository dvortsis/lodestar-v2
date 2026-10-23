// What the sign-in and setup pages ask on load: is the instance set up, and is this browser
// signed in? A failure (server down) returns null and the page shows its form anyway; the
// submit then reports the problem.
import { api } from '$lib/api/client';
import type { components } from '$lib/api/schema';
import { setSession } from './session.svelte';

export type AuthStatus = components['schemas']['AuthStatus'];

export async function checkAuth(): Promise<AuthStatus | null> {
	try {
		const status = await api.get('/auth');
		setSession(
			status.setup_required ? 'setup_required' : status.logged_in ? 'signed_in' : 'signed_out'
		);
		return status;
	} catch {
		return null;
	}
}

/** Where a page should send the browser instead of showing its form, if anywhere. */
export function redirectFor(
	page: 'login' | 'setup',
	status: AuthStatus | null,
	next: string
): string | null {
	if (!status) return null;
	if (page === 'login') {
		if (status.setup_required) return '/setup';
		return status.logged_in ? next : null;
	}
	if (status.setup_required) return null;
	return '/login';
}
