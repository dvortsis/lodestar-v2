import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';
import { syncDevRoutes } from './scripts/dev-routes.mjs';

// Showcase routes under src/dev-routes are linked into src/routes only for dev and test builds.
syncDevRoutes();

/** @type {import('@sveltejs/kit').Config} */
export default {
	preprocess: vitePreprocess(),
	kit: {
		adapter: adapter({ pages: 'build', assets: 'build', fallback: 'index.html', strict: false })
	}
};
