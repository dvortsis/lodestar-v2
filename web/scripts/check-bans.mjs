// Fails on hex colour literals, Tailwind arbitrary values and off-ramp spacing steps in src/**/*.{svelte,ts}.
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
		else if (/\.(svelte|ts)$/.test(name)) yield path;
	}
}

const messages = {
	hex: 'hex colour literal; use a token',
	arbitrary: 'Tailwind arbitrary value; use a token or scale value',
	spacing:
		'spacing step is off the ramp (design-tokens.md §5); use an allowed step or a layout token'
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
