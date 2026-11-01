// The flows' two dynamic-import roots (flows-root.ts, flows-adult.ts) may be imported by
// flows-door.ts alone: a direct `import()` anywhere else puts the flows in that view's first
// load, and `.size-limit.mjs` only sees it when a measured view happens to reach it. Test files
// are not shipped and are left out. Type-only `typeof import('…')` is erased by the compiler and
// allowed.
import { readdirSync, readFileSync } from 'node:fs';
import { join, relative } from 'node:path';
import { describe, expect, it } from 'vitest';

const ROOT = /['"`][^'"`\n]*\/flows-(?:root|adult)(?:\.[jt]s)?['"`]/;
const DOOR = 'src/lib/features/filters/flows-door.ts';

/** The files in `sources` (path → text) that name a flows root, apart from the door. */
export function strayImporters(sources: Record<string, string>): string[] {
	return Object.entries(sources)
		.filter(([file]) => file !== DOOR && !/\.test\.[jt]s$/.test(file))
		.filter(([, text]) =>
			text
				.split('\n')
				.some((line) =>
					ROOT.test(line.replace(/typeof\s+import\([^)]*\)/g, '').replace(/\/\/.*$/, ''))
				)
		)
		.map(([file]) => file);
}

function walk(dir: string, out: Record<string, string> = {}): Record<string, string> {
	for (const e of readdirSync(dir, { withFileTypes: true })) {
		const p = join(dir, e.name);
		if (e.isDirectory()) walk(p, out);
		else if (/\.(?:ts|js|svelte)$/.test(e.name))
			out[relative(process.cwd(), p).replaceAll('\\', '/')] = readFileSync(p, 'utf8');
	}
	return out;
}

describe('flows roots', () => {
	it('are imported by flows-door.ts alone', () => {
		expect(strayImporters(walk('src'))).toEqual([]);
	});
	it('fail the guard when another module imports a root', () => {
		const planted = {
			[DOOR]: "export const load = () => import('./flows-root');",
			'src/lib/features/search/Planted.svelte':
				"const a = () => import('$lib/features/filters/flows-adult');",
			'src/lib/features/filters/Sibling.ts': "export const x = () => import('./flows-root.ts');",
			'src/lib/features/filters/Type.ts':
				"let B: typeof import('$lib/features/filters/flows-root').X;"
		};
		expect(strayImporters(planted)).toEqual([
			'src/lib/features/search/Planted.svelte',
			'src/lib/features/filters/Sibling.ts'
		]);
	});
});
