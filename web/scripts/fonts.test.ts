// @vitest-environment node
// The committed subsets keep what the UI relies on: the tnum feature (tabular numerals for
// sizes, counts and times), the wght axis, and the budgets (§10.1).
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
// @ts-expect-error fontverter ships no types; it is subset-font's own converter.
import fontverter from 'fontverter';

const load = (name: string) => readFileSync(`static/fonts/${name}.woff2`);

/** Table directory of an sfnt: tag -> [offset, length]. */
function tables(sfnt: Buffer) {
	const n = sfnt.readUInt16BE(4);
	const out = new Map<string, [number, number]>();
	for (let i = 0; i < n; i++) {
		const at = 12 + i * 16;
		out.set(sfnt.toString('latin1', at, at + 4), [
			sfnt.readUInt32BE(at + 8),
			sfnt.readUInt32BE(at + 12)
		]);
	}
	return out;
}

/** Feature tags in the GSUB FeatureList. */
function gsubFeatures(sfnt: Buffer): string[] {
	const t = tables(sfnt).get('GSUB');
	if (!t) return [];
	const list = t[0] + sfnt.readUInt16BE(t[0] + 6);
	const count = sfnt.readUInt16BE(list);
	return Array.from({ length: count }, (_, i) =>
		sfnt.toString('latin1', list + 2 + i * 6, list + 6 + i * 6)
	);
}

async function sfntOf(name: string) {
	return Buffer.from(await fontverter.convert(load(name), 'sfnt'));
}

describe('font subsets', () => {
	it('keeps the tnum feature in the first-paint UI font', async () => {
		// Latin-Extended carries no digits, so only the Latin subset has tnum lookups.
		expect(gsubFeatures(await sfntOf('atkinson-hyperlegible-next-latin'))).toContain('tnum');
	});

	it('stays variable (fvar present) with the weight axis', async () => {
		for (const name of ['atkinson-hyperlegible-next-latin', 'atkinson-hyperlegible-mono-latin']) {
			const sfnt = await sfntOf(name);
			const fvar = tables(sfnt).get('fvar');
			expect(fvar, name).toBeDefined();
		}
	});

	it('fits the §10.1 budgets', () => {
		expect(load('atkinson-hyperlegible-next-latin').length).toBeLessThanOrEqual(35 * 1024);
		expect(load('atkinson-hyperlegible-mono-latin').length).toBeLessThanOrEqual(25 * 1024);
	});
});
