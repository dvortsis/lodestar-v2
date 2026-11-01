import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { messageId } from './messages';

// B6 (M5 design review): an enum the generated form renders without an
// `settings_<section>_<field>_option_<value>` message shows the raw value ("daily", "starttls").
// Every enum in the golden index needs a message in en and zh, except the fields whose screens
// are hand-built and label their own options.
const GOLDEN = join(
	import.meta.dirname,
	'../../../../../../crates/lodestar/src/settings/golden/settings_index.txt'
);
// Exact field keys, each with the component that labels its values itself.
const HAND_BUILT = new Set([
	'updates.mode', // UpdatesScreen (modes)
	'updates.level', // UpdatesScreen (levels)
	'updates.security_fixes', // UpdatesScreen (security card)
	'updates.channel', // updates/Advanced.svelte
	'preferences.mode', // AppearanceScreen (modeOptions)
	'preferences.density', // AppearanceScreen (densityOptions)
	'preferences.motion', // AppearanceScreen (motionOptions)
	'logging.verbosity', // logging/VerbosityWidget.svelte
	'logging.temporary_verbosity', // no Settings screen renders it (the live log sets it)
	'logging.data' // logging/DataKinds.svelte (presets)
]);
const handBuilt = (field: string) => HAND_BUILT.has(field);

type Index = { sections: { section: string; fields: { name: string }[]; schema: unknown }[] };
const golden = (): Index => {
	const text = readFileSync(GOLDEN, 'utf8');
	return JSON.parse(text.slice(text.indexOf('\n') + 1)) as Index;
};
const messages = (lang: 'en' | 'zh') =>
	JSON.parse(readFileSync(join(import.meta.dirname, `../messages/${lang}.json`), 'utf8')) as Record<
		string,
		string
	>;

/** `section.field` and the enum's values, for every string enum in the index. */
function enums(): [string, string[]][] {
	const out: [string, string[]][] = [];
	for (const s of golden().sections) {
		const props =
			(s.schema as { properties?: Record<string, { enum?: unknown }> }).properties ?? {};
		for (const f of s.fields) {
			const e = props[f.name]?.enum;
			if (Array.isArray(e) && e.every((v) => typeof v === 'string'))
				out.push([`${s.section}.${f.name}`, e as string[]]);
		}
	}
	return out;
}

describe('enum option messages', () => {
	it('finds the enums the schema declares', () => {
		expect(enums().length).toBeGreaterThan(10);
	});

	for (const lang of ['en', 'zh'] as const) {
		it(`labels every enum value of a generated screen in ${lang}`, () => {
			const have = messages(lang);
			const missing = enums()
				.filter(([field]) => !handBuilt(field))
				.flatMap(([field, values]) =>
					values.map((v) => `settings_${messageId(field)}_option_${messageId(v)}`)
				)
				.filter((id) => !(id in have));
			expect(missing).toEqual([]);
		});
	}
});
