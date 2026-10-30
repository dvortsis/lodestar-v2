// The Newest tabs (§10.7): one per content type, in the glossary's order. The server decides what
// a type holds (and whether Adult shows at all); the client only names the tabs and builds links.
import { m } from '$lib/paraglide/messages.js';
import { TYPE_LABELS } from '$lib/util/content-labels';

export const TYPES = [
	'movie',
	'tv',
	'music',
	'audiobook',
	'ebook',
	'comic',
	'software',
	'game',
	'images',
	'video',
	'other',
	'adult'
] as const;
export type ContentType = (typeof TYPES)[number];

/** A tab is a content type, or null for All. */
export type Tab = ContentType | null;

export const tabLabel = (tab: Tab): string =>
	tab === null ? m.discover_type_all() : TYPE_LABELS[tab]!();

/** The `type` parameter as sent: a known tab, a value the server will reject, or null for All. */
export function typeParam(search: URLSearchParams): string | null {
	const raw = search.get('type');
	return raw === null || raw === '' ? null : raw;
}

/** The tab for a `type` value, or undefined when it isn't one (the server answers `unknown_type`). */
export function knownTab(type: string | null): Tab | undefined {
	if (type === null) return null;
	return (TYPES as readonly string[]).includes(type) ? (type as ContentType) : undefined;
}

/** Unresolved paths: the page passes each through `resolve()`. `/discover` for `type`, keeping every other parameter (density, …) and dropping the cursor. */
export function tabHref(type: string | null, search: URLSearchParams): string {
	const next = new URLSearchParams(search);
	next.delete('cursor');
	if (type === null) next.delete('type');
	else next.set('type', type);
	const qs = next.toString();
	return '/discover' + (qs ? `?${qs}` : '');
}

/** "See all in Search": the query the server runs for Newest, so the counts agree. */
export function searchHref(type: string | null, search: URLSearchParams): string {
	const next = new URLSearchParams();
	if (type !== null) next.set('q', `type:${type}`);
	else next.set('browse', '1');
	next.set('sort', '-discovered');
	const density = search.get('density');
	if (density) next.set('density', density);
	return '/search' + `?${next.toString()}`;
}

const HASH = /^[0-9a-f]{40}$/;
/** `/t/<hash>` for a torrent hash from the server, or null when it isn't 40 lowercase hex digits. */
export function torrentHref(hash: string, search: URLSearchParams): string | null {
	if (!HASH.test(hash)) return null;
	const density = search.get('density');
	return `/t/${hash}` + (density ? `?density=${encodeURIComponent(density)}` : '');
}
