// "Group versions" (§10.4, owner §2.66 item 2): a results parameter, kept in the URL and
// remembered per account. The URL's `group` (`1` or `0`) wins; without it the account's
// `preferences.group_versions` decides (on by default). The account's value is cached in this
// browser for the first query of a visit, and the server's answer, read once, corrects the cache:
// like the density, a late answer takes effect with the next search, never under painted rows
// (`pin`).
import { api } from '$lib/api/client';

export const PARAM = 'group';
export const PREF = 'group_versions';
export const CACHE = 'lodestar.groupVersions';
const SECTION = { path: { section: 'preferences' } };

/** On or off from the query string; `pref` (the account's choice) when the URL doesn't say. */
export function groupedIn(search: string, pref: boolean): boolean {
	// A local reader, never stored or rendered.
	// eslint-disable-next-line svelte/prefer-svelte-reactivity
	const v = new URLSearchParams(search).get(PARAM);
	return v === '1' ? true : v === '0' ? false : pref;
}

/** This page with grouping written out, so a shared link keeps it whatever the reader's default. */
export function groupHref(url: URL, on: boolean): string {
	// A local builder, never stored or rendered.
	// eslint-disable-next-line svelte/prefer-svelte-reactivity
	const u = new URL(url);
	u.searchParams.set(PARAM, on ? '1' : '0');
	return u.pathname + u.search + u.hash;
}

function readCache(): boolean {
	try {
		return localStorage.getItem(CACHE) !== '0';
	} catch {
		return true; // storage blocked: the default
	}
}
function writeCache(on: boolean) {
	try {
		localStorage.setItem(CACHE, on ? '1' : '0');
	} catch {
		/* storage blocked: the server stays the source of truth */
	}
}

type Fields = { fields?: Record<string, { value?: unknown } | undefined> };

async function readPref(): Promise<boolean | undefined> {
	const res = (await api.get('/settings/{section}', SECTION)) as unknown as Fields;
	const v = res.fields?.[PREF]?.value;
	return typeof v === 'boolean' ? v : undefined;
}

export function createGrouping() {
	const cached = readCache();
	let pref = $state(cached);
	/** The choice the current search was asked with, and that search (without the view). */
	let applied = $state(cached);
	let appliedFor: string | undefined;
	let loaded: Promise<void> | undefined;
	/** Bumped by a choice: an older server answer, or an older save, must not undo it. */
	let generation = 0;
	/** Saves go one at a time, so they land in the order they were made. */
	let saving: Promise<void> = Promise.resolve();

	return {
		/** The account's choice, as this browser last knew it. */
		get pref() {
			return pref;
		},
		/** The account's choice the rows on screen were asked with (see `pin`). */
		get applied() {
			return applied;
		},
		/**
		 * The account's choice for the search `key` (the query and its options, not the view): taken
		 * when the search changes and kept while it doesn't, so a late answer from `load` never
		 * re-runs the rows on screen, even when the view changes. Call untracked: it writes state.
		 */
		pin(key: string): boolean {
			if (key !== appliedFor) {
				appliedFor = key;
				applied = pref;
			}
			return applied;
		},
		/** Reads the account's choice once per visit; any failure keeps the cached one. */
		load(): Promise<void> {
			const g = generation;
			return (loaded ??= readPref().then(
				(on) => {
					if (on === undefined || g !== generation) return;
					pref = on;
					writeCache(on);
				},
				() => {}
			));
		},
		/**
		 * Remembers a choice for the account: at once in this browser, then on the server, as the one
		 * field (no `if_revision`, like the shortcuts toggle). Saves run one at a time and a save
		 * overtaken by a newer choice is skipped, so the last choice is the one kept. A failure
		 * leaves the URL and the cache holding the choice.
		 */
		remember(on: boolean): Promise<void> {
			const g = ++generation;
			pref = on;
			writeCache(on);
			saving = saving.then(async () => {
				if (g !== generation) return; // a newer choice saves itself
				try {
					await api.patch('/settings/{section}', {
						...SECTION,
						body: { [PREF]: on } as unknown as Record<string, never>
					});
				} catch {
					/* offline or refused: this browser keeps the choice */
				}
			});
			return saving;
		}
	};
}

export type Grouping = ReturnType<typeof createGrouping>;

/** The page's one instance, read by the toolbar's switch and the results panel. */
export const grouping = createGrouping();
