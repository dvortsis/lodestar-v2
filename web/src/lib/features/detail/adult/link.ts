// Control characters and bidirectional controls never belong in a link the service sends.
// eslint-disable-next-line no-control-regex -- that is the point
const UNSAFE = /[\u0000-\u001f\u007f\u202a-\u202e\u2066-\u2069]/;

/** The scene link, only when it is an absolute `https` URL; anything else shows no link. */
export function safeLink(link: string | null | undefined): string | null {
	if (!link || UNSAFE.test(link)) return null;
	try {
		const url = new URL(link);
		return url.protocol === 'https:' && url.host && !url.username && !url.password
			? url.href
			: null;
	} catch {
		return null;
	}
}
