// The Settings screen registry (§10.10.2, §10.10.8): which screens exist, in which group and
// order, and which one each schema field lives on. Features register a screen by exporting it
// from a tiny `screen.ts` (README.md); everything the schema has that no screen claims gets an
// interim generated screen, so a new server setting is never unreachable (§10.10.1).
import type { Component } from 'svelte';
import type { components } from '$lib/api/schema';
import { currentLocale } from '$lib/i18n/locale.svelte';
import type { SettingsIndexView } from '../form/schema';
import { settingText } from '../form/messages';
import type { FieldKey, FieldSpec } from '../form/types';

export type ScreenGroup = 'preferences' | 'crawling' | 'library' | 'connections' | 'system';

export interface SettingsScreen {
	/** The URL segment: `/settings/<id>`. */
	id: string;
	group: ScreenGroup;
	title: () => string;
	/** A hand-built screen, loaded on open; it renders its own `<h1 tabindex="-1">`. */
	load?: () => Promise<{ default: Component }>;
	/** The schema sections whose fields it shows. */
	sections?: readonly string[];
	/** Only these `x-group`s of them (Keyboard: `['keyboard']`). */
	groups?: readonly string[];
	/** A `~icons/ph/*` import; the shell's gear when absent. */
	icon?: Component;
	/** A hand-built screen's own findable items (§10.10.3's static registry). */
	search?: () => SearchEntry[];
	/** Its Overview rows. */
	attention?: (ctx: AttentionContext) => Promise<AttentionItem[]>;
}

export interface SearchEntry {
	id: string;
	label: () => string;
	words?: () => string[];
	anchor?: string;
}

export interface AttentionItem {
	id: string;
	screen: string;
	severity: 'problem' | 'restart' | 'notice';
	text: () => string;
	action: { label: () => string; href?: string; run?: () => Promise<void> };
}

/** The `/system` the shell already fetched. */
export interface AttentionContext {
	system: components['schemas']['SystemInfo'];
	signal: AbortSignal;
}

export const GROUPS: readonly ScreenGroup[] = [
	'preferences',
	'crawling',
	'library',
	'connections',
	'system'
];

/** The order inside each group (§10.10.2); an id not listed goes last, by title. */
export const SCREEN_ORDER: Record<ScreenGroup, readonly string[]> = {
	preferences: ['appearance', 'keyboard', 'language'],
	crawling: ['crawler', 'network', 'sources', 'workers', 'companions'],
	library: ['storage', 'search', 'trackers', 'filters'],
	connections: ['integrations', 'notifications', 'insights'],
	system: ['general', 'updates', 'logging', 'security', 'about']
};

/** `preferences` is saved per account and applies instantly; every other section is staged. */
export const INSTANT_SECTION = 'preferences';

/** The group of an interim screen (a section no screen claims); anything else is System. */
const INTERIM_GROUP: Record<string, ScreenGroup> = {
	preferences: 'preferences',
	content: 'preferences'
};

/**
 * Fields a screen shows before its own `screen.ts` says so: S-keys' file declares no `sections`,
 * so Keyboard keeps its two fields through this claim.
 * TODO(shortcuts): move this into features/shortcuts/screen.ts and delete it here.
 */
const FALLBACK_CLAIMS: Record<string, { sections: readonly string[]; groups: readonly string[] }> =
	{ keyboard: { sections: ['preferences'], groups: ['keyboard'] } };

/** Aliases the shell's More menu links to (`lib/shell/nav.ts`). */
export const ALIASES: Record<string, string> = {
	backup: '/settings/general#backup',
	import: '/settings/general#backup'
};

// --- discovery -------------------------------------------------------------------------------

const isScreen = (v: unknown): v is SettingsScreen =>
	typeof v === 'object' &&
	v !== null &&
	typeof (v as SettingsScreen).id === 'string' &&
	typeof (v as SettingsScreen).group === 'string' &&
	typeof (v as SettingsScreen).title === 'function';

function devError(message: string) {
	if (import.meta.env.DEV) console.error(`settings: ${message}`);
}

/**
 * The screens in these modules: a module exports `screen` (one), `screens` (several) or a default
 * of either. One screen exported under two names counts once; a repeated id keeps the first.
 */
export function collect(modules: Record<string, unknown>): SettingsScreen[] {
	const seen = new Set<unknown>();
	const ids = new Set<string>();
	const out: SettingsScreen[] = [];
	const take = (value: unknown, from: string) => {
		for (const v of Array.isArray(value) ? value : [value]) {
			if (v === undefined || seen.has(v)) continue;
			seen.add(v);
			if (!isScreen(v)) {
				devError(`${from} exports something that isn't a SettingsScreen`);
			} else if (!GROUPS.includes(v.group)) {
				devError(`${from}: screen "${v.id}" has an unknown group "${v.group}"`);
			} else if (ids.has(v.id)) {
				devError(`${from}: screen id "${v.id}" is registered twice`);
			} else {
				ids.add(v.id);
				out.push(v);
			}
		}
	};
	for (const [path, mod] of Object.entries(modules)) {
		const m = (mod ?? {}) as Record<string, unknown>;
		take(m.screens, path);
		take(m.screen, path);
		take(m.default, path);
	}
	return out;
}

/** Every `screen.ts` of a feature folder (`features/<f>/screen.ts` or `features/<f>/<g>/screen.ts`). */
export const registered: SettingsScreen[] = collect(
	import.meta.glob(['/src/lib/features/*/screen.ts', '/src/lib/features/*/*/screen.ts'], {
		eager: true
	})
);

// --- order -----------------------------------------------------------------------------------

