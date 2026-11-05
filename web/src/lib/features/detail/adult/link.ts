/** The scene link, only when it is an absolute `https` URL; anything else shows no link. */
export function safeLink(link: string | null | undefined): string | null {
	if (!link) return null;
	try {
		const url = new URL(link);
		return url.protocol === 'https:' && url.host ? url.href : null;
	} catch {
		return null;
	}
}
