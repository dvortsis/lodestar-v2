// @vitest-environment node
import { describe, expect, it } from 'vitest';
import { BARREL, findStrays, routeNodes, svelteBarrel } from './first-load.mjs';

const transform = async (code: string, id = '/app/src/Comp.svelte') => {
	const plugin = svelteBarrel();
	const ctx = {
		resolve: async (source: string) =>
			source === 'svelte/internal/client' || source.endsWith('internal/client/index.js')
				? { id: BARREL }
				: { id: source }
	};
	const out = await (plugin.transform as (this: unknown, c: string, i: string) => unknown).call(
		ctx,
		code,
		id
	);
	return (out as { code: string } | null)?.code ?? null;
};

describe('svelteBarrel', () => {
	it('rewrites the runtime namespace into direct imports, one alias per name', async () => {
		const code = [
			"import 'svelte/internal/disclose-version';",
			"import * as $ from 'svelte/internal/client';",
			'var root = $.from_html(`<p>$.not_code</p>`);',
			'export default function C(a) { $.each(a, 0, () => 1); $.each(a, 1, () => 2); $.append(a, root()); }'
		].join('\n');
		const out = (await transform(code))!;
		expect(out).not.toContain("from 'svelte/internal/client'");
		expect(out).toMatch(/import \{ each as \$__sv\d+_each \} from ".*dom\/blocks\/each\.js";/);
		// Names from one module share an import line.
		expect(out).toMatch(
			/import \{ from_html as \$__sv\d+_from_html, append as \$__sv\d+_append \} from ".*dom\/template\.js";/
		);
		expect(out.match(/\$__sv\d+_each\(/g)).toHaveLength(2);
		// Text that merely looks like a member access is left alone.
		expect(out).toContain('`<p>$.not_code</p>`');
		expect(out).toContain("import 'svelte/internal/disclose-version';");
	});

	it('rewrites named imports, keeping their local names', async () => {
		const out = (await transform(
			"import { user_effect } from './internal/client/index.js';\nuser_effect(() => {});",
			'/x/node_modules/svelte/src/index-client.js'
		))!;
		expect(out).toMatch(/^import \{ user_effect \} from ".*reactivity\/effects\.js";/);
	});

	it('leaves a namespace used as a value, or an unknown name, as it was', async () => {
		const asValue = "import * as $ from 'svelte/internal/client';\nconsole.log($);";
		expect(await transform(asValue)).toBeNull();
		const unknown = "import * as $ from 'svelte/internal/client';\n$.no_such_export();";
		expect(await transform(unknown)).toBeNull();
		const computed = "import * as $ from 'svelte/internal/client';\n$['each']();";
		expect(await transform(computed)).toBeNull();
	});

	it('ignores modules that do not import the barrel', async () => {
		expect(await transform("import { x } from './internal/client/other.js';")).toBeNull();
		expect(await transform('const a = 1;')).toBeNull();
	});
});

describe('findStrays', () => {
	const graph = {
		chunks: {
			'first.js': { modules: ['/r/search', '/r/shared', '/r/alerts-only', '\0virtual'] },
			'lazy.js': { modules: ['/r/alerts'] }
		},
		modules: {
			'/r/search': { imports: ['/r/shared'] },
			'/r/shared': { imports: [] },
			'/r/alerts': { imports: ['/r/alerts-only', '/r/shared'] },
			'/r/alerts-only': { imports: [] }
		}
	};

	it('names a module in the first load that only another route reaches', () => {
		expect(findStrays(graph, ['first.js'], ['/r/search'])).toEqual([
			'first.js: /r/alerts-only is not reachable from /search'
		]);
	});

	it('passes when the first load holds only what the route reaches', () => {
		const clean = { ...graph, chunks: { 'first.js': { modules: ['/r/search', '/r/shared'] } } };
		expect(findStrays(clean, ['first.js'], ['/r/search'])).toEqual([]);
	});

	it('names a module that still imports the Svelte runtime barrel', () => {
		const left = { chunks: {}, modules: { '/r/c.svelte': { imports: [BARREL] } } };
		expect(findStrays(left, [], [])[0]).toContain('imports the Svelte runtime barrel');
	});
});

describe('routeNodes', () => {
	it('reads the route from the generated app.js, with the layout and error nodes', () => {
		const app = 'export const dictionary = {\n\t\t"/": [2],\n\t\t"/search": [11],\n\t};';
		expect(routeNodes(app, '/search')).toEqual([0, 1, 11]);
		expect(() => routeNodes(app, '/nope')).toThrow(/not in the generated app\.js/);
	});
});
