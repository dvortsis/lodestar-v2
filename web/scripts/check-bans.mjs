// Fails on hex colour literals and Tailwind arbitrary values in src/**/*.{svelte,ts}.
// Exempt: src/lib/theme/** and generated files (see bans.mjs).
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join, relative, sep } from 'node:path';
import { ARBITRARY_VALUE, HEX_COLOUR, isExempt } from './bans.mjs';

const root = new URL('..', import.meta.url).pathname;

function* walk(dir) {
	for (const name of readdirSync(dir)) {
		const path = join(dir, name);
		if (statSync(path).isDirectory()) yield* walk(path);
		else if (/\.(svelte|ts)$/.test(name)) yield path;
	}
}

const failures = [];
for (const path of walk(join(root, 'src'))) {
	const file = relative(root, path).split(sep).join('/');
	if (isExempt(file)) continue;
	readFileSync(path, 'utf8')
		.split('\n')
		.forEach((line, i) => {
			if (HEX_COLOUR.test(line)) failures.push(`${file}:${i + 1}: hex colour literal; use a token`);
			if (ARBITRARY_VALUE.test(line))
				failures.push(`${file}:${i + 1}: Tailwind arbitrary value; use a token or scale value`);
		});
}

if (failures.length) {
	console.error(failures.join('\n'));
	console.error(`\n${failures.length} lint-ban violation(s).`);
	process.exit(1);
}
console.log('check-bans: ok');
