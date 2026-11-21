import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import { differences, snapshot } from './check-build-stable.mjs';

const roots: string[] = [];
function tree(files: Record<string, string>) {
	const root = mkdtempSync(join(tmpdir(), 'build-stable-'));
	roots.push(root);
	for (const [path, body] of Object.entries(files)) {
		mkdirSync(join(root, path, '..'), { recursive: true });
		writeFileSync(join(root, path), body);
	}
	return root;
}
afterEach(() => {
	for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

const base = {
	'build/index.html': '<html>',
	'build/_app/immutable/chunks/BAbc1234.js': 'export{}',
	'.svelte-kit/output/client/.vite/manifest.json': '{}',
	// Outside the compared trees: never read.
	'.svelte-kit/output/chunk-graph.json': '{"a":1}'
};

describe('check-build-stable', () => {
	it('finds nothing between identical builds, whatever lies outside the compared trees', () => {
		const a = snapshot(tree(base));
		const b = snapshot(tree({ ...base, '.svelte-kit/output/chunk-graph.json': '{"b":2}' }));
		expect(Object.keys(a)).toEqual([
			'.svelte-kit/output/client/.vite/manifest.json',
			'build/_app/immutable/chunks/BAbc1234.js',
			'build/index.html'
		]);
		expect(differences(a, b)).toEqual([]);
	});

	it('names a changed file, and a file only one build has', () => {
		const a = snapshot(tree(base));
		const renamed: Record<string, string> = { ...base, 'build/index.html': '<html lang>' };
		delete renamed['build/_app/immutable/chunks/BAbc1234.js'];
		renamed['build/_app/immutable/chunks/CXyz9876.js'] = 'export{}';
		const b = snapshot(tree(renamed));
		expect(differences(a, b)).toEqual([
			'only in the first build: build/_app/immutable/chunks/BAbc1234.js',
			'only in the second build: build/_app/immutable/chunks/CXyz9876.js',
			'differs: build/index.html'
		]);
	});
});
