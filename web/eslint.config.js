import js from '@eslint/js';
import svelte from 'eslint-plugin-svelte';
import globals from 'globals';
import ts from 'typescript-eslint';
import svelteConfig from './svelte.config.js';
import { noBannedLiterals } from './scripts/bans.mjs';

export default ts.config(
	{
		ignores: [
			'build/',
			'.svelte-kit/',
			'node_modules/',
			'src/lib/api/schema.d.ts',
			'**/*.generated.*',
			'src/lib/paraglide/'
		]
	},
	js.configs.recommended,
	...ts.configs.recommended,
	...svelte.configs.recommended,
	{ languageOptions: { globals: globals.browser } },
	{
		files: ['*.config.{js,ts}', 'scripts/**', 'vitest-setup.ts', 'e2e/**'],
		languageOptions: { globals: globals.node }
	},
	{
		files: ['**/*.svelte', '**/*.svelte.ts', '**/*.svelte.js'],
		languageOptions: {
			parserOptions: {
				projectService: true,
				extraFileExtensions: ['.svelte'],
				parser: ts.parser,
				svelteConfig
			}
		}
	},
	{
		files: ['src/**/*.ts', 'src/**/*.svelte'],
		ignores: ['src/lib/theme/**', 'src/**/*.test.ts'],
		plugins: { lodestar: { rules: { 'no-banned-literals': noBannedLiterals } } },
		rules: { 'lodestar/no-banned-literals': 'error' }
	}
);
