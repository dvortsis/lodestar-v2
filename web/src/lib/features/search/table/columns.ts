// Table mode's columns (§10.4): which exist, which are shown and in what order. The choice is kept
// per browser in localStorage, validated on every read: a corrupt or unavailable store falls back
// to the defaults. Pure and unit-tested.
export const COLUMN_IDS = [
	'name',
	'type',
	'size',
	'files',
	'seeders',
	'leechers',
	'trackers',
	'discovered'
] as const;
export type ColumnId = (typeof COLUMN_IDS)[number];

export interface ColumnPrefs {
	/** Every column, in display order. */
	order: ColumnId[];
	hidden: ColumnId[];
}

/** The server sorts by these only (§7): other headers are plain. */
export const SORT_FIELD: Partial<Record<ColumnId, string>> = {
	size: 'size',
	files: 'files',
	discovered: 'discovered'
};
/** Numbers read right-aligned (§10.4). */
export const NUMERIC: ReadonlySet<ColumnId> = new Set([
	'size',
	'files',
	'seeders',
	'leechers',
	'trackers'
]);
/** The name is the row's header: it can't be hidden. */
export const REQUIRED: ColumnId = 'name';

export const STORAGE_KEY = 'lodestar.search.columns';

export const defaultPrefs = (): ColumnPrefs => ({ order: [...COLUMN_IDS], hidden: ['leechers'] });

const known = (v: unknown): v is ColumnId => COLUMN_IDS.includes(v as ColumnId);
const ids = (v: unknown): ColumnId[] => (Array.isArray(v) ? [...new Set(v.filter(known))] : []);

/** Anything stored becomes valid prefs: unknown ids dropped, missing ones appended, name kept. */
export function normalize(raw: unknown): ColumnPrefs {
	const base = defaultPrefs();
	if (typeof raw !== 'object' || raw === null) return base;
	const o = raw as Record<string, unknown>;
	const order = ids(o.order);
	for (const id of COLUMN_IDS) if (!order.includes(id)) order.push(id);
	const hidden = Array.isArray(o.hidden)
		? ids(o.hidden).filter((id) => id !== REQUIRED)
		: base.hidden;
	return { order, hidden };
}

export function readPrefs(
	storage: Pick<Storage, 'getItem'> | undefined = safeStorage()
): ColumnPrefs {
	try {
		const text = storage?.getItem(STORAGE_KEY);
		return text ? normalize(JSON.parse(text)) : defaultPrefs();
	} catch {
		return defaultPrefs();
	}
}

export function writePrefs(
	prefs: ColumnPrefs,
	storage: Pick<Storage, 'setItem'> | undefined = safeStorage()
): boolean {
	try {
		storage?.setItem(STORAGE_KEY, JSON.stringify(prefs));
		return !!storage;
	} catch {
		return false; // private mode or a full store: the choice lasts for this visit only
	}
}

function safeStorage(): Storage | undefined {
	try {
		return globalThis.localStorage;
	} catch {
		return undefined;
	}
}

/** The shown columns, in order. */
export const visible = (p: ColumnPrefs): ColumnId[] =>
	p.order.filter((id) => !p.hidden.includes(id));

export function toggleColumn(p: ColumnPrefs, id: ColumnId): ColumnPrefs {
	if (id === REQUIRED) return p;
	return {
		...p,
		hidden: p.hidden.includes(id) ? p.hidden.filter((h) => h !== id) : [...p.hidden, id]
	};
}

/** Moves `id` one place earlier (-1) or later (+1); the ends stay put. */
export function moveColumn(p: ColumnPrefs, id: ColumnId, by: -1 | 1): ColumnPrefs {
	const i = p.order.indexOf(id);
	const j = i + by;
	if (i < 0 || j < 0 || j >= p.order.length) return p;
	const order = [...p.order];
	[order[i], order[j]] = [order[j]!, order[i]!];
	return { ...p, order };
}

/**
 * The sort a header press asks for: a first press sorts largest/newest first (the server's default
 * order), a second reverses it, a third reverses it back. W5 owns the URL; this only picks the value.
 */
export function nextSort(current: string, field: string): string {
	return current === `-${field}` || current === field ? `+${field}` : `-${field}`;
}

/** aria-sort of a header given W5's sort. */
export function ariaSort(current: string, field: string): 'ascending' | 'descending' | 'none' {
	if (current === `+${field}`) return 'ascending';
	return current === `-${field}` || current === field ? 'descending' : 'none';
}
