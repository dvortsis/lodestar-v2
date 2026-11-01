// The results panel's links. Density is URL state (§10.3): links inside the app keep it, as the
// navigation does. No base path, so resolve() returns the path unchanged (shell/nav.ts does the same).
import { page } from '$app/state';
import { resolve } from '$app/paths';
import { withDensity } from '$lib/shell/density';

const keep = (path: string) => withDensity(path, page.url.searchParams);

/** A search for `q` (and `sort`, for a saved search). */
export function searchHref(q: string, sort?: string | null): string {
	const p = new URLSearchParams({ q });
	if (sort) p.set('sort', sort);
	return keep(`${resolve('/search')}?${p}`);
}

/** Where "Show adult content" is (provisional until S-appearance owns the page, as S-detail's). */
export const appearanceHref = (): string => keep(resolve('/settings/appearance' as '/'));

/** A torrent's own page, for "Open in a new tab". */
export const torrentTabHref = (hash: string): string => keep(resolve('/t/[hash]', { hash }));
