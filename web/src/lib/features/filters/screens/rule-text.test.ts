// @vitest-environment node
// Every built-in rule and pack has a plain name and a description in every locale (I5), read
// against the ids `crates/filter` defines, so a rule added there without words here fails.
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { afterEach, describe, expect, it } from 'vitest';
import { setLocale } from '$lib/paraglide/runtime.js';
import { ids, nameOf, textOf } from './rule-text';

const crate = (path: string) =>
	readFileSync(
		fileURLToPath(new URL(`../../../../../../crates/filter/src/${path}`, import.meta.url)),
		'utf8'
	);

/** The strings `fn <name>(self) -> &'static str` returns in `impl <type>`, in its match. */
function idsOf(source: string, type: string, fn: string): string[] {
	const impl = source.indexOf(`impl ${type} {`);
	expect(impl, `impl ${type}`).toBeGreaterThanOrEqual(0);
	const start = source.indexOf(`pub fn ${fn}(self) -> &'static str {`, impl);
	expect(start, `${type}::${fn}`).toBeGreaterThan(impl);
	const body = source.slice(start, source.indexOf('\n    }\n', start));
	const found = [...body.matchAll(new RegExp(`${type}::\\w+ => "([^"]+)"`, 'g'))].map((x) => x[1]!);
	// As many as the type's `ALL` lists: none missed by the pattern.
	const all = new RegExp(`pub const ALL: \\[${type}; (\\d+)\\]`).exec(source);
	expect(all, `${type}::ALL`).not.toBeNull();
	expect(found).toHaveLength(Number(all![1]));
	return found;
}

const core = idsOf(crate('core.rs'), 'CoreRule', 'id');
const packs = crate('packs/mod.rs');
const builtIn = [...core, ...idsOf(packs, 'PackRule', 'id'), ...idsOf(packs, 'Pack', 'as_str')];

afterEach(() => setLocale('en', { reload: false }));

describe('rule and pack texts', () => {
	it('reads the built-in ids from crates/filter', () => {
		expect(core).toContain('CS-1a');
		expect(core).toContain('CS-0a');
		expect(builtIn).toContain('MW-6');
		expect(builtIn).toContain('unauthorised-copy');
		expect(builtIn).toHaveLength(10 + 23 + 6);
	});
	it('names every built-in id, and no id the build no longer has', () => {
		const missing = builtIn.filter((id) => !ids.includes(id));
		const extra = ids.filter((id) => !builtIn.includes(id));
		expect(missing, 'built-in rule or pack ids with no name or description').toEqual([]);
		expect(extra, 'names for ids crates/filter no longer defines').toEqual([]);
	});
	for (const locale of ['en', 'zh'] as const)
		it(`gives every built-in id a name and a description in ${locale}`, () => {
			setLocale(locale, { reload: false });
			for (const id of builtIn) {
				const name = nameOf(id);
				const text = textOf(id);
				expect(name?.trim(), `${id} name`).toBeTruthy();
				expect(text?.trim(), `${id} text`).toBeTruthy();
				expect(name, `${id} name is its code`).not.toBe(id);
				expect(text, `${id}`).not.toBe(name);
			}
		});
	it('has the zh words, not the en ones, in zh', () => {
		setLocale('zh', { reload: false });
		expect(nameOf('CS-4a')).not.toBe('Child terms near sexual terms');
		expect(nameOf('malware')).toBe('恶意软件');
	});
	it('has no words for an unknown code or an inherited property name', () => {
		for (const id of ['CS-9z', '', 'constructor', '__proto__', 'toString', 'cs-1a']) {
			expect(nameOf(id), id).toBeNull();
			expect(textOf(id), id).toBeNull();
		}
	});
});
