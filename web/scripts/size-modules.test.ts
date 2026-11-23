// @vitest-environment node
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { gzipSync } from 'node:zlib';
import { afterEach, describe, expect, it } from 'vitest';
import {
	PLACEHOLDER,
	hashesOf,
	moduleMeasurer,
	moduleStreams,
	normalizeNames,
	stripLinks
} from './size-modules.mjs';

type Build = {
	files: Record<string, string>;
	graph: { chunks: Record<string, { modules: string[] }> };
};

const gz = (s: string) => (s ? gzipSync(s, { level: 9 }).length : 0);
/** What size-limit reports for an entry: the gzip of each file, summed. */
const perFile = (build: Build, files: string[]) =>
	files.reduce((n, f) => n + gz(build.files[f] ?? ''), 0);
/** What the route entry reports now: the gzip of its JS stream plus its CSS stream. */
const byModules = (build: Build, files: string[]) => {
	const { js, css } = moduleStreams(files, {
		graph: build.graph,
		hashes: hashesOf(Object.keys(build.files)),
		read: (f) => build.files[f] ?? ''
	});
	return gz(js) + gz(css);
};

// Module code as Rolldown's minified output has it.
const A = 'function a(e){return e.map(t=>t*2)}';
const M = 'function m(e){return`moved: `+e.join(`, `)}';
const B = 'function b(e){return m(e).length}';
const PAGE = 'var p=()=>import(`./CmPage01.js`).then(e=>e.show);';
const CSS = '.panel{display:grid;gap:1rem}';

/**
 * A route of three chunks, `m.ts` in the first; `moved` puts it in the second, as a regrouping
 * does: the code is the same, the import and export lines around it change.
 */
const route = ({ moved = false, page = 'CmPage01' } = {}): Build => {
	const one = moved ? 'BxOne001' : 'BxOne000';
	const two = moved ? 'CxTwo001' : 'CxTwo000';
	const files: Record<string, string> = moved
		? {
				[`build/chunks/${one}.js`]: `${A}export{a as t};`,
				[`build/chunks/${two}.js`]: `${M}${B}export{m as n,b as t};`
			}
		: {
				[`build/chunks/${one}.js`]: `${A}${M}export{m as n,a as t};`,
				[`build/chunks/${two}.js`]: `import{n as m}from"./${one}.js";${B}export{b as t};`
			};
	files[`build/nodes/4.Dn0de000.js`] =
		`import{t as a}from"../chunks/${one}.js";import{t as b}from"../chunks/${two}.js";` +
		PAGE.replace('CmPage01', page) +
		'console.log(a([1]),b([2]),p);';
	files[`build/chunks/${page}.js`] = 'var s=1;export{s as show};';
	files['build/assets/Panel.Ax0css00.css'] = CSS;
	const graph = {
		chunks: {
			[`chunks/${one}.js`]: { modules: moved ? ['/w/src/a.ts'] : ['/w/src/a.ts', '/w/src/m.ts'] },
			[`chunks/${two}.js`]: { modules: moved ? ['/w/src/m.ts', '/w/src/z.ts'] : ['/w/src/z.ts'] },
			'nodes/4.Dn0de000.js': { modules: ['/w/.svelte-kit/generated/nodes/4.js'] },
			[`chunks/${page}.js`]: { modules: ['/w/src/page.ts'] }
		}
	};
	return { files, graph };
};
/** The route's first view: everything but the lazy page chunk. */
const firstView = (build: Build) =>
	Object.keys(build.files).filter((f) => !/CmPage|CnPage/.test(f));

