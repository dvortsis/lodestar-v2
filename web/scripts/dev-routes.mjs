// Dev-only routes (UI showcases for Playwright and axe) live in src/dev-routes/, outside the
// SvelteKit routes directory, so a production build never sees them. When they are wanted they
// are linked into the routes tree as src/routes/dev (gitignored). The link is removed only by a
// build without them, so it survives vitest, check and eslint and a running `vite dev`.
// Called from svelte.config.js.
import { existsSync, lstatSync, realpathSync, rmSync, symlinkSync } from 'node:fs';
import { join } from 'node:path';

const TARGET = '../dev-routes';

const truthy = (v) => !!v && !['0', 'false', 'off', 'no'].includes(v.toLowerCase());

/** `vite dev`, or any run with VITE_DEV_ROUTES=1 (what Playwright's web server sets). */
export function wantDevRoutes(argv = process.argv, env = process.env) {
	return truthy(env.VITE_DEV_ROUTES) || argv.slice(2).includes('dev');
}

/** `vite build` (the only command allowed to remove the link). */
export const isBuild = (argv = process.argv) => argv.slice(2).includes('build');

export function syncDevRoutes(
	root = import.meta.dirname + '/..',
	want = wantDevRoutes(),
	build = isBuild()
) {
	const link = join(root, 'src/routes/dev');
	let stat;
	try {
		stat = lstatSync(link);
	} catch {
		stat = null;
	}
	if (stat && !stat.isSymbolicLink()) {
		throw new Error(
			'src/routes/dev is a real folder. Dev-only showcases belong in src/dev-routes/ ' +
				'(e.g. src/dev-routes/ui/<group>/+page.svelte); scripts/dev-routes.mjs links that folder in.'
		);
	}
	// A link is good when it resolves to the showcase folder. Compare real paths: on Windows a
	// junction reads back as an absolute path, so the link text can't be compared.
	const good =
		stat && existsSync(link) && realpathSync(link) === realpathSync(join(root, 'src/dev-routes'));
	if (want) {
		if (good) return;
		if (stat) rmSync(link); // dangling or pointing elsewhere: recreate
		if (existsSync(join(root, 'src/dev-routes'))) {
			// 'junction' makes this work on Windows without elevated rights; ignored elsewhere.
			symlinkSync(TARGET, link, 'junction');
		}
	} else if (stat && build) {
		rmSync(link);
	}
}
