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
