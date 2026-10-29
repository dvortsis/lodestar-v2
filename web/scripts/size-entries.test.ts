// @vitest-environment node
import { describe, expect, it } from 'vitest';
import { requireMeasuredJs, routeFilesFrom } from './size-entries.mjs';

type Chunk = { file: string; imports?: string[]; dynamicImports?: string[]; css?: string[] };
const app = 'export const dictionary = {"/search": [3], "/nested": [~7,[2,null,4],[null,5]],};';
const node = (id: number) => `.svelte-kit/generated/client-optimized/nodes/${id}.js`;
const fixture = (): Record<string, Chunk> => ({
	'.svelte-kit/generated/client-optimized/app.js': {
		file: 'entry/app.js',
		dynamicImports: [node(7)]
	},
	'node_modules/@sveltejs/kit/src/runtime/client/entry.js': {
		file: 'entry/start.js',
		imports: ['shared']
	},
	shared: { file: 'chunks/shared.js', imports: ['cycle'], css: ['assets/shared.css'] },
	cycle: { file: 'chunks/cycle.js', imports: ['shared'] },
	...Object.fromEntries(
		[0, 1, 2, 3, 4, 5, 7].map((id) => [
			node(id),
			{
				file: `nodes/${id}.js`,
				imports: ['shared'],
				css: [`assets/${id}.css`, 'assets/shared.css']
			}
		])
	)
});

describe('route bundle accounting', () => {
	it('includes nested layout and error nodes and decodes server-load indices', () => {
		const files = routeFilesFrom(fixture(), app, '/nested');
		expect(files.js).toEqual([
			'build/entry/app.js',
			'build/entry/start.js',
			'build/chunks/shared.js',
			'build/chunks/cycle.js',
			'build/nodes/0.js',
			'build/nodes/1.js',
			'build/nodes/7.js',
			'build/nodes/2.js',
			'build/nodes/4.js',
			'build/nodes/5.js'
		]);
		expect(files.css).toEqual([
			'build/assets/shared.css',
			...[0, 1, 7, 2, 4, 5].map((id) => `build/assets/${id}.css`)
		]);
	});
	it('does not load another route from the app entry dynamic imports', () => {
		const files = routeFilesFrom(fixture(), app, '/search');
		expect(files.js).toContain('build/nodes/3.js');
		expect(files.js).not.toContain('build/nodes/7.js');
		expect(new Set(files.js).size).toBe(files.js.length);
	});
	it('fails when the route is absent', () => {
		expect(() => routeFilesFrom(fixture(), app, '/missing')).toThrow(/route.*not in/);
	});
	it.each([0, 1, 2, 4, 5, 7])('fails when required node %s is missing', (id) => {
		const manifest = fixture();
		delete manifest[node(id)];
		expect(() => routeFilesFrom(manifest, app, '/nested')).toThrow(/expected one manifest entry/);
	});
	it('fails on a missing transitive import instead of dropping its bytes', () => {
		const manifest = fixture();
		delete manifest.shared;
		expect(() => routeFilesFrom(manifest, app, '/search')).toThrow(
			/missing manifest import shared/
		);
	});
	it('fails on ambiguous generated nodes', () => {
		const manifest = fixture();
		manifest[`duplicate/${node(3)}`] = { file: 'other.js' };
		expect(() => routeFilesFrom(manifest, app, '/search')).toThrow(/found 2/);
	});
	it('fails on a manifest entry without an output file', () => {
		const manifest = fixture();
		manifest.shared!.file = '';
		expect(() => routeFilesFrom(manifest, app, '/search')).toThrow(/missing output file/);
	});
});

describe('required on-demand budgets', () => {
	it.each([{ files: [] }, { files: ['build/controls.css'] }])(
		'cannot disappear when lazy keys stop matching: %j',
		({ files }) => {
			expect(() => requireMeasuredJs('Settings controls', files)).toThrow(/no measured JavaScript/);
		}
	);
	it('retains all measured files when JavaScript is present', () => {
		const files = ['build/controls.js', 'build/controls.css'];
		expect(requireMeasuredJs('Settings controls', files)).toBe(files);
	});
});
