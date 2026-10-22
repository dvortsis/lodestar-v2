// Where a 401 goes. The API client calls the registered handler once per login requirement:
// `/setup` while the instance has no password yet, otherwise `/login` with the way back.
//
// TODO(M-auth): M-auth owns the sign-in and setup flows. When it exports its own handler, replace
// `handleLoginRequired` with it in `Shell.svelte`; `loginTarget` keeps the contract tested.
import { goto } from '$app/navigation';

/** Only same-origin paths may be a `next`: nothing that could leave the site. */
export function safeNext(next: string): string {
	// URL parsing strips tabs and newlines, so `/\t/evil.com` would become `//evil.com`: refuse control
	// characters (and backslashes), then check the parsed result stays on this origin.
	// eslint-disable-next-line no-control-regex -- control characters are exactly what is refused
	if (!next.startsWith('/') || next.startsWith('//') || /[\u0000-\u001f\u007f\\]/.test(next))
		return '/';
	try {
		const base = 'http://lodestar.invalid';
		return new URL(next, base).origin === base ? next : '/';
	} catch {
		return '/';
	}
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