describe('a route measured by its module set', () => {
	it('keeps its figure when a shared module moves between chunks', () => {
		const before = route();
		const after = route({ moved: true });
		// The old measure moves with the partition, though no code changed.
		expect(perFile(after, firstView(after))).not.toBe(perFile(before, firstView(before)));
		expect(byModules(after, firstView(after))).toBe(byModules(before, firstView(before)));
	});

	it('keeps its figure when a chunk it imports is renamed', () => {
		const before = route();
		const after = route({ page: 'CnPage02' });
		const view = (b: Build) => firstView(b);
		expect(byModules(after, view(after))).toBe(byModules(before, view(before)));
		// The lazy chunk itself, renamed, measures the same too.
		const lazy = (b: Build) => Object.keys(b.files).filter((f) => /Page0/.test(f));
		expect(byModules(after, lazy(after))).toBe(byModules(before, lazy(before)));
	});

	it('does not depend on the order the files are listed in', () => {
		const build = route();
		const files = firstView(build);
		expect(byModules(build, [...files].reverse())).toBe(byModules(build, files));
		expect(byModules(build, [...files, ...files])).toBe(byModules(build, files));
	});

	it('still grows when the code does', () => {
		const build = route();
		const grown = route();
		grown.files['build/chunks/BxOne000.js'] =
			`${A}${M}function g(e){return e+1}export{m as n,a as t};`;
		expect(byModules(grown, firstView(grown))).toBeGreaterThan(byModules(build, firstView(build)));
	});

	it('keeps the route code, the dynamic imports and the stylesheet', () => {
		const build = route();
		const { js, css } = moduleStreams(firstView(build), {
			graph: build.graph,
			hashes: hashesOf(Object.keys(build.files)),
			read: (f) => build.files[f] ?? ''
		});
		expect(js).toContain(A);
		expect(js).toContain(M);
		expect(js).toContain(B);
		expect(js).toContain(`import(\`./${PLACEHOLDER}.js\`)`);
		expect(js).not.toMatch(/\bfrom"|export\{/);
		expect(css).toBe(CSS);
	});

	const streams = (build: Build, files: string[], root?: string) =>
		moduleStreams(files, {
			graph: build.graph,
			hashes: hashesOf(Object.keys(build.files)),
			...(root ? { root } : {}),
			read: (f) => build.files[f] ?? ''
		});

	it('refuses a JS file the chunk graph does not know', () => {
		const build = route();
		build.files['build/chunks/Stray000.js'] = 'var x=1;';
		expect(() => streams(build, [...firstView(build), 'build/chunks/Stray000.js'])).toThrow(
			'size-modules: build/chunks/Stray000.js is not in the chunk graph: run `npm run size:ci`'
		);
	});

	it('refuses a file that is neither JS nor CSS', () => {
		const build = route();
		build.files['build/assets/Font.Ff000000.woff2'] = 'wOF2';
		expect(() => streams(build, [...firstView(build), 'build/assets/Font.Ff000000.woff2'])).toThrow(
			'is neither JS nor CSS'
		);
	});

	it('orders stylesheets whose names differ only by hash by their content', () => {
		const build = route();
		build.files['build/assets/Panel.Bx0css00.css'] = '.a{color:red}';
		const files = [...firstView(build), 'build/assets/Panel.Bx0css00.css'];
		const { css } = streams(build, files);
		expect(css).toBe(`.a{color:red}${CSS}`);
		expect(streams(build, [...files].reverse()).css).toBe(css);
	});

	it('sorts chunks by module ids relative to the project root', () => {
		// A module outside the checkout (`/t/x.ts`) sorts before `/w/src/…` by its absolute id, and
		// after `/src/…` once the root `/w` is stripped: the checkout's path decides nothing.
		const build = route();
		build.files['build/chunks/Ext00000.js'] = 'var ext=1;';
		build.graph.chunks['chunks/Ext00000.js'] = { modules: ['/t/x.ts'] };
		const files = [...firstView(build), 'build/chunks/Ext00000.js'];
		expect(streams(build, files).js.startsWith('var ext=1;')).toBe(true);
		expect(streams(build, files, '/w').js.startsWith('var ext=1;')).toBe(false);
		expect(streams(build, files, '/w').js.endsWith('var ext=1;')).toBe(true);
	});
});

describe('stripLinks', () => {
	it('drops import and export statements and keeps declarations', () => {
		const code =
			'import"./side.js";import d,{a as b}from"./x.js";import*as n from"./y.js";' +
			'export*from"./z.js";export const k=1;export default function(){return b}' +
			'var q=import("./lazy.js");export{q as r};';
		expect(stripLinks(code)).toBe('const k=1;function(){return b}var q=import("./lazy.js");');
	});
	it('names the file it cannot parse', () => {
		expect(() => stripLinks('var = ;', 'build/chunks/bad.js')).toThrow(/build\/chunks\/bad\.js/);
	});
});

describe('normalizeNames', () => {
	it('replaces only the hashes of known output files', () => {
		const hashes = hashesOf([
			'chunks/BaCVSyZ5.js',
			'nodes/3.Ck0HRBSN.js',
			'assets/Row.B4I2M8I_.css'
		]);
		const code =
			'"./BaCVSyZ5.js","../nodes/3.Ck0HRBSN.js","assets/Row.B4I2M8I_.css","./Settings.js"';
		expect(normalizeNames(code, hashes)).toBe(
			`"./${PLACEHOLDER}.js","../nodes/3.${PLACEHOLDER}.js","assets/Row.${PLACEHOLDER}.css","./Settings.js"`
		);
	});
});

describe('moduleMeasurer', () => {
	let dir = '';
	const setup = () => {
		dir = mkdtempSync(join(tmpdir(), 'size-modules-'));
		const build = route();
		const graphFile = join(dir, 'graph.json');
		writeFileSync(graphFile, JSON.stringify(build.graph));
		const read = (f: string) => build.files[f] ?? '';
		return { build, graphFile, outDir: join(dir, 'out'), read };
	};
	afterEach(() => {
		if (dir) rmSync(dir, { recursive: true, force: true });
	});

	it('writes a JS and a CSS stream per entry and empties its directory first', () => {
		const { build, graphFile, outDir, read } = setup();
		moduleMeasurer({ outDir, graphFile, outputFiles: [] });
		writeFileSync(join(outDir, 'stale.js'), 'old');
		const measure = moduleMeasurer({
			outDir,
			graphFile,
			outputFiles: Object.keys(build.files),
			read
		});
		expect(existsSync(join(outDir, 'stale.js'))).toBe(false);
		const files = ['build/chunks/BxOne000.js', 'build/assets/Panel.Ax0css00.css'];
		const out = measure('Compare route (own JS + CSS)', files);
		expect(out).toEqual([
			join(outDir, 'compare-route-own-js-css.js'),
			join(outDir, 'compare-route-own-js-css.css')
		]);
		expect(readFileSync(out[0] ?? '', 'utf8')).toBe(`${A}${M}`);
		expect(readFileSync(out[1] ?? '', 'utf8')).toBe(CSS);
		expect(measure('Logs route', ['build/chunks/BxOne000.js'])).toHaveLength(1);
		expect(() => measure('Compare route (own JS + CSS)', files)).toThrow(/two entries/);
		expect(() => measure('CSS only', ['build/assets/Panel.Ax0css00.css'])).toThrow(
			/no measured JavaScript/
		);
	});

	it('refuses to measure without the chunk graph', () => {
		const { outDir } = setup();
		expect(() =>
			moduleMeasurer({ outDir, graphFile: join(dir, 'none.json'), outputFiles: [] })
		).toThrow(/no chunk graph/);
	});
});
