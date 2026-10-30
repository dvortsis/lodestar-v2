// The search route's first load (§10.1: 45 KB of JS, 102,400 bytes in Lighthouse) must carry only
// what that route uses, however many other routes the app grows. Three build plugins and a check:
// `svelteBarrel` (below), `firstLoadGraph` + `vendorGroup` (one vendor chunk for the dependencies
// the route really reaches), `chunkGraph` (a record of the output) and `checkFirstLoad`.
//
// Compiled Svelte components import the whole runtime as one namespace
// (`import * as $ from 'svelte/internal/client'`), and `svelte`'s own entry imports that barrel too.
// So to the bundler every route reaches every runtime module. Rolldown then puts any runtime piece
// that any route uses (a lazy route's `{#each}` or `bind:clientWidth`, say) into the chunk every
// page loads, and the search route's first load (§10.1) grows with every other route.
//
// This plugin rewrites imports of that barrel into direct imports from the modules that define
// each name (`$.each(...)` → `import { each as $__each } from '.../dom/blocks/each.js'`), so the
// chunk graph follows what each module really uses. ES import bindings are live, so the runtime's
// own `export let` state still reads through. A module that uses the namespace any other way (as a
// value, a computed member, a name the barrel doesn't export) is left as it was.
// `checkFirstLoad()` below checks the result after every build (run by `.size-limit.mjs`).
// Build only; dev and tests are unchanged.
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { normalizePath, parseAst } from 'vite';

const require = createRequire(import.meta.url);
const SVELTE = dirname(require.resolve('svelte/package.json'));
export const BARREL = normalizePath(join(SVELTE, 'src/internal/client/index.js'));

/** @typedef {{ file: string, name: string }} Origin */
/** @typedef {{ chunks: Record<string, { modules: string[] }>, modules: Record<string, { imports: string[] }> }} Graph */

/**
 * exported name → its origin, for the barrel's `export { a, b as c } from './x.js'` lines.
 * @param {string} file
 */
function exportMap(file) {
	/** @type {Map<string, Origin>} */
	const map = new Map();
	for (const node of parseAst(readFileSync(file, 'utf8')).body) {
		if (node.type === 'ExportAllDeclaration') throw new Error(`svelte-barrel: export * in ${file}`);
		if (node.type !== 'ExportNamedDeclaration') continue;
		if (!node.source) throw new Error(`svelte-barrel: local export in ${file}`);
		const from = normalizePath(resolve(dirname(file), node.source.value));
		for (const s of node.specifiers)
			map.set(nameOf(s.exported), { file: from, name: nameOf(s.local) });
	}
	return map;
}

/** @param {any} n an identifier or a string-literal export name */
const nameOf = (n) => (n.type === 'Identifier' ? n.name : String(n.value));

/**
 * Calls `visit(node, parent)` for every AST node.
 * @param {any} node
 * @param {any} parent
 * @param {(node: any, parent: any) => void} visit
 */
function walk(node, parent, visit) {
	visit(node, parent);
	for (const key in node) {
		const value = node[key];
		if (key === 'parent' || !value || typeof value !== 'object') continue;
		if (Array.isArray(value)) {
			for (const child of value)
				if (child && typeof child.type === 'string') walk(child, node, visit);
		} else if (typeof value.type === 'string') walk(value, node, visit);
	}
}

/** @param {string} id */
const clean = (id) => normalizePath(id.slice(0, id.indexOf('?') < 0 ? id.length : id.indexOf('?')));