/** Navigation order: group order, then the id table, then unlisted ids last by title. */
export function sortScreens<T extends { group: ScreenGroup; id: string; title: () => string }>(
	screens: readonly T[]
): T[] {
	const rank = (s: T) => {
		const at = SCREEN_ORDER[s.group].indexOf(s.id);
		return at < 0 ? SCREEN_ORDER[s.group].length : at;
	};
	return [...screens].sort(
		(a, b) =>
			GROUPS.indexOf(a.group) - GROUPS.indexOf(b.group) ||
			rank(a) - rank(b) ||
			a.title().localeCompare(b.title(), currentLocale())
	);
}

// --- resolving -------------------------------------------------------------------------------

export interface ResolvedScreen {
	screen: SettingsScreen;
	/** Made for a section no screen claims; disappears when its slice claims the fields. */
	interim: boolean;
	/** Preferences save instantly; everything else is staged and saved together. */
	mode: 'staged' | 'instant';
	/** The fields it shows, in schema order. */
	keys: FieldKey[];
}

export interface Registry {
	/** In navigation order. */
	readonly screens: readonly ResolvedScreen[];
	/** Fields claimed by more than one screen (a dev error): `key → the screens`. */
	readonly doubleClaims: ReadonlyMap<FieldKey, string[]>;
	get(id: string): ResolvedScreen | undefined;
	/** The fields a screen shows, in schema order. */
	fieldsFor(screen: SettingsScreen | string): FieldSpec[];
	/** The screen a field lives on. */
	screenOf(key: FieldKey): ResolvedScreen | undefined;
}

function claims(screen: SettingsScreen) {
	if (screen.sections) return { sections: screen.sections, groups: screen.groups };
	return FALLBACK_CLAIMS[screen.id];
}

/**
 * Resolves the screens against the schema: each field goes to the first registered screen that
 * claims it, and the fields nobody claims form an interim generated screen per section. Without
 * an index (still loading) only the registered screens are known.
 */
export function buildRegistry(
	index: SettingsIndexView | undefined,
	screens: readonly SettingsScreen[] = registered
): Registry {
	const owner = new Map<FieldKey, SettingsScreen>();
	const doubleClaims = new Map<FieldKey, string[]>();
	const keysOf = new Map<string, FieldKey[]>();
	const specOf = new Map<FieldKey, FieldSpec>();
	const unclaimed: FieldSpec[] = [];
	const ordered = sortScreens(screens);

	if (index) {
		for (const section of index.sections) {
			for (const spec of index.fieldsOf([section.section])) {
				specOf.set(spec.key, spec);
				let first: SettingsScreen | undefined;
				for (const screen of ordered) {
					const c = claims(screen);
					if (!c?.sections.includes(spec.section)) continue;
					if (c.groups && !c.groups.includes(spec.group)) continue;
					if (!first) first = screen;
					else if (first !== screen) {
						const names = doubleClaims.get(spec.key) ?? [first.id];
						names.push(screen.id);
						doubleClaims.set(spec.key, names);
					}
				}
				if (first) {
					owner.set(spec.key, first);
					(keysOf.get(first.id) ?? keysOf.set(first.id, []).get(first.id)!).push(spec.key);
				} else unclaimed.push(spec);
			}
		}
		for (const [key, names] of doubleClaims)
			devError(`${key} is claimed by ${names.join(' and ')}`);
	}

	const resolved: ResolvedScreen[] = ordered.map((screen) => ({
		screen,
		interim: false,
		mode: instantFor(screen.sections),
		keys: keysOf.get(screen.id) ?? []
	}));

	const registeredIds = new Set(ordered.map((s) => s.id));
	const bySection = new Map<string, FieldSpec[]>();
	for (const spec of unclaimed)
		(bySection.get(spec.section) ?? bySection.set(spec.section, []).get(spec.section)!).push(spec);
	const interim: ResolvedScreen[] = [];
	for (const [section, specs] of bySection) {
		if (registeredIds.has(section)) {
			devError(`section "${section}" has unclaimed fields but a screen already uses its id`);
			continue;
		}
		const label = index!.sections.find((s) => s.section === section)?.labelKey;
		interim.push({
			interim: true,
			mode: instantFor([section]),
			keys: specs.map((s) => s.key),
			screen: {
				id: section,
				group: INTERIM_GROUP[section] ?? 'system',
				title: () => (label ? settingText(label) : undefined) ?? section,
				sections: [section]
			}
		});
	}

	const all = [...resolved, ...interim];
	const sortedAll = sortScreens(all.map((r) => ({ ...r.screen, r }))).map((x) => x.r);
	const byId = new Map(sortedAll.map((r) => [r.screen.id, r]));
	for (const r of sortedAll) for (const key of r.keys) owner.set(key, r.screen);

	return {
		screens: sortedAll,
		doubleClaims,
		get: (id) => byId.get(id),
		fieldsFor: (screen) => {
			const r = byId.get(typeof screen === 'string' ? screen : screen.id);
			return (r?.keys ?? []).flatMap((k) => specOf.get(k) ?? []);
		},
		screenOf: (key) => {
			const s = owner.get(key);
			return s ? byId.get(s.id) : undefined;
		}
	};
}

function instantFor(sections: readonly string[] | undefined): 'staged' | 'instant' {
	return sections && sections.length > 0 && sections.every((s) => s === INSTANT_SECTION)
		? 'instant'
		: 'staged';
}

/** `fieldsFor(screen, index)`: a screen's fields without keeping a registry around. */
export function fieldsFor(
	screen: SettingsScreen,
	index: SettingsIndexView,
	screens: readonly SettingsScreen[] = registered
): FieldSpec[] {
	return buildRegistry(index, screens.includes(screen) ? screens : [...screens, screen]).fieldsFor(
		screen
	);
}
