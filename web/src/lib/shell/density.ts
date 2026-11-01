// Density is URL state (§10.3): links keep it when moving between pages. Its own module, apart
// from nav.ts: pages that only build links (Alerts, the results panel) would otherwise
// load the destination table and its eight labels with it (X-alert-search-headroom).

/** `href` with the current `density` parameter, if there is one. */
export function withDensity(href: string, search: URLSearchParams): string {
	const density = search.get('density');
	if (!density) return href;
	return `${href}${href.includes('?') ? '&' : '?'}density=${encodeURIComponent(density)}`;
}