/** @returns {import('vite').Plugin} */
export function svelteBarrel() {
	/** @type {Map<string, Origin> | undefined} */
	let exports;
	return {
		name: 'lodestar-svelte-barrel',
		apply: 'build',
		enforce: 'post',
		async transform(code, id) {
			if (!code.includes('internal/client') || clean(id) === BARREL) return null;
			const ast = parseAst(code);
			/** @type {[number, number, string][]} */
			const edits = [];
			let n = 0;
			for (const decl of ast.body) {
				if (decl.type !== 'ImportDeclaration' || !decl.source.value.includes('internal/client'))
					continue;
				const target = await this.resolve(decl.source.value, id);
				if (!target || clean(target.id) !== BARREL) continue;
				const map = (exports ??= exportMap(BARREL));
				/** @type {[Origin, string][]} origin, local name */
				const named = [];
				/** @type {[any, string][]} member expression, its replacement */
				const local = [];
				let ok = true;
				for (const s of decl.specifiers) {
					if (s.type === 'ImportSpecifier') {
						const origin = map.get(nameOf(s.imported));
						if (!origin) ok = false;
						else named.push([origin, s.local.name]);
						continue;
					}
					// `import * as $`: every use must be `$.name` with a name the barrel exports.
					const ns = s.local.name;
					/** @type {[any, Origin][]} */
					const uses = [];
					walk(ast, null, (node, parent) => {
						if (node.type !== 'Identifier' || node.name !== ns || node === s.local) return;
						const member = parent?.type === 'MemberExpression' && parent.object === node;
						const origin = member && !parent.computed && map.get(parent.property.name);
						if (!origin) ok = false;
						else uses.push([parent, origin]);
					});
					/** @type {Map<string, string>} */
					const alias = new Map();
					for (const [member, origin] of uses) {
						const key = `${origin.file}#${origin.name}`;
						if (!alias.has(key)) {
							const name = `$__sv${n++}_${origin.name}`;
							alias.set(key, name);
							named.push([origin, name]);
						}
						local.push([member, /** @type {string} */ (alias.get(key))]);
					}
				}
				if (!ok) continue; // leave this import as it was
				/** @type {Map<string, string[]>} */
				const byFile = new Map();
				for (const [origin, name] of named) {
					const list = byFile.get(origin.file) ?? [];
					list.push(origin.name === name ? name : `${origin.name} as ${name}`);
					byFile.set(origin.file, list);
				}
				const lines = [...byFile].map(
					([file, names]) => `import { ${names.join(', ')} } from ${JSON.stringify(file)};`
				);
				edits.push([decl.start, decl.end, lines.join('\n')]);
				for (const [member, name] of local) edits.push([member.start, member.end, name]);
			}
			if (!edits.length) return null;
			edits.sort((a, b) => b[0] - a[0]);
			let out = code;
			for (const [start, end, text] of edits) out = out.slice(0, start) + text + out.slice(end);
			// No source map: production builds ship none (build.sourcemap is off).
			return { code: out, map: null };
		}
	};
}

// ---- the search route's first load ------------------------------------------------------------

const GENERATED = '.svelte-kit/generated/client-optimized/';

/**
 * The modules a route's first load starts from: kit's client entry and app, and its nodes.
 * @param {string} id
 * @param {number[]} nodes
 */
const isRoot = (id, nodes) =>
	id.endsWith(`${GENERATED}app.js`) ||
	id.endsWith('/@sveltejs/kit/src/runtime/client/entry.js') ||
	nodes.some((n) => id.endsWith(`${GENERATED}nodes/${n}.js`));

/**
 * The route nodes kit loads for `route`, read from its generated client manifest.
 * @param {string} appJs
 * @param {string} route
 * @returns {number[]}
 */
export function routeNodes(appJs, route) {
	const dictionary = appJs.match(/dictionary = (\{[\s\S]*?\});/);
	const entry = dictionary?.[1]
		? JSON.parse(
				dictionary[1].replace(/,\s*}/, '}').replace(/~(\d+)/g, (_, d) => String(~Number(d)))
			)[route]
		: undefined;
	if (!entry) throw new Error(`first-load: route ${route} not in the generated app.js`);
	// An entry is the page node, then its layouts and error pages, possibly nested and negated
	// (kit writes ~n for a node with a server load). The root layout (0) and error page (1) always load.
	/** @type {number[]} */
	const nodes = [entry].flat(Infinity).filter((n) => typeof n === 'number');
	return [...new Set([0, 1, ...nodes.map((n) => (n < 0 ? ~n : n))])];
}

/**
 * Modules the search route's first load reaches statically, filled at `buildEnd` (the graph is
 * then complete and, with the barrel rewritten, follows real use). `vendorGroup()` puts the
 * dependencies among them in one chunk, so the first load is one vendor file and never carries
 * runtime only another route uses.
 */
/** @type {Set<string>} */
export const firstLoad = new Set();

