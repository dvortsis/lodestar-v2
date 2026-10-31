// The download-client list of `integrations.clients` (B-settings-sections part a): the shape the
// cards edit, and the pure helpers around it. A client's password is not part of this value:
// the server never sends it, and the cards never stage one (see ClientCard.svelte).
export interface Client {
	id: string;
	name: string;
	kind: string;
	url: string;
	username: string;
	category: string;
	save_path: string;
	paused: boolean;
}

export const MAX_CLIENTS = 8;

/** The kinds a client can be, named the way their projects name themselves. */
export const KINDS = [
	{ value: 'qbittorrent', label: 'qBittorrent' },
	{ value: 'transmission', label: 'Transmission' },
	{ value: 'deluge', label: 'Deluge' },
	{ value: 'rtorrent', label: 'rTorrent' },
	{ value: 'aria2', label: 'Aria2' }
] as const;

const text = (v: unknown) => (typeof v === 'string' ? v : '');

/** Reads the field's value as a list of clients; anything else is an empty list. */
export function toClients(value: unknown): Client[] {
	if (!Array.isArray(value)) return [];
	return value
		.filter((v): v is Record<string, unknown> => typeof v === 'object' && v !== null)
		.map((v) => ({
			id: text(v.id),
			name: text(v.name),
			kind: text(v.kind) || 'qbittorrent',
			url: text(v.url),
			username: text(v.username),
			category: text(v.category),
			save_path: text(v.save_path),
			paused: v.paused === true
		}));
}

/** The first `client-N` not in use: ids are lowercase letters, digits and `-`. */
export function newId(list: Client[]): string {
	const used = new Set(list.map((c) => c.id));
	for (let n = 1; ; n++) if (!used.has(`client-${n}`)) return `client-${n}`;
}

export function blankClient(list: Client[]): Client {
	return {
		id: newId(list),
		name: '',
		kind: 'qbittorrent',
		url: '',
		username: '',
		category: '',
		save_path: '',
		paused: false
	};
}

export function move<T>(list: T[], from: number, to: number): T[] {
	if (to < 0 || to >= list.length || from === to) return list;
	const next = [...list];
	next.splice(to, 0, next.splice(from, 1)[0]!);
	return next;
}
