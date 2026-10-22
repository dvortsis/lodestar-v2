import js from '@eslint/js';
import svelte from 'eslint-plugin-svelte';
import globals from 'globals';
import ts from 'typescript-eslint';
import svelteConfig from './svelte.config.js';
import { ARBITRARY_VALUE, FRAGMENT_ATTRIBUTES, HEX_COLOUR } from './scripts/bans.mjs';

// §10.18: no hex colours and no Tailwind arbitrary values outside the theme module.
// Shares its patterns with scripts/check-bans.mjs.
const noBannedLiterals = {
	meta: { type: 'problem', schema: [] },
	create(context) {
		function check(node, text, attribute) {
			if (!(attribute && FRAGMENT_ATTRIBUTES.test(attribute)) && HEX_COLOUR.test(text)) {
				context.report({ node, message: 'Hex colour literal: use a theme token.' });
			}
			if (ARBITRARY_VALUE.test(text)) {
				context.report({
					node,
					message: 'Tailwind arbitrary value: use a token or scale value.'
				});
			}
		}
		return {
			Literal: (node) => typeof node.value === 'string' && check(node, node.value),
			TemplateElement: (node) => check(node, node.value.raw),
			SvelteLiteral: (node) => check(node, node.value, node.parent?.key?.name)
		};
	}
};

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
