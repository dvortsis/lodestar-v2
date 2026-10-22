// Where a 401 goes. The API client calls the registered handler once per login requirement:
// `/setup` while the instance has no password yet, otherwise `/login` with the way back.
//
// TODO(M-auth): M-auth owns the sign-in and setup flows. When it exports its own handler, replace
// `handleLoginRequired` with it in `+layout.svelte`; `loginTarget` keeps the contract tested.
import { goto } from '$app/navigation';

/** Only same-origin paths may be a `next`: nothing that could leave the site. */
export function safeNext(next: string): string {
	return next.startsWith('/') && !next.startsWith('//') && !next.includes('\\') ? next : '/';
}

export function loginTarget(next: string, opts: { setupRequired: boolean }): string {
	if (opts.setupRequired) return '/setup';
	const safe = safeNext(next);
	return safe === '/' ? '/login' : `/login?next=${encodeURIComponent(safe)}`;
}

export function handleLoginRequired(next: string, opts: { setupRequired: boolean }): void {
	// eslint-disable-next-line svelte/no-navigation-without-resolve -- no base path: the app is served from the site root (svelte.config.js sets no paths.base)
	void goto(loginTarget(next, opts), { replaceState: true });
}
