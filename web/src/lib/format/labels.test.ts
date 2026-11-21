// @vitest-environment node
// One label formatter per field (X-labels): every value the server can send has a label, read
// against the vocabularies in `crates/` at test time, and anything else shows as sent.
import { readdirSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { afterEach, describe, expect, it } from 'vitest';
import { setLocale } from '$lib/paraglide/runtime.js';
import {
	adultTypeLabel,
	audioCodecLabel,
	fieldLabel,
	groupLabel,
	hdrLabel,
	languageLabel,
	releaseSourceLabel,
	sourceIdLabel,
	sourceLabel,
	themeLabel,
	typeLabel,
	valueLabel,
	videoCodecLabel
} from './labels';

const crates = fileURLToPath(new URL('../../../../crates/', import.meta.url));
const read = (path: string) => readFileSync(crates + path, 'utf8');

/** The strings of a `str_enum!` block in the release parser (`Name { A => "a", … }`). */
function parserValues(name: string): string[] {
	const source = read('release-parser/src/lib.rs');
	const start = source.search(new RegExp(`\\n\\s*${name} \\{`));
	expect(start, `release-parser ${name}`).toBeGreaterThan(0);
	const body = source.slice(start, source.indexOf('}', source.indexOf('{', start)));
	const values = [...body.matchAll(/=> "([^"]+)"/g)].map((x) => x[1]!);
	expect(values.length, `release-parser ${name}`).toBeGreaterThan(3);
	return values;
}

/** Every plugin manifest in `crates/sources-plugins`: its id and its name. */
function manifests(): [string, string][] {
	const dir = 'sources-plugins/src/';
	const found: [string, string][] = [];
	for (const file of readdirSync(crates + dir).filter((f) => f.endsWith('.rs'))) {
		const source = read(dir + file);
		const consts = new Map(
			[...source.matchAll(/const (\w+): &str = "([^"]+)";/g)].map((x) => [x[1]!, x[2]!])
		);
		for (const x of source.matchAll(/\bid: (\w+|"[^"]+"),\s*\n\s*name: "([^"]+)"/g)) {
			const id = x[1]!.startsWith('"') ? x[1]!.slice(1, -1) : consts.get(x[1]!);
			expect(id, `${file}: the manifest's id ${x[1]}`).toBeDefined();
			found.push([id!, x[2]!]);
		}
	}
	return found;
}

/** The query fields the Builder lists (`crates/search/src/query.rs` FIELDS), aliases left out. */
function queryFields(): string[] {
	const source = read('search/src/query.rs');
	const table = source.slice(
		source.indexOf('const FIELDS'),
		source.indexOf('];', source.indexOf('const FIELDS'))
	);
	const rows = [...table.matchAll(/\("([a-z0-9_]+)", (FieldKind::[^)]*\)?)\)/g)].map(
		(x) => [x[1]!, x[2]!] as const
	);
	expect(rows.length).toBeGreaterThan(40);
	// A later name for a kind already listed is an alias (`age` for `discovered`), as the catalogue reads it.
	return rows
		.filter(([, kind], i) => !rows.slice(0, i).some(([, k]) => k === kind))
		.map(([n]) => n);
}

afterEach(() => setLocale('en', { reload: false }));

describe('the release parser vocabulary', () => {
	const cases: [string, (v: string) => string][] = [
		['VideoCodec', videoCodecLabel],
		['AudioCodec', audioCodecLabel],
		['Hdr', hdrLabel],
		['Source', releaseSourceLabel]
	];
	for (const [name, label] of cases)
		it(`labels every ${name} value the parser emits, in en and zh`, () => {
			for (const locale of ['en', 'zh'] as const) {
				setLocale(locale, { reload: false });
				const missing = parserValues(name).filter((v) => !label(v).trim() || label(v) === v);
				expect(missing, `${name} values with no label in ${locale}`).toEqual([]);
			}
		});
	it('names codecs, HDR and sources as written', () => {
		expect(videoCodecLabel('h264')).toBe('H.264');
		expect(audioCodecLabel('dts-hd-ma')).toBe('DTS-HD MA');
		expect(hdrLabel('dv')).toBe('Dolby Vision');
		expect(releaseSourceLabel('web-dl')).toBe('WEB-DL');
		expect(releaseSourceLabel('cam')).toBe('Camcorder');
		setLocale('zh', { reload: false });
		expect(releaseSourceLabel('cam')).toBe('摄像机录制');
		expect(videoCodecLabel('h264')).toBe('H.264');
	});
});

describe('languages', () => {
	it('names a code in the UI language', () => {
		expect(languageLabel('fr', 'en')).toBe('French');
		expect(languageLabel('zh', 'en')).toBe('Chinese');
		expect(languageLabel('fr', 'zh')).toBe('法语');
	});
	it('names multi-language releases', () => {
		expect(languageLabel('multi', 'en')).toBe('Multiple languages');
		setLocale('zh', { reload: false });
		expect(languageLabel('multi', 'zh')).toBe('多种语言');
	});
	it('falls back to the code for a code with no name or a malformed one', () => {
		expect(languageLabel('qq', 'en')).toBe('qq');
		expect(languageLabel('not a tag!', 'en')).toBe('not a tag!');
		expect(languageLabel('', 'en')).toBe('');
	});
});

describe('sources', () => {
	it('names every plugin by its manifest name', () => {
		const wrong = manifests().filter(([id, name]) => sourceIdLabel(id) !== name);
		expect(manifests().length).toBeGreaterThan(15);
		expect(wrong, 'plugin ids whose label differs from their manifest name').toEqual([]);
	});
	it('writes the DHT as the glossary does, whatever the server called it', () => {
		expect(sourceLabel('dht')).toBe('DHT');
		expect(sourceLabel('dht', 'Mainline DHT')).toBe('DHT');
		expect(sourceLabel('x', 'Mainline DHT')).toBe('DHT');
	});
	it('names the importers in lower case, as the projects write them', () => {
		expect(sourceLabel('bitmagnet')).toBe('bitmagnet import');
		expect(sourceLabel('magnetico')).toBe('magnetico import');
	});
	it("prefers the server's name, unless it's only the id; else the id", () => {
		expect(sourceLabel('nyaa', 'Nyaa (mirror)')).toBe('Nyaa (mirror)');
		expect(sourceLabel('nyaa', 'nyaa')).toBe('nyaa');
		// With ids alone (Insights), the manifest's name.
		expect(sourceIdLabel('nyaa')).toBe('Nyaa');
		expect(sourceIdLabel('dht')).toBe('DHT');
		expect(sourceIdLabel('bitmagnet')).toBe('bitmagnet import');
		expect(sourceIdLabel('gone')).toBe('gone');
		expect(sourceLabel('gone', 'gone')).toBe('gone');
		expect(sourceLabel('gone')).toBe('gone');
	});
});

describe('types, groups and fields', () => {
	it('uses the glossary type labels, one spelling', () => {
		expect(typeLabel('ebook')).toBe('Ebook');
		expect(typeLabel('video')).toBe('Video');
		expect(adultTypeLabel('vr')).toBe('VR');
		expect(adultTypeLabel('later')).toBe('later');
		expect(typeLabel('later')).toBe('later');
	});
	it('keeps a release group as written', () => {
		expect(groupLabel('RARBG')).toBe('RARBG');
		expect(groupLabel('rarbg')).toBe('rarbg');
	});
	it('labels every query field the Builder lists', () => {
		const missing = queryFields().filter((f) => fieldLabel(f) === f || !fieldLabel(f).trim());
		expect(missing, 'query fields with no label').toEqual([]);
		expect(fieldLabel('res')).toBe('Resolution');
		expect(fieldLabel('name')).toBe('Raw name');
		expect(fieldLabel('seeders')).toBe('Seeders');
		expect(fieldLabel('later')).toBe('later');
	});
	it('labels a value by its field, and leaves fields with no table as sent', () => {
		expect(valueLabel('codec', 'h265', 'en')).toBe('H.265 (HEVC)');
		expect(valueLabel('audio', 'truehd', 'en')).toBe('Dolby TrueHD');
		expect(valueLabel('lang', 'de', 'en')).toBe('German');
		expect(valueLabel('type', 'tv', 'en')).toBe('TV');
		expect(valueLabel('res', '2160p', 'en')).toBe('2160p');
		expect(valueLabel('ext', 'mkv', 'en')).toBe('mkv');
	});
});

describe('themes', () => {
	const themes = [
		{ id: 'ayu-light', family: 'Ayu' },
		{ id: 'ayu-dark', family: 'Ayu' },
		{ id: 'my-theme', family: 'My theme' }
	];
	it("shows a theme's own display name", () => {
		expect(themeLabel('riverstone', themes)).toBe('Riverstone');
		expect(themeLabel('ayu', themes)).toBe('Ayu');
		expect(themeLabel('ayu-dark', themes)).toBe('Ayu');
		expect(themeLabel('my-theme', themes)).toBe('My theme');
	});
	it('shows the id of a theme it has no name for', () => {
		expect(themeLabel('gone', themes)).toBe('gone');
		expect(themeLabel('gone', [])).toBe('gone');
	});
});

describe('no inherited names', () => {
	it('never reads a code through the prototype', () => {
		for (const code of ['constructor', '__proto__', 'toString', 'hasOwnProperty']) {
			expect(videoCodecLabel(code)).toBe(code);
			expect(audioCodecLabel(code)).toBe(code);
			expect(hdrLabel(code)).toBe(code);
			expect(releaseSourceLabel(code)).toBe(code);
			expect(sourceLabel(code)).toBe(code);
			expect(sourceIdLabel(code)).toBe(code);
			expect(fieldLabel(code)).toBe(code);
			expect(typeLabel(code)).toBe(code);
		}
	});
});
