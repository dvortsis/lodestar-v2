import type { components } from '$lib/api/schema';
import { m } from '$lib/paraglide/messages.js';

export type CompanionNeed = NonNullable<components['schemas']['Source']['companion_need']>;

/** Catalogue ids and their proper-noun names; an id nobody here knows reads as "a companion program". */
const NAMES: Record<string, string> = {
	qbittorrent: 'qBittorrent',
	prowlarr: 'Prowlarr',
	gluetun: 'gluetun',
	flaresolverr: 'FlareSolverr'
};

export const companionName = (id: string): string =>
	Object.hasOwn(NAMES, id) ? NAMES[id]! : m.companions_need_name_other();
