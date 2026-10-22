import js from '@eslint/js';
import svelte from 'eslint-plugin-svelte';
import globals from 'globals';
import ts from 'typescript-eslint';
import svelteConfig from './svelte.config.js';
import { ARBITRARY_VALUE, HEX_COLOUR } from './scripts/bans.mjs';

const banned = (re) => `Literal[value=${String(re)}]`;

export default ts.config(
	{
		ignores: [
			'build/',
			'.svelte-kit/',
			'node_modules/',
			'src/lib/api/schema.d.ts',
			'**/*.generated.*'
		]
	},
	js.configs.recommended,
	...ts.configs.recommended,
	...svelte.configs.recommended,
	{
		languageOptions: { globals: { ...globals.browser, ...globals.node } }
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
		// §10.18: no hex colours, no Tailwind arbitrary values outside the theme module.
		files: ['src/**/*.ts', 'src/**/*.svelte'],
		ignores: ['src/lib/theme/**'],
		rules: {
			'no-restricted-syntax': [
				'error',
				{ selector: banned(HEX_COLOUR), message: 'Hex colour literal: use a theme token.' },
				{
					selector: banned(ARBITRARY_VALUE),
					message: 'Tailwind arbitrary value: use a token or scale value.'
				},
				{
					selector: `TemplateElement[value.raw=${String(HEX_COLOUR)}]`,
					message: 'Hex colour literal: use a theme token.'
				},
				{
					selector: `TemplateElement[value.raw=${String(ARBITRARY_VALUE)}]`,
					message: 'Tailwind arbitrary value: use a token or scale value.'
				},
				{
					selector: `SvelteLiteral[value=${String(HEX_COLOUR)}]`,
					message: 'Hex colour literal: use a theme token.'
				},
				{
					selector: `SvelteLiteral[value=${String(ARBITRARY_VALUE)}]`,
					message: 'Tailwind arbitrary value: use a token or scale value.'
				}
			]
		}
	}
);
