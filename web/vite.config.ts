import { sveltekit } from '@sveltejs/kit/vite';
import tailwindcss from '@tailwindcss/vite';
import Icons from 'unplugin-icons/vite';
import { paraglideVitePlugin } from '@inlang/paraglide-js';
import { createHash } from 'node:crypto';
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

// The size step's measurement build (CI sets LODESTAR_STABLE_NAMES=1 there, and never for the
// shipped build): client chunks are named after the modules they hold, not their bytes. SvelteKit
// enforces content-hash names (`[hash].js`), so any change renamed every chunk that imports the
// changed one, and gzip packed the new name a few bytes differently: untouched routes drifted by
// up to ±10 B and limits at their measured size flaked (X-web-build-determinism). A name from
// the module set changes only when the chunk's make-up does. The shipped build keeps content
// hashes, which cache-busting needs: a name must change when the bytes do.
const stableNames: Plugin = {
	name: 'lodestar:stable-names',
	apply: 'build',
	enforce: 'post',
	outputOptions(opts) {
		if (!process.env.LODESTAR_STABLE_NAMES) return null;
		const pre = '_app/immutable';
		const entries = opts.entryFileNames;
		// The server build (`[name].js`) and workers keep their names.
		if (typeof entries !== 'string' || !entries.startsWith(`${pre}/`)) return null;
		if (entries.startsWith(`${pre}/workers/`)) return null;
		if (entries !== `${pre}/[name].[hash].js`) {
			throw new Error(`stable names: SvelteKit's entry pattern changed (${entries})`);
		}
		const assets = opts.assetFileNames;
		if (assets !== `${pre}/assets/[name].[hash][extname]`) {
			throw new Error(`stable names: SvelteKit's asset pattern changed (${String(assets)})`);
		}
		// 8 base64url characters, the first like rolldown's (mostly B, C or D), so each name costs
		// gzip about what a real one does and the measured size stays close to the shipped one.
		const stable = (ids: readonly string[], fallback: string) => {
			const key = ids.map((m) => m.replace(import.meta.dirname, '')).sort();
			const d = createHash('sha256')
				.update(key.join('\n') || fallback)
				.digest();
			const b = d.readUInt8(0);
			const first =
				b < 200 ? 'BCD'.charAt(b % 3) : d.subarray(9, 10).toString('base64url').charAt(0);
			return first + d.subarray(1).toString('base64url').slice(0, 7);
		};
		const name = (c: { moduleIds: readonly string[]; name: string }) => stable(c.moduleIds, c.name);
		// CSS too: importers embed its file name, so a content hash there moved their sizes
		// (X-size-ci-script). Named from its source files, or its name (unique per build) when
		// rolldown gives none. Fonts and images keep content hashes: code changes don't touch them.
		return {
			...opts,
			entryFileNames: (c) => `${pre}/${c.name}.${name(c)}.js`,
			chunkFileNames: (c) => `${pre}/chunks/${name(c)}.js`,
			assetFileNames: (a) => {
				if (!a.names.some((n) => n.endsWith('.css'))) return assets;
				return `${pre}/assets/[name].${stable(a.originalFileNames, a.names.join('\n'))}[extname]`;
			}
		};
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
		previewApi,
		stableNames
	],
	resolve: process.env.VITEST ? { conditions: ['browser'] } : undefined,
	test: {
		environment: 'jsdom',
		include: ['src/**/*.test.ts', 'scripts/**/*.test.ts'],
		setupFiles: ['./vitest-setup.ts']
	}
});
