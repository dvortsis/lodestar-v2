// Keeps what loads on demand out of a section's first view (README.md): no file outside the
// on-demand set imports one of those statically. A static import would bundle it eagerly.
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

const HERE = import.meta.dirname;

/**
 * Loaded only through `import()` (controls.svelte.ts, form.svelte.ts, SettingField, SettingsForm),
 * or only by files that are.
 */
const ON_DEMAND = [
	'NumberControl.svelte',
	'SliderControl.svelte',
	'ChoiceControl.svelte',
	'SecretControl.svelte',
	'ChipsControl.svelte',
	'JsonControl.svelte',
	'edit.ts',
	'saving.ts',
	'draft.ts',
	'merge.ts',
	'validate.ts',
	'batch.ts',
	'display.ts',
	'units.ts',
	'questions.ts',
	'dialogs.ts',
	'ConfirmDialog.svelte',
	'DraftBanner.svelte',
	'ErrorSummary.svelte',
	'FieldConflict.svelte',
	'FieldSaved.svelte',
	'FormNotice.svelte',
	'SaveBar.svelte',
	'instant.ts',
	'preview.ts',
	'FieldPreview.svelte',
	'menu.ts',
	'FieldMenu.svelte',
	'FieldKeys.svelte',
	'save-key.ts',
	'copy.ts',
	'signed-out.ts',
	'kept-restore.ts',
	'KeptNote.svelte'
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

describe('the session-end path loads on its own (X-session-draft-keep)', () => {
	/** The files the edit chunk bundles: edit.ts and what it imports statically, transitively. */
	function editChunk(): Set<string> {
		const seen = new Set<string>();
		const visit = (file: string) => {
			if (seen.has(file)) return;
			seen.add(file);
			const text = readFileSync(join(HERE, file), 'utf8');
			const FROM = /(?:^|\n)\s*(?:import|export)\s+(?!type\b)[^'"]*?from\s+['"]\.\/([^'"]+)['"]/g;
			for (const [, spec] of text.matchAll(FROM)) {
				const name = [spec, `${spec}.ts`].find((n) => sources.includes(n!));
				if (name) visit(name);
			}
		};
		visit('edit.ts');
		return seen;
	}

	it.each(['signed-out.ts', 'kept-restore.ts', 'KeptNote.svelte'])(
		'the edit chunk never imports %s statically',
		(lazy) => {
			const chunk = editChunk();
			expect(chunk.has('saving.ts')).toBe(true); // the walk works
			expect(chunk.has(lazy)).toBe(false);
		}
	);
});
