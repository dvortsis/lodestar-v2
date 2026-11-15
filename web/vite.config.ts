import { sveltekit } from '@sveltejs/kit/vite';
import tailwindcss from '@tailwindcss/vite';
import Icons from 'unplugin-icons/vite';
import { paraglideVitePlugin } from '@inlang/paraglide-js';
import type { Plugin } from 'vite';
import { defineConfig } from 'vitest/config';
import { mergeMessages } from './scripts/messages.ts';
import { licenses } from './scripts/licenses.ts';
import {
	chunkGraph,
	firstLoadGraph,
	statusIconsGroup,
	keysGroup,
	routePreloads,
	stripPreloadsOf,
	svelteBarrel,
	vendorGroup
} from './scripts/first-load.mjs';

// Dev, build and test all need messages/*.json; the merge is idempotent and cheap.
const merged = mergeMessages(import.meta.dirname);
if (merged.errors.length) throw new Error(merged.errors.join('\n'));

// `vite preview` has no API behind it: it would answer `/api/…` with the SPA page and a 404, a page
// the real server never sends there. Lighthouse runs against preview and counts that page's bytes
// (the search page asks `GET /auth` after mount), so answer as the server does for an unknown route:
// a short JSON error. Playwright mocks `/api` in the browser, so its requests never get here.
const previewApi: Plugin = {
	name: 'lodestar:preview-api',
	configurePreviewServer(server) {
		server.middlewares.use('/api/', (_req, res) => {
			res.statusCode = 404;
			res.setHeader('content-type', 'application/json');
			res.end('{"error":{"code":"not_found","message":"No API in preview."}}');
		});
	}
};

export default defineConfig({
	// The search route's first load carries only what it uses (scripts/first-load.mjs, §10.1),
	// and no route's preload list rides along in the route table.
	build: {
		rolldownOptions: {
			output: {
				codeSplitting: {
					groups: [vendorGroup(), statusIconsGroup(), keysGroup()]
				}
			}
		},
		modulePreload: { resolveDependencies: routePreloads }
	},
	plugins: [
		paraglideVitePlugin({
			project: './project.inlang',
			outdir: './src/lib/paraglide',
			// No URL or cookie strategy: the locale is a client setting (src/lib/i18n/locale.ts).
			strategy: ['globalVariable', 'baseLocale']
		}),
		tailwindcss(),
		sveltekit(),
		licenses(),
		chunkGraph(),
		svelteBarrel(),
		firstLoadGraph(),
		stripPreloadsOf(/\/src\/routes\/(search\/\+page|\+layout|\+error)\.svelte$/),
		Icons({ compiler: 'svelte' }),
		previewApi
	],
	resolve: process.env.VITEST ? { conditions: ['browser'] } : undefined,
	test: {
		environment: 'jsdom',
		include: ['src/**/*.test.ts', 'scripts/**/*.test.ts'],
		setupFiles: ['./vitest-setup.ts']
	}
});
