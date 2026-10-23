// URLs the server names (`system.address_change`): the shell navigates to them, so only an
// http(s) address is ever followed. Anything else is shown as text, never opened.

/** The address as a URL when it is http: or https:; null for anything else, malformed included. */
export function acceptUrl(address: string): URL | null {
	let url: URL;
	try {
		url = new URL(address);
	} catch {
		return null;
	}
	return url.protocol === 'http:' || url.protocol === 'https:' ? url : null;
}

/** `/healthz` of a server, or null when its address isn't acceptable. */
export function healthUrl(address: string): URL | null {
	const base = acceptUrl(address);
	return base ? new URL('/healthz', base) : null;
}

/** Where to land after moving to a new address: Settings → Network & VPN there. */
export function networkUrl(address: string): URL | null {
	const base = acceptUrl(address);
	return base ? new URL('/settings/network', base) : null;
}
