// Where a sign-in returns to. `next` comes from the URL, so anyone can craft it: only a path on
// this origin is followed, never another site (an open redirect), and never back to the sign-in
// or setup pages themselves.

export const DEFAULT_NEXT = '/';

const AUTH_PAGES = ['/login', '/setup'];

/** `raw` as a same-origin path (with its query and hash), or `/` when it isn't one. */
export function safeNext(raw: string | null | undefined, origin: string): string {
	if (!raw || raw.length > 2048) return DEFAULT_NEXT;
	// Only a root-relative path: not `//host`, not `/\host`, no scheme, no control characters
	// (browsers strip tabs and newlines, which turns `/\t/host` into `//host`).
	// eslint-disable-next-line no-control-regex
	if (!raw.startsWith('/') || /^\/[/\\]/.test(raw) || /[\\\u0000-\u001F\u007F]/.test(raw)) {
		return DEFAULT_NEXT;
	}
	let url: URL;
	try {
		url = new URL(raw, origin);
	} catch {
		return DEFAULT_NEXT;
	}
	if (url.origin !== new URL(origin).origin) return DEFAULT_NEXT;
	const path = url.pathname;
	// Dot segments are resolved after the checks above: `/.//host`, `/a/..//host` and
	// `/%2e%2e//host` all become `//host`, a scheme-relative URL to another site.
	if (path.startsWith('//') || path.startsWith('/\\')) return DEFAULT_NEXT;
	if (AUTH_PAGES.some((p) => path === p || path.startsWith(p + '/'))) return DEFAULT_NEXT;
	if (path === '/api' || path.startsWith('/api/')) return DEFAULT_NEXT;
	return path + url.search + url.hash;
}

/** `/login?next=…` for the current path (W5's 401 handler passes pathname + search). */
export function loginUrl(next: string): string {
	return next && next !== DEFAULT_NEXT ? `/login?next=${encodeURIComponent(next)}` : '/login';
}
