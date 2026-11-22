// Finding a setting (§10.10.3, UI review I12): one index of every Settings screen and every field,
// built the first time the box is focused or the command palette is searched, in the current
// locale. It holds labels, help, group names, `config.toml` keys, `LODESTAR_*` names and keywords,
// never a value: a result's value is read when it is shown (`describe`), and a secret's only as
// "Set" or "Not set". This whole module loads on demand; the shell carries none of it.
import { m } from '$lib/paraglide/messages.js';
import { displayValue } from '../form/display';
import { loadMessages, fieldHelp, fieldLabel, settingText } from '../form/messages';
import { getSection } from '../form/requests';
import { fieldAnchor, loadSettingsIndex } from '../form/schema';
import type { FieldKey, FieldSpec, SettingSource } from '../form/types';
import { aboutOf, groups, INERT, screens, type Screen } from './screens';

export interface FindEntry {
	/** The screen's id, which is `/settings/<screen>`. */
	screen: string;
	screenTitle: string;
	/** Absent for an entry that is a whole screen. */
	spec?: FieldSpec;
	/** The label, or the screen's title. */
	label: string;
	/** "Crawling › Crawler › Advanced": where it lives. */
	trail: string;
	/** `/settings/<screen>` or `/settings/<screen>#<field>`. */
	href: string;
	/** What the matching looks at, normalized. Never a value. */
	hay: string;
	/** Just the label, for ranking. */
	head: string;
}

/** Sections no screen lists belong to Appearance (the Overview does the same). */
const SCREEN_OF: Record<string, string> = { content: 'appearance', preferences: 'appearance' };

const screenOfSection = (section: string): Screen | undefined => {
	const id =
		screens.find((s) => s.sections?.includes(section))?.id ??
		screens.find((s) => s.id === section)?.id ??
		SCREEN_OF[section];
	return screens.find((s) => s.id === id);
};

/** Case and accent insensitive, so "Réseau" and "reseau" meet. */
export const normalize = (s: string): string =>
	s.normalize('NFD').replace(/\p{M}/gu, '').toLocaleLowerCase();

const words = (s: string): string[] => s.split(/[^\p{L}\p{N}]+/u).filter(Boolean);
const CJK = /[\p{Script=Han}\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Hangul}]/u;

/**
 * Whether every word of the query is in the text: a prefix of one of its words for Latin text, and
 * a substring for Chinese, Japanese and Korean, which have no spaces to prefix by.
 */
export function matches(query: string, hay: string): boolean {
	const q = words(normalize(query));
	if (!q.length) return false;
	const hayWords = words(hay);
	return q.every((w) => (CJK.test(w) ? hay.includes(w) : hayWords.some((h) => h.startsWith(w))));
}

const trailOf = (screen: Screen, advanced: boolean): string =>
	[
		groups.find((g) => g.id === screen.group)?.label(),
		screen.title(),
		advanced ? m.settings_find_advanced() : undefined
	]
		.filter(Boolean)
		.join(' › ');

const rank = (query: string, e: FindEntry): number => (matches(query, e.head) ? 0 : 1);

function fieldEntry(spec: FieldSpec, screen: Screen, group: string | undefined): FindEntry {
	const label = fieldLabel(spec);
	const keywords = spec.keywordsKey ? (settingText(spec.keywordsKey) ?? '') : '';
	return {
		screen: screen.id,
		screenTitle: screen.title(),
		spec,
		label,
		trail: trailOf(screen, spec.advanced),
		href: `/settings/${screen.id}#${fieldAnchor(spec.key)}`,
		hay: normalize(
			[label, fieldHelp(spec), group, screen.title(), spec.config, spec.env, spec.key, keywords]
				.filter(Boolean)
				.join(' ')
		),
		head: normalize(label)
	};
}

let built: Promise<FindEntry[]> | undefined;

