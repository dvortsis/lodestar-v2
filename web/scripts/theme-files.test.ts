// @vitest-environment node
import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import type { Family } from './themes.ts';

const root = fileURLToPath(new URL('..', import.meta.url));

// Each web/themes/<id>.json must match the fixture family of the same id: same name, group and
// palettes. license and source are file-only metadata, so they are left out of the comparison.
function palette(f: Family) {
	return { id: f.id, name: f.name, group: f.group, light: f.light, dark: f.dark };
}

// Minimal reader for the yaml blocks in docs/themes.md. It handles only what those blocks use:
// top-level scalars, `light:`/`dark:` maps of quoted scalars, and a JSON-style categorical array.
function parseDocFamilies(md: string): Family[] {
	const blocks = [...md.matchAll(/```yaml\n([\s\S]*?)```/g)].map((m) => m[1]!);
	return blocks.map((block) => {
		const fam: Record<string, unknown> = {};
		let mode: 'light' | 'dark' | null = null;
		for (const raw of block.split('\n')) {
			if (!raw.trim()) continue;
			const indent = raw.length - raw.trimStart().length;
			const body = raw.trim();
			const colon = body.indexOf(':');
			const key = body.slice(0, colon);
			const value = body.slice(colon + 1).trim();
			const scalar = value.startsWith('"') ? JSON.parse(value) : value;
			if (indent === 0) {
				if (key === 'light' || key === 'dark') {
					mode = key;
					fam[key] = {};
				} else {
					fam[key] = scalar;
				}
			} else {
				const target = fam[mode!] as Record<string, unknown>;
				target[key] = key === 'categorical' ? JSON.parse(value) : scalar;
			}
		}
		return fam as unknown as Family;
	});
}

describe('web/themes/*.json match the docs/themes.md fixture', () => {
	const fixture = JSON.parse(
		readFileSync(join(root, 'scripts/fixtures/families.json'), 'utf8')
	) as Family[];
	const files = readdirSync(join(root, 'themes'))
		.filter((f) => f.endsWith('.json'))
		.sort();
	const fromFiles = files.map(
		(f) => JSON.parse(readFileSync(join(root, 'themes', f), 'utf8')) as Family
	);

	it('has one file per fixture family', () => {
		expect(files).toHaveLength(fixture.length);
		expect(files.sort()).toEqual(fixture.map((f) => `${f.id}.json`).sort());
	});

	it.each(fixture.map((f) => [f.id, f] as const))('%s matches the fixture', (id, expected) => {
		const actual = fromFiles.find((f) => f.id === id);
		expect(actual, `web/themes/${id}.json`).toBeDefined();
		expect(palette(actual!)).toEqual(palette(expected));
	});
});

describe('docs/themes.md matches the fixture', () => {
	const fixture = JSON.parse(
		readFileSync(join(root, 'scripts/fixtures/families.json'), 'utf8')
	) as Family[];
	const doc = parseDocFamilies(readFileSync(join(root, '..', 'docs', 'themes.md'), 'utf8'));

	it('has the same 24 families', () => {
		expect(doc.map((f) => f.id).sort()).toEqual(fixture.map((f) => f.id).sort());
	});

	it.each(fixture.map((f) => [f.id, f] as const))('%s matches its doc block', (id, expected) => {
		const actual = doc.find((f) => f.id === id);
		expect(actual, `docs/themes.md block ${id}`).toBeDefined();
		expect(palette(actual!)).toEqual(palette(expected));
	});
});
