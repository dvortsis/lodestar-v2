// A route's size by the source modules it loads, not by how the bundler split them into files
// (X-size-own-modules, runner r14). Summing each file's gzip credits a route by its chunk
// partition: the same modules cost more or less as Rolldown regroups them (each file has its own
// gzip header and dictionary, and every file boundary adds `import{…}from"./<hash>.js"` and
// `export{…}` lines), and a chunk renamed because another chunk's make-up changed moves every
// importer by a few bytes. X-detail-depth moved Compare 30,895 → 31,135 B that way (+240 B, the
// same modules); measured as below, 22,273 → 22,226 B.
//
// `moduleMeasurer` writes one JS stream and one CSS stream per route instead, and size-limit gzips
// those two files (so the figure is still "gzipped JS + CSS"):
//   - the stream holds the route's module set: every chunk holding one of its modules, ordered by
//     the source modules it holds (not by file name) and joined with nothing between them;
//   - the import and export statements between chunks are dropped (the bindings they name are the
//     partition, not the route's code); dynamic `import()` calls stay;
//   - every output file name left in the code (`import("./<hash>.js")`, preload lists, stylesheet
//     links) has its hash replaced by a fixed placeholder, so a renamed chunk changes nothing.
// What stays partition-dependent is the minifier's naming inside a chunk and the statement order
// within it: tens of bytes rather than hundreds.
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { basename, join } from 'node:path';
import { parseAst } from 'vite';

/** The 8-character name Rolldown (or the stable-names plugin) gives an output file. */
const HASH = /[\w-]{8}(?=\.(?:js|css)\b)/g;
/** What every hashed output name becomes in the measured stream. */
export const PLACEHOLDER = '________';

/**
 * The hashes in the given output files' names: `chunks/<h>.js`, `nodes/3.<h>.js`,
 * `entry/app.<h>.js`, `assets/Name.<h>.css`.
 * @param {Iterable<string>} files
 * @returns {Set<string>}
 */
export function hashesOf(files) {
	/** @type {Set<string>} */
	const out = new Set();
	for (const file of files) {
		const m = basename(file).match(/([\w-]{8})\.(?:js|css)$/);
		if (m?.[1]) out.add(m[1]);
	}
	return out;
}

/**
 * `code` with every known output hash replaced by the placeholder. Only names in `hashes` are
 * touched, so an 8-character word before `.js` in a string the app shows is left alone.
 * @param {string} code
 * @param {Set<string>} hashes
 */
export const normalizeNames = (code, hashes) =>
	code.replace(HASH, (h) => (hashes.has(h) ? PLACEHOLDER : h));

/**
 * `code` without its static import and export statements: `import … from "…"`, `import "…"`,
 * `export { … }`, `export * from "…"`; `export <declaration>` and `export default <expression>`
 * keep the declaration or expression. Dynamic `import()` is an expression and stays.
 * @param {string} code a module (an output chunk)
 * @param {string} [file] for the error message
 */
export function stripLinks(code, file = 'chunk') {
	/** @type {any} */
	let ast;
	try {
		ast = parseAst(code);
	} catch (e) {
		throw new Error(`size-modules: cannot parse ${file}: ${/** @type {Error} */ (e).message}`);
	}
	/** @type {[number, number][]} */
	const cuts = [];
	for (const node of ast.body) {
		if (
			node.type === 'ImportDeclaration' ||
			node.type === 'ExportAllDeclaration' ||
			(node.type === 'ExportNamedDeclaration' && !node.declaration)
		)
			cuts.push([node.start, node.end]);
		else if (node.type === 'ExportNamedDeclaration' || node.type === 'ExportDefaultDeclaration')
			cuts.push([node.start, node.declaration.start]);
	}
	let out = '';
	let at = 0;
	for (const [start, end] of cuts) {
		out += code.slice(at, start);
		at = end;
	}
	return out + code.slice(at);
}

/** @typedef {{ chunks: Record<string, { modules: string[] }> }} ChunkGraph */

/**
 * The order key of an output file: the source modules it holds, sorted (from the chunk graph), so
 * a renamed chunk keeps its place among the others. CSS, and files the graph doesn't know, sort
 * after them by their name with the hash normalised.
 * @param {string} file a `build/…` path
 * @param {ChunkGraph} graph
 * @param {Set<string>} hashes
 */
const orderKey = (file, graph, hashes) => {
	const modules = graph.chunks[file.replace(/^build\//, '')]?.modules;
	if (modules?.length) return [...modules].sort().join('\n');
	return `￿${normalizeNames(file, hashes)}`;
};

/**
 * A route's code as one stream per kind, from the output files it loads. Files are joined with
 * nothing between them, so a module that moves from the end of one chunk to the start of the next
 * leaves the stream as it was.
 * @param {string[]} files `build/…` paths (JS and CSS)
 * @param {{ graph: ChunkGraph, hashes: Set<string>, read?: (file: string) => string }} ctx
 * @returns {{ js: string, css: string }}
 */
export function moduleStreams(files, { graph, hashes, read = (f) => readFileSync(f, 'utf8') }) {
	const keyed = [...new Set(files)].map((file) => ({ file, key: orderKey(file, graph, hashes) }));
	keyed.sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0));
	let js = '';
	let css = '';
	for (const { file } of keyed) {
		const code = normalizeNames(read(file), hashes);
		if (file.endsWith('.css')) css += code.trim();
		else if (file.endsWith('.js')) js += stripLinks(code, file).trim();
		else throw new Error(`size-modules: ${file} is neither JS nor CSS`);
	}
	return { js, css };
}

/**
 * A measurer for `.size-limit.mjs`: `measure(name, files)` writes the route's two streams under
 * `outDir` and returns their paths, for an entry's `path` (size-limit gzips each, as before). The
 * directory is emptied once, when the measurer is made, so no stream from an earlier run is
 * measured.
 * @param {{ outDir: string, graphFile: string, outputFiles: Iterable<string>, read?: (file: string) => string }} opts
 *   `outputFiles`: every output file of the client build; their hashes are normalised wherever
 *   they appear. `read`: how a `build/…` file is read (tests pass their own).
 */
export function moduleMeasurer({ outDir, graphFile, outputFiles, read }) {
	if (!existsSync(graphFile))
		throw new Error(`size-modules: no chunk graph at ${graphFile}: run \`npm run size:ci\``);
	/** @type {ChunkGraph} */
	const graph = JSON.parse(readFileSync(graphFile, 'utf8'));
	const hashes = hashesOf(outputFiles);
	rmSync(outDir, { recursive: true, force: true });
	mkdirSync(outDir, { recursive: true });
	/** @type {Set<string>} */
	const used = new Set();
	/**
	 * @param {string} name the entry's name
	 * @param {string[]} files the route's output files (`own()`)
	 */
	return (name, files) => {
		const slug = name
			.toLowerCase()
			.replace(/[^a-z0-9]+/g, '-')
			.replace(/^-|-$/g, '');
		if (used.has(slug)) throw new Error(`size-modules: two entries named like ${name}`);
		used.add(slug);
		const { js, css } = moduleStreams(files, { graph, hashes, ...(read ? { read } : {}) });
		if (!js) throw new Error(`size-modules: ${name} has no measured JavaScript`);
		const out = [join(outDir, `${slug}.js`)];
		writeFileSync(out[0], js);
		if (css) {
			out.push(join(outDir, `${slug}.css`));
			writeFileSync(out[1], css);
		}
		return out;
	};
}
