// Fails when two builds of the same tree differ (X-web-build-determinism). CI runs the size
// step's measurement build twice:
//
//   node scripts/check-build-stable.mjs save   .build-hashes.json   (after the first build)
//   node scripts/check-build-stable.mjs compare .build-hashes.json  (after the second)
//
// It hashes every file the size budgets read or the app ships: `build/` and the client output
// SvelteKit writes before adapting (`.svelte-kit/output/client/`, manifest included). A file that
// differs, or exists in only one build, is named; nothing is compared beyond those two trees.
import { createHash } from 'node:crypto';
import { readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { join, relative } from 'node:path';

export const DIRS = ['build', '.svelte-kit/output/client'];

/** Every file under `dirs` (relative to `root`), by path, with its SHA-256. */
export function snapshot(root, dirs = DIRS) {
	/** @type {Record<string, string>} */
	const out = {};
	const walk = (dir) => {
		for (const entry of readdirSync(dir, { withFileTypes: true })) {
			const path = join(dir, entry.name);
			if (entry.isDirectory()) walk(path);
			else if (entry.isFile()) {
				out[relative(root, path).split('\\').join('/')] = createHash('sha256')
					.update(readFileSync(path))
					.digest('hex');
			}
		}
	};
	for (const dir of dirs) walk(join(root, dir));
	return Object.fromEntries(Object.entries(out).sort(([a], [b]) => (a < b ? -1 : 1)));
}

/** What differs between two snapshots, one line per file, sorted. */
export function differences(before, after) {
	const lines = [];
	for (const path of [...new Set([...Object.keys(before), ...Object.keys(after)])].sort()) {
		if (!(path in after)) lines.push(`only in the first build: ${path}`);
		else if (!(path in before)) lines.push(`only in the second build: ${path}`);
		else if (before[path] !== after[path]) lines.push(`differs: ${path}`);
	}
	return lines;
}

if (import.meta.url === `file://${process.argv[1]}`) {
	const [mode, file] = process.argv.slice(2);
	if (!file || (mode !== 'save' && mode !== 'compare')) {
		console.error('usage: check-build-stable.mjs save|compare <hashes.json>');
		process.exit(2);
	}
	const now = snapshot(process.cwd());
	if (mode === 'save') {
		writeFileSync(file, JSON.stringify(now, null, '\t') + '\n');
		console.log(`${Object.keys(now).length} files hashed`);
	} else {
		const diff = differences(JSON.parse(readFileSync(file, 'utf8')), now);
		if (diff.length) {
			console.error(`The same tree built twice gave different output (${diff.length} files):`);
			for (const line of diff) console.error(`  ${line}`);
			process.exit(1);
		}
		console.log(`${Object.keys(now).length} files identical in both builds`);
	}
}
