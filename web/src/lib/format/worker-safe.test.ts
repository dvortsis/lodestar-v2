// lib/format must not import `$lib/i18n` or any `.svelte` module. The file-tree web worker
// (features/detail/**/tree.worker.ts) imports these formatters, and a rune or Paraglide module in
// its graph stops the worker from loading: the tree then builds on the main thread
// (e2e/detail.routes.test.ts "3,000 files page in and build in the worker"). The display time zone
// reaches shared.ts through `globalThis.lodestarZone` instead (decisions: 2026-10-31-S-locale-boot).
import { describe, expect, it } from 'vitest';

const files = import.meta.glob<string>(['./*.ts', '!./*.test.ts'], {
	query: '?raw',
	import: 'default',
	eager: true
});
const sources = Object.entries(files);

describe('lib/format stays worker-safe', () => {
	it('finds the formatter sources', () => expect(sources.length).toBeGreaterThan(10));
	for (const [file, text] of sources) {
		it(`${file} imports no $lib/i18n or .svelte module`, () => {
			const specs = [...text.matchAll(/(?:from|import)\s*\(?\s*['"]([^'"]+)['"]/g)].map(
				(m) => m[1] ?? ''
			);
			expect(specs.filter((s) => s.includes('$lib/i18n') || s.includes('.svelte'))).toEqual([]);
		});
	}
});
