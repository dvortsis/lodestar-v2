import { sveltekit } from '@sveltejs/kit/vite';
import tailwindcss from '@tailwindcss/vite';
import Icons from 'unplugin-icons/vite';
import { paraglideVitePlugin } from '@inlang/paraglide-js';
import { defineConfig } from 'vitest/config';
import { mergeMessages } from './scripts/messages.ts';

// Dev, build and test all need messages/*.json; the merge is idempotent and cheap.
const merged = mergeMessages(import.meta.dirname);
if (merged.errors.length) throw new Error(merged.errors.join('\n'));

export default defineConfig({
	plugins: [
		paraglideVitePlugin({
			project: './project.inlang',
			outdir: './src/lib/paraglide',
			// No URL or cookie strategy: the locale is a client setting (src/lib/i18n/locale.ts).
			strategy: ['globalVariable', 'baseLocale']
		}),
		tailwindcss(),
		sveltekit(),
		Icons({ compiler: 'svelte' })
	],
	resolve: process.env.VITEST ? { conditions: ['browser'] } : undefined,
	test: {
		environment: 'jsdom',
		include: ['src/**/*.test.ts', 'scripts/**/*.test.ts'],
		setupFiles: ['./vitest-setup.ts']
	}
});