/** @returns {import('vite').Plugin} */
export function firstLoadGraph(route = '/search') {
	return {
		name: 'lodestar-first-load',
		apply: 'build',
		buildEnd() {
			const ids = [...this.getModuleIds()];
			const app = ids.find((id) => id.endsWith(`${GENERATED}app.js`));
			if (!app) return; // the server build
			firstLoad.clear();
			const nodes = routeNodes(readFileSync(app, 'utf8'), route);
			const roots = ids.filter((id) => isRoot(id, nodes));
			const stack = [...roots];
			while (stack.length) {
				const id = /** @type {string} */ (stack.pop());
				if (firstLoad.has(id)) continue;
				firstLoad.add(id);
				stack.push(...(this.getModuleInfo(id)?.importedIds ?? []));
			}
		}
	};
}

/** The code-splitting group for the first load's dependencies. */
export const vendorGroup = () => ({
	name: 'vendor',
	/** @param {string} id */
	test: (id) => firstLoad.has(id) && id.includes('/node_modules/'),
	priority: 10
});

// Badge and Banner both import these four status icons. Keep the icon family
// together with their shared mapping instead of paying separate requests and compression.
// Anything needed by initial search remains in its existing first-load group.
export const statusIconsGroup = () => ({
	name: 'status-icons',
	/** @param {string} id */
	test: (id) =>
		!firstLoad.has(id) &&
		(/^~icons\/ph\/(info|check-circle|warning|x-circle)\.svelte$/.test(id) ||
			id.endsWith('/src/lib/ui/status.ts')),
	priority: 5
});

// Command registration needs the registry and combo parser together. The
// deferred keyboard family is shared by result navigation and detail commands.
export const keysGroup = () => ({
	name: 'keys',
	/** @param {string} id */
	test: (id) => !firstLoad.has(id) && id.includes('/src/lib/keys/'),
	priority: 5
});

// ---- route preloads --------------------------------------------------------------------------

/**
 * Vite's `build.modulePreload.resolveDependencies`: kit's entry (`app.js`) imports every route's
 * node, and by default lists each node's JS chunks to preload with it (`__vite__mapDeps`). That
 * list is in `/search`'s first load and grows with every route. Only CSS stays (it must load with
 * a node, or the page paints unstyled); a node's JS imports load when the node does, and kit
 * starts that on hover or touch (`data-sveltekit-preload-data`), before the navigation.
 *
 * `/search`'s own node, the root layout and the root error node get the same rule: their lists
 * of deferred chunks ride in the first load too, and grow with every chunk their lazy views
 * use. Other route nodes keep their preloads. `stripPreloadsOf` identifies these first-load
 * nodes by source module before their hashed file names are written into preload maps.
 * @param {string} _file
 * @param {string[]} deps
 * @param {{ hostId: string }} ctx
 */
export const routePreloads = (_file, deps, { hostId }) =>
	/(^|\/)entry\/app\.[^/]*\.js$/.test(hostId) || stripNodes.has(hostId)
		? deps.filter((d) => d.endsWith('.css'))
		: deps;

/** Route nodes whose lazy parts lose their JS preload lists (`stripPreloadsOf`), by file name. */
export const stripNodes = new Set();

/**
 * Marks the route node chunks that contain one of `pages` for `routePreloads`. It runs before Vite
 * writes the preload maps.
 * @param {RegExp} pages
 */
export function stripPreloadsOf(pages) {
	return {
		name: 'lodestar:strip-preloads',
		generateBundle: {
			order: /** @type {const} */ ('pre'),
			/** @param {unknown} _ @param {Record<string, { type: string; fileName: string; moduleIds?: string[] }>} bundle */
			handler(_, bundle) {
				for (const chunk of Object.values(bundle))
					if (
						chunk.type === 'chunk' &&
						/(^|\/)nodes\/\d+\./.test(chunk.fileName) &&
						chunk.moduleIds?.some((id) => pages.test(id))
					)
						stripNodes.add(chunk.fileName);
			}
		}
	};
}

/**
 * The JS files a built `app.js` still lists in its preload map (`__vite__mapDeps`): none, or the
 * route table grows with every route's chunks again.
 * @param {string} appJs the built entry's source
 * @returns {string[]}
 */
