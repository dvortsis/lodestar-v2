// Recent queries (§10.4), kept in this browser only. Every access is guarded: storage can be
// blocked, full or hold anything (another tab, an older build), and a failure just means no recents.
const KEY = 'lodestar.search.recent';
export const RECENT_MAX = 8;
/** A query this long is not worth remembering (the server's limit is far higher). */
const QUERY_MAX = 200;

export function loadRecent(): string[] {
	try {
		const v: unknown = JSON.parse(localStorage.getItem(KEY) ?? '[]');
		if (!Array.isArray(v)) return [];
		return v
			.filter((q): q is string => typeof q === 'string' && !!q.trim() && q.length <= QUERY_MAX)
			.slice(0, RECENT_MAX);
	} catch {
		return [];
	}
}

/** Puts `q` first (a repeat moves up), capped at `RECENT_MAX`. */
export function addRecent(q: string): void {
	const query = q.trim();
	if (!query || query.length > QUERY_MAX) return;
	const next = [query, ...loadRecent().filter((r) => r !== query)].slice(0, RECENT_MAX);
	try {
		localStorage.setItem(KEY, JSON.stringify(next));
	} catch {
		// blocked or full: nothing to keep
	}
}
