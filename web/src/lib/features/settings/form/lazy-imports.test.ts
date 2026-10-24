// Keeps what loads on demand out of a section's first view (README.md): no file outside the
// on-demand set imports one of those statically. A static import would bundle it eagerly.
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

const HERE = import.meta.dirname;

/** Loaded only through `import()` (controls.svelte.ts, form.svelte.ts, SettingField, SettingsForm). */
const ON_DEMAND = [
	'NumberControl.svelte',
	'SliderControl.svelte',
	'ChoiceControl.svelte',
	'SecretControl.svelte',
	'ChipsControl.svelte',
	'JsonControl.svelte',
	'instant.ts',
	'preview.ts',
	'FieldPreview.svelte',
	'menu.ts',
	'FieldMenu.svelte',
	'FieldKeys.svelte',
	'save-key.ts',
	'copy.ts'
];

const STATIC = /(?:^|\n)\s*import\s+(?!type\b)[^'"]*?from\s+['"]\.\/([^'"]+)['"]/g;
const sources = readdirSync(HERE).filter(
	(f) => /\.(ts|svelte)$/.test(f) && !f.endsWith('.test.ts') && !f.endsWith('.gen.ts')
);

describe('on-demand files', () => {
	it.each(sources.filter((f) => !ON_DEMAND.includes(f)))(
		'%s imports none of them statically',
		(file) => {
			const text = readFileSync(join(HERE, file), 'utf8');
			const imported = [...text.matchAll(STATIC)].map((m) => m[1]!.replace(/\.js$/, ''));
			const lazy = imported.filter((p) =>
				ON_DEMAND.some((f) => f === p || f.replace(/\.ts$/, '') === p)
			);
			expect(lazy).toEqual([]);
		}
	);
});
