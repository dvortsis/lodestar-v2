// Resolves the files of a route's initial load from the SvelteKit build, so the size-limit
// budgets only count what that route loads (not every chunk in the app).
import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { routeNodes } from './first-load.mjs';

const at = (p) => fileURLToPath(new URL(p, new URL('..', import.meta.url)));

/** Static-import closure of the given manifest keys, as { js, css } build-relative paths. */
function closure(manifest, keys) {
	const seen = new Set();
	const js = new Set();
	const css = new Set();
	const visit = (key) => {
		if (seen.has(key)) return;
		if (!Object.hasOwn(manifest, key))
			throw new Error(`size-entries: missing manifest import ${key}`);
		seen.add(key);
		const chunk = manifest[key];
		if (typeof chunk.file !== 'string' || !chunk.file)
			throw new Error(`size-entries: missing output file for ${key}`);
		js.add(chunk.file);
		for (const c of chunk.css ?? []) css.add(c);
		for (const i of chunk.imports ?? []) visit(i);
	};
	keys.forEach(visit);
	return { js: [...js], css: [...css] };
}

/**
 * JS and CSS loaded for `route` on first visit, including nested layouts and error nodes.
 * Missing build inputs are errors: a budget must never pass by measuring an empty route.
 */
export function routeFiles(route) {
	const manifestPath = at('.svelte-kit/output/client/.vite/manifest.json');
	const appPath = at('.svelte-kit/generated/client-optimized/app.js');
	if (!existsSync(manifestPath) || !existsSync(appPath))
		throw new Error('size-entries: production build missing; build the frontend before measuring');

	const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
	return routeFilesFrom(manifest, readFileSync(appPath, 'utf8'), route);
}

/** Pure resolver shared by the real build reader and disposable manifest fixtures. */
export function routeFilesFrom(manifest, appJs, route) {
	const nodes = routeNodes(appJs, route);
	const byFile = (suffix) => {
		const matches = Object.keys(manifest).filter((key) => key.endsWith(suffix));
		if (matches.length !== 1)
			throw new Error(
				`size-entries: expected one manifest entry for ${suffix}, found ${matches.length}`
			);
		return matches[0];
	};
	const keys = [
		byFile('/client-optimized/app.js'),
		'node_modules/@sveltejs/kit/src/runtime/client/entry.js',
		...nodes.map((n) => byFile(`/client-optimized/nodes/${n}.js`))
	];
	const { js, css } = closure(manifest, keys);
	const abs = (f) => `build/${f}`.replaceAll('\\', '/');
	return { js: js.map(abs), css: css.map(abs) };
}

/** Required measured families must survive module moves and chunk renaming. */
export function requireMeasuredJs(name, files) {
	if (!files.some((file) => file.endsWith('.js')))
		throw new Error(
			`size-entries: ${name} has no measured JavaScript; check its manifest boundaries`
		);
	return files;
}
