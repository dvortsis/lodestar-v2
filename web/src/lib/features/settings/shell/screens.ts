// The Settings screen registry (§10.10.2): which screens exist, in which group and order. A feature
// registers a screen by exporting `screen` (one), `screens` (several) or a default of either from a
// tiny `screen.ts`: `features/settings/<part>/screen.ts` or `features/<feature>/screen.ts`.
// `/settings/[section]` finds it here. Keep screen.ts small, since every Settings page loads it: a
// hand-built screen's body and a screen's extras are dynamic imports (`load`, `extra`).
import type { Component } from 'svelte';
import { m } from '$lib/paraglide/messages.js';
import type { FieldSpec } from '../form/types';
import type { SettingsIndexView } from '../form/schema';

export const groups = [
	{ id: 'preferences', label: () => m.settings_shell_preferences() },
	{ id: 'crawling', label: () => m.settings_shell_crawling() },
	{ id: 'library', label: () => m.settings_shell_library() },
	{ id: 'connections', label: () => m.settings_shell_connections() },
	{ id: 'system', label: () => m.settings_shell_system() }
] as const;

export type ScreenGroup = (typeof groups)[number]['id'];

/** The order inside each group (§10.10.2); an id not listed goes last, by title. */
export const SCREEN_ORDER: Record<ScreenGroup, readonly string[]> = {
	preferences: ['appearance', 'keyboard', 'language'],
	crawling: ['crawler', 'network', 'sources', 'workers', 'companions'],
	library: ['storage', 'search', 'trackers', 'filters'],
	connections: ['integrations', 'notifications', 'insights'],
	system: ['general', 'updates', 'logging', 'security', 'about']
};

export interface Screen {
	/** The URL segment: `/settings/<id>`. */
	id: string;
	group: ScreenGroup;
	title: () => string;
	/** The one-sentence purpose under a generated screen's heading, also matched by the filter. */
	purpose?: () => string;
	/** A `~icons/ph/*` component; the gear when absent. */
	icon?: Component;
	/** The same, loaded after the navigation shows (the gear holds its place), for a screen whose icon
	 * would otherwise weigh on every Settings page. */
	iconLoad?: () => Promise<{ default: Component }>;
	/**
	 * A hand-built screen, loaded when it opens. It renders everything below the shell, its own
	 * `<h1>` included. Name its file `*Screen.svelte`: `.size-limit.mjs` keeps it off a section's
	 * first view by that name.
	 */
	load?: () => Promise<{ default: Component }>;
	/** A generated screen: the schema sections whose fields the form engine shows. */
	sections?: readonly string[];
	/** Every field of a generated screen is read-only (its guarded flow isn't built yet). */
	readOnly?: boolean;
	/** A line above a generated screen's form (why it's read-only, say). */
	notice?: () => string;
	/** A panel after a generated screen's form, loaded once the form shows. */
	extra?: () => Promise<{ default: Component }>;
}

const isScreen = (v: unknown): v is Screen =>
	typeof v === 'object' &&
	v !== null &&
	typeof (v as Screen).id === 'string' &&
	groups.some((g) => g.id === (v as Screen).group) &&
	typeof (v as Screen).title === 'function' &&
	(typeof (v as Screen).load === 'function' ||
		(Array.isArray((v as Screen).sections) && (v as Screen).sections!.length > 0));

/**
 * The screens these modules export, in navigation order. A screen exported under two names counts
 * once; anything that isn't a screen, and a repeated id after its first, is left out (and named in
 * development).
 */
export function collect(modules: Record<string, unknown>): Screen[] {
	const seen = new Set<unknown>();
	const ids = new Set<string>();
	const out: Screen[] = [];
	const take = (value: unknown, path: string) => {
		for (const v of Array.isArray(value) ? value : [value]) {
			if (v === undefined || seen.has(v)) continue;
			seen.add(v);
			if (isScreen(v) && !ids.has(v.id)) {
				ids.add(v.id);
				out.push(v);
			} else if (import.meta.env.DEV) {
				console.error(`settings: ${path} exports an invalid or repeated screen`);
			}
		}
	};
	for (const [path, mod] of Object.entries(modules).sort(([a], [b]) => a.localeCompare(b))) {
		const exported = (mod ?? {}) as Record<string, unknown>;
		take(exported.screens, path);
		take(exported.screen, path);
		take(exported.default, path);
	}
	const group = (s: Screen) => groups.findIndex((g) => g.id === s.group);
	const rank = (s: Screen) => {
		const at = SCREEN_ORDER[s.group].indexOf(s.id);
		return at < 0 ? SCREEN_ORDER[s.group].length : at;
	};
	return out.sort(
		(a, b) => group(a) - group(b) || rank(a) - rank(b) || a.title().localeCompare(b.title())
	);
}

export const screens: readonly Screen[] = collect(
	import.meta.glob(['/src/lib/features/settings/*/screen.ts', '/src/lib/features/*/screen.ts'], {
		eager: true
	})
);

export function screenFields(screen: Screen, index: SettingsIndexView): FieldSpec[] {
	return index.fieldsOf([...(screen.sections ?? [])]).map((spec) => ({
		...spec,
		readOnly:
			spec.readOnly ||
			screen.readOnly === true ||
			(spec.danger !== undefined && spec.danger !== 'disruptive')
	}));
}

/** Navigation matching does not load any setting values or private data. */
export function matchesScreen(screen: Screen, query: string): boolean {
	const normalize = (s: string) => s.normalize('NFD').replace(/\p{M}/gu, '').toLocaleLowerCase();
	const haystack = normalize(`${screen.title()} ${screen.purpose?.() ?? ''}`);
	return normalize(query)
		.trim()
		.split(/\s+/)
		.every((word) => haystack.includes(word));
}
