// `/compare?h=a,b,c` is the whole state (§10.3). S-search-5's bulk bar links with `compareHref`.
import { resolve } from '$app/paths';

const HASH = /^[0-9a-f]{40}$/;

/** The `h` list in column order: split on commas, empties dropped, nothing else checked. */
export function parseHashes(param: string | null): string[] {
	return (param ?? '')
		.split(',')
		.map((h) => h.trim())
		.filter((h) => h !== '');
}

/** `/compare?h=a,b` plus any other parameters of `current` (`density`) left as they are. */
export function compareHref(hashes: string[], current?: URLSearchParams): string {
	const params = new URLSearchParams(current);
	params.set('h', hashes.join(','));
	// The commas are the list's separator (§10.3); keep them readable in the address bar.
	return `${resolve('/compare')}?${params.toString().replaceAll('%2C', ',')}`;
}

/** `/t/<hash>` for a well-formed hash; null otherwise, so nothing else is ever linked. */
export function torrentHref(hash: string): string | null {
	return HASH.test(hash) ? resolve('/t/[hash]', { hash }) : null;
}
