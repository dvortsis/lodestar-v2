// Fails on hex colour literals, Tailwind arbitrary values, off-ramp spacing steps, off-scale text sizes,
// dead breakpoint variants and unweighted Phosphor icons in src/**/*.{svelte,ts,css}.
// Exempt: src/lib/theme/** and generated files (see bans.mjs).
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { findViolations, isExempt } from './bans.mjs';

const root = fileURLToPath(new URL('..', import.meta.url));

function* walk(dir) {
	for (const name of readdirSync(dir)) {
		const path = join(dir, name);
		if (statSync(path).isDirectory()) yield* walk(path);
		else if (/\.(svelte|ts|css)$/.test(name)) yield path;
	}
}

const messages = {
	hex: 'hex colour literal; use a token',
	arbitrary: 'Tailwind arbitrary value; use a token or scale value',
	spacing:
		'spacing step is off the ramp (design-tokens.md §5); use an allowed step or a layout token',
	text: 'text size is off the §8 scale (caption, small, body, body-lg, title, display)',
	breakpoint:
		'sm:/md:/lg:/xl:/2xl: compile to nothing here; use the §7 variants (medium:, large:, x-large:, xx-large:, xxx-large:)',
	icon: 'Phosphor icon without a weight; import the -bold glyph (-fill only for selected or on states)'
};

const failures = [];
for (const path of walk(join(root, 'src'))) {
	const file = relative(root, path).split(sep).join('/');
	if (isExempt(file)) continue;
	for (const { line, kind } of findViolations(readFileSync(path, 'utf8'))) {
		failures.push(`${file}:${line}: ${messages[kind]}`);
	}
}

if (failures.length) {
	console.error(failures.join('\n'));
	console.error(`\n${failures.length} lint-ban violation(s).`);
	process.exit(1);
}
console.log('check-bans: ok');
