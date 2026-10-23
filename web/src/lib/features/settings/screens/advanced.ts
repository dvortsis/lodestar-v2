// Which Advanced disclosures a screen leaves open (§10.10.2): remembered per screen in
// `localStorage` under `lodestar.settings.advanced.<screen>` as the list of open group ids. Every
// access is in a try/catch: storage may be blocked, full or absent, and nothing else depends on it.
const KEY = 'lodestar.settings.advanced.';

export function readOpen(screen: string): string[] {
	try {
		const raw = localStorage.getItem(KEY + screen);
		const value: unknown = raw === null ? [] : JSON.parse(raw);
		return Array.isArray(value) ? value.filter((v): v is string => typeof v === 'string') : [];
	} catch {
		return [];
	}
}

export function writeOpen(screen: string, groups: Iterable<string>): void {
	try {
		const list = [...groups];
		if (list.length) localStorage.setItem(KEY + screen, JSON.stringify(list));
		else localStorage.removeItem(KEY + screen);
	} catch {
		// Blocked or full: the disclosure just isn't remembered.
	}
}