export function preloadedJs(appJs) {
	const list = appJs.match(/__vite__mapDeps=[^[]*\[([^\]]*)\]/);
	if (!list?.[1]) return [];
	return [...list[1].matchAll(/"([^"]+)"/g)]
		.map((m) => m[1] ?? '')
		.filter((f) => f.endsWith('.js'));
}

// ---- the record and the check -----------------------------------------------------------------

// Which modules each client chunk holds, and every module's static imports.
export const GRAPH = fileURLToPath(
	new URL('../.svelte-kit/output/chunk-graph.json', import.meta.url)
);

/** @returns {import('vite').Plugin} */
export function chunkGraph() {
	return {
		name: 'lodestar-chunk-graph',
		apply: 'build',
		generateBundle(_options, bundle) {
			if (this.environment?.name && this.environment.name !== 'client') return;
			/** @type {Graph['chunks']} */
			const chunks = {};
			/** @type {Graph['modules']} */
			const modules = {};
			for (const out of Object.values(bundle)) {
				if (out.type !== 'chunk') continue;
				chunks[out.fileName] = { modules: [...out.moduleIds] };
			}
			// Every module, bundled or not: barrels and re-exports carry the import edges.
			for (const id of this.getModuleIds()) {
				const info = this.getModuleInfo(id);
				modules[id] = { imports: [...(info?.importedIds ?? [])] };
			}
			mkdirSync(dirname(GRAPH), { recursive: true });
			writeFileSync(GRAPH, JSON.stringify({ chunks, modules }));
		}
	};
}

/** @param {string} p a path from web/ */
const at = (p) => fileURLToPath(new URL(`../${p}`, import.meta.url));

/**
 * What is wrong with `route`'s first load, as messages (none: fine). Two rules:
 * - every module in the route's initial chunks is one the route reaches by static imports (so no
 *   other route's code rides along);
 * - nothing imports the Svelte runtime barrel any more (a module the rewrite had to leave alone
 *   would pull every route's runtime pieces back in).
 * Returns [] before a build exists.
 * @param {string} route
 * @param {string[]} files the route's initial JS (`routeFiles(route).js`)
 */
export function checkFirstLoad(route, files) {
	if (!existsSync(GRAPH)) return files.length ? [`no chunk graph at ${GRAPH}: rebuild`] : [];
	/** @type {Graph} */
	const { chunks, modules } = JSON.parse(readFileSync(GRAPH, 'utf8'));
	const nodes = routeNodes(readFileSync(at(`${GENERATED}app.js`), 'utf8'), route);
	const roots = Object.keys(modules).filter((id) => isRoot(id, nodes));
	return findStrays(
		{ chunks, modules },
		files.map((f) => f.replace(/^build\//, '')),
		roots,
		route
	);
}

/**
 * The check itself, over a recorded graph: modules in `files`' chunks that `roots` don't reach by
 * static imports, and modules that still import the Svelte runtime barrel.
 * @param {Graph} graph
 * @param {string[]} files
 * @param {string[]} roots
 */
export function findStrays({ chunks, modules }, files, roots, route = '/search') {
	/** @type {Set<string>} */
	const reach = new Set();
	const stack = [...roots];
	while (stack.length) {
		const id = /** @type {string} */ (stack.pop());
		if (reach.has(id)) continue;
		reach.add(id);
		stack.push(...(modules[id]?.imports ?? []));
	}
	/** @param {string} id */
	const short = (id) => id.replace(/^.*\/node_modules\//, '~/').replace(at(''), '');
	const problems = [];
	for (const file of files) {
		const chunk = chunks[file];
		if (!chunk) {
			problems.push(`${file} is not in the chunk graph (stale or missing: rebuild)`);
			continue;
		}
		for (const id of chunk.modules) {
			if (!reach.has(id) && !id.startsWith('\0') && !id.startsWith('rolldown:'))
				problems.push(`${file}: ${short(id)} is not reachable from ${route}`);
		}
	}
	for (const [id, m] of Object.entries(modules)) {
		if (m.imports.includes(BARREL))
			problems.push(
				`${short(id)} imports the Svelte runtime barrel (svelteBarrel did not rewrite it)`
			);
	}
	return problems;
}
