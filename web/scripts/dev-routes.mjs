// Dev-only routes (UI showcases for Playwright and axe) live in src/dev-routes/, outside the
// SvelteKit routes directory, so a production build never sees them. When they are wanted they
// are linked into the routes tree as src/routes/dev (gitignored); otherwise any stale link is
// removed. Called from svelte.config.js, so it runs for dev, build, check and the Playwright build.
import { existsSync, lstatSync, rmSync, symlinkSync } from 'node:fs';
import { join } from 'node:path';

/** `vite dev`, or a build/preview with VITE_DEV_ROUTES=1 (what Playwright's web server sets). */
export function wantDevRoutes(argv = process.argv, env = process.env) {
	return !!env.VITE_DEV_ROUTES || argv.slice(2).includes('dev');
}

export function syncDevRoutes(root = import.meta.dirname + '/..', want = wantDevRoutes()) {
	const link = join(root, 'src/routes/dev');
	const present = (() => {
		try {
			return lstatSync(link).isSymbolicLink();
		} catch {
			return false;
		}
	})();
	if (want && !present && existsSync(join(root, 'src/dev-routes'))) {
		// 'junction' makes this work on Windows without elevated rights; ignored elsewhere.
		symlinkSync('../dev-routes', link, 'junction');
	} else if (!want && present) {
		rmSync(link);
	}
}