/** Every screen and every field, once per tab and locale. A failure isn't kept, so the next call retries. */
export function loadFindIndex(): Promise<FindEntry[]> {
	built ??= (async () => {
		const index = await loadSettingsIndex();
		const names = index.sections.map((s) => s.section);
		await loadMessages(names);
		const out: FindEntry[] = [];
		for (const screen of screens) {
			const about = await aboutOf(screen).catch(() => ({}) as { purpose?: () => string });
			out.push({
				screen: screen.id,
				screenTitle: screen.title(),
				label: screen.title(),
				trail: trailOf(screen, false),
				href: `/settings/${screen.id}`,
				hay: normalize(`${screen.title()} ${about.purpose?.() ?? ''}`),
				head: normalize(screen.title())
			});
		}
		for (const name of names) {
			const screen = screenOfSection(name);
			if (!screen) continue;
			for (const spec of index.fieldsOf([name])) {
				if (INERT.has(spec.key)) continue;
				const group = settingText(`settings.group.${spec.group}`);
				out.push(fieldEntry(spec, screen, group));
			}
		}
		return out;
	})().catch((e: unknown) => {
		built = undefined;
		throw e;
	});
	return built;
}

/** Tests only: forget the index (the locale or the schema changed). */
export function clearFindIndex(): void {
	built = undefined;
}

/** The entries that match, label matches first, otherwise in registry order; at most `limit`. */
export function search(entries: readonly FindEntry[], query: string, limit = 30): FindEntry[] {
	if (!query.trim()) return [];
	return entries
		.map((e, i) => ({ e, i }))
		.filter(({ e }) => matches(query, e.hay))
		.sort((a, b) => rank(query, a.e) - rank(query, b.e) || a.i - b.i)
		.slice(0, limit)
		.map(({ e }) => e);
}

/** A source badge's text. */
export function sourceText(source: SettingSource): string {
	if (source === 'db') return m.settings_overview_source_db();
	if (source === 'file') return m.settings_overview_source_file();
	if (source === 'env') return m.settings_overview_source_env_unnamed();
	return m.settings_find_source_default();
}

export interface Described {
	value: string;
	source: string;
}

/**
 * What a result shows for a field: its current value and where it comes from. A secret (a
 * write-only field, or one that holds a credential) is never read out: only "Set" or "Not set".
 */
export function describeValue(spec: FieldSpec, value: unknown, source: SettingSource): Described {
	const secret = spec.writeOnly || spec.holdsSecret === true;
	return {
		value: secret
			? source === 'default'
				? m.settings_find_secret_unset()
				: m.settings_overview_secret_set()
			: displayValue(spec, value),
		source: sourceText(source)
	};
}

const sections = new Map<string, ReturnType<typeof getSection>>();

/** Forget the values read for results (a box that closes, so the next search reads fresh ones). */
export const forgetValues = () => sections.clear();

/** The value of a result, read when it's shown. A screen entry, or a failed read, has none. */
export async function describe(entry: FindEntry): Promise<Described | undefined> {
	const spec = entry.spec;
	if (!spec) return undefined;
	try {
		let read = sections.get(spec.section);
		if (!read) sections.set(spec.section, (read = getSection(spec.section)));
		const values = await read.catch((e: unknown) => {
			sections.delete(spec.section);
			throw e;
		});
		const v = (values.fields as Record<string, { value: unknown; source: SettingSource }>)[
			spec.name
		];
		return v ? describeValue(spec, v.value, v.source) : undefined;
	} catch {
		return undefined;
	}
}

/**
 * After a jump: wait for the field to be on the page, then show where it is for about two
 * seconds. With reduced motion it's a still outline; otherwise the outline fades. The screen's own
 * hash handling has already scrolled to it and focused it. Gives up after `within` ms.
 */
export function highlight(key: FieldKey, within = 10_000): () => void {
	let stop = false;
	const reduced = matchMedia('(prefers-reduced-motion: reduce)').matches;
	const started = performance.now();
	let timer: ReturnType<typeof setTimeout> | undefined;
	const mark = (el: HTMLElement) => {
		el.style.outline = '2px solid var(--color-primary)';
		el.style.outlineOffset = 'calc(var(--spacing) * 2)';
		const clear = () => {
			el.style.outline = '';
			el.style.outlineOffset = '';
		};
		const anim = reduced
			? undefined
			: el.animate?.([{ outlineColor: 'var(--color-primary)' }, { outlineColor: 'transparent' }], {
					duration: 2000,
					easing: 'ease'
				});
		if (anim) anim.onfinish = clear;
		else timer = setTimeout(clear, 2000);
	};
	const look = () => {
		if (stop) return;
		const el = document.getElementById(key);
		if (el?.querySelector('[data-control]')) return mark(el);
		if (performance.now() - started < within) requestAnimationFrame(look);
	};
	look();
	return () => {
		stop = true;
		clearTimeout(timer);
	};
}
