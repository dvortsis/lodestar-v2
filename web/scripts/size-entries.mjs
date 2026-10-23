// Resolves the files of a route's initial load from the SvelteKit build, so the size-limit
// budgets only count what that route loads (not every chunk in the app).
import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const at = (p) => fileURLToPath(new URL(p, new URL('..', import.meta.url)));

/** Static-import closure of the given manifest keys, as { js, css } build-relative paths. */
function closure(manifest, keys) {
	const seen = new Set();
	const js = new Set();
	const css = new Set();
	const visit = (key) => {
		if (!key || seen.has(key) || !manifest[key]) return;
		seen.add(key);
		const chunk = manifest[key];
		js.add(chunk.file);
		for (const c of chunk.css ?? []) css.add(c);
		for (const i of chunk.imports ?? []) visit(i);
	};
	keys.forEach(visit);
	return { js: [...js], css: [...css] };
}

/**
 * JS and CSS loaded for `route` on first visit: the client entry (start + app), the root
 * layout node (0) and the route's own node. Returns empty lists before a build exists.
 */
export function routeFiles(route) {
	const manifestPath = at('.svelte-kit/output/client/.vite/manifest.json');
	const appPath = at('.svelte-kit/generated/client-optimized/app.js');
	if (!existsSync(manifestPath) || !existsSync(appPath)) return { js: [], css: [] };

	const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
	const dictionary = readFileSync(appPath, 'utf8').match(/dictionary = (\{[\s\S]*?\});/);
	const nodes = dictionary ? JSON.parse(dictionary[1].replace(/,\s*}/, '}'))[route] : undefined;
	if (!nodes) throw new Error(`size-entries: route ${route} not found in ${appPath}`);

	const byFile = (suffix) => Object.keys(manifest).filter((k) => k.endsWith(suffix));
	const keys = [
		...byFile('/client-optimized/app.js'),
		'node_modules/@sveltejs/kit/src/runtime/client/entry.js',
		...[0, ...nodes].flatMap((n) => byFile(`/client-optimized/nodes/${n}.js`))
	];
	const { js, css } = closure(manifest, keys);
	const abs = (f) => `build/${f}`.replaceAll('\\', '/');
	return { js: js.map(abs), css: css.map(abs) };
}

/**
 * Files of a dynamically imported module (a manifest key such as
 * `src/lib/features/detail/DetailHost.svelte`): its static-import closure, minus what the search
 * route already loads. Empty when the module is not a separate chunk in this build (or none exists).
 */
export function chunkFiles(sourceKey) {
	const manifestPath = at('.svelte-kit/output/client/.vite/manifest.json');
	if (!existsSync(manifestPath)) return { js: [], css: [] };
	const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
	if (!manifest[sourceKey]) return { js: [], css: [] };
	const { js, css } = closure(manifest, [sourceKey]);
	const abs = (f) => `build/${f}`.replaceAll('\\', '/');
	const base = routeFiles('/search');
	return {
		js: js.map(abs).filter((f) => !base.js.includes(f)),
		css: css.map(abs).filter((f) => !base.css.includes(f))
	};
}
