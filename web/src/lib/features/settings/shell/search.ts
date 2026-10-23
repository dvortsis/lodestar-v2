// Settings search (§10.10.3): an in-house index over every field of every screen and the
// hand-built screens' own entries. Case- and accent-insensitive, matching on word prefixes in the
// current locale; a label match ranks above a keyword, config key or variable match, above the
// group, above the help. Loaded when the search box is first focused.
import { m } from '$lib/paraglide/messages.js';
import type { FieldKey, FieldSpec } from '../form/types';
import { fieldHelp, fieldLabel, settingText } from '../form/messages';
import { groupHeading } from './groups';
import type { Registry } from './registry';

export interface SearchItem {
	id: string;
	kind: 'screen' | 'field' | 'entry';
	screenId: string;
	/** The field, for a schema field. */
	key?: FieldKey;
	spec?: FieldSpec;
	/** The hash to open: the field key, or a hand-built screen's own anchor. */
	anchor?: string;
	label: string;
	help?: string;
	/** "Crawling › Crawler › Advanced". */
	crumb: string;
	/** Keywords, the `config.toml` key and the `LODESTAR_*` name. */
	names: string[];
	group?: string;
}

export interface Hit {
	item: SearchItem;
	score: number;
}

const COMBINING = /\p{M}/gu;
const SPLIT = /[^\p{L}\p{N}]+/u;
const CJK = /[\p{Script=Han}\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Hangul}]/u;

/** NFD, combining marks dropped, lower-cased in `locale`: "Éclair" and "eclair" are the same. */
export function normalize(text: string, locale = 'en'): string {
	return text.normalize('NFD').replace(COMBINING, '').toLocaleLowerCase(locale);
}

/**
 * The words a text can be found by: its alphanumeric runs, plus a whole `a.b` / `A_B` name, so
 * both "dht" and "dht_port" find `LODESTAR_DHT_PORT`.
 */
export function wordsOf(text: string, locale = 'en'): string[] {
	const n = normalize(text, locale);
	const parts = n.split(SPLIT).filter(Boolean);
	const whole = n.split(/[\s,;]+/).filter((w) => /[._-]/.test(w));
	return [...new Set([...parts, ...whole])];
}

const hasPrefix = (words: string[], q: string) => words.some((w) => w.startsWith(q));

export interface SearchIndex {
	items: readonly SearchItem[];
	search(query: string): Hit[];
}

interface Prepared {
	item: SearchItem;
	label: string;
	labelWords: string[];
	nameWords: string[];
	groupWords: string[];
	helpWords: string[];
	help: string;
}

export function createIndex(items: readonly SearchItem[], locale = 'en'): SearchIndex {
	const prepared: Prepared[] = items.map((item) => ({
		item,
		label: normalize(item.label, locale),
		labelWords: wordsOf(item.label, locale),
		nameWords: item.names.flatMap((n) => wordsOf(n, locale)),
		groupWords: wordsOf(`${item.group ?? ''} ${item.crumb}`, locale),
		helpWords: wordsOf(item.help ?? '', locale),
		help: normalize(item.help ?? '', locale)
	}));
	return {
		items,
		search(query) {
			const qs = normalize(query, locale).split(SPLIT).filter(Boolean);
			if (qs.length === 0) return [];
			const hits: Hit[] = [];
			for (const p of prepared) {
				let score = 0;
				let all = true;
				for (const q of qs) {
					const cjk = CJK.test(q);
					let s = 0;
					if (hasPrefix(p.labelWords, q) || (cjk && p.label.includes(q))) {
						s = p.labelWords[0]?.startsWith(q) ? 120 : 100;
					} else if (hasPrefix(p.nameWords, q)) s = 60;
					else if (hasPrefix(p.groupWords, q)) s = 40;
					else if (hasPrefix(p.helpWords, q) || (cjk && p.help.includes(q))) s = 20;
					if (s === 0) {
						all = false;
						break;
					}
					score += s;
				}
				if (!all) continue;
				// A label that is exactly the query is the best answer.
				if (p.label === normalize(query.trim(), locale)) score += 200;
				hits.push({ item: p.item, score });
			}
			// Stable: the index is in screen order, so equal scores keep it.
			return hits.sort((a, b) => b.score - a.score);
		}
	};
}

/** Every searchable thing in the registry, in screen order. */
export function buildItems(registry: Registry): SearchItem[] {
	const items: SearchItem[] = [];
	for (const r of registry.screens) {
		const group = groupHeading[r.screen.group]();
		const title = r.screen.title();
		items.push({
			id: `screen:${r.screen.id}`,
			kind: 'screen',
			screenId: r.screen.id,
			label: title,
			crumb: `${group} › ${title}`,
			names: [r.screen.id]
		});
		for (const spec of registry.fieldsFor(r.screen.id)) {
			const keywords = spec.keywordsKey ? (settingText(spec.keywordsKey) ?? '') : '';
			items.push({
				id: spec.key,
				kind: 'field',
				screenId: r.screen.id,
				key: spec.key,
				spec,
				anchor: spec.key,
				label: fieldLabel(spec),
				help: fieldHelp(spec),
				crumb: `${group} › ${title}${spec.advanced ? ` › ${m.settings_ui_crumb_advanced()}` : ''}`,
				names: [...keywords.split(','), spec.config ?? '', spec.env ?? ''].filter(Boolean),
				group: settingText(`settings.group.${spec.group}`)
			});
		}
		for (const entry of r.screen.search?.() ?? []) {
			items.push({
				id: `${r.screen.id}:${entry.id}`,
				kind: 'entry',
				screenId: r.screen.id,
				anchor: entry.anchor,
				label: entry.label(),
				names: entry.words?.() ?? [],
				crumb: `${group} › ${title}`
			});
		}
	}
	return items;
}
