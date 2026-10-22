// Flags (as an annotation, not a failure) any route under src/routes that the smoke matrix does not
// cover, so the slice that adds a route knows to add it to support/routes.ts.
import { readdirSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test, expect } from '@playwright/test';
import { ROUTES } from './support/routes';

const root = fileURLToPath(new URL('../src/routes', import.meta.url));

function pages(dir: string, prefix = ''): string[] {
	return readdirSync(dir).flatMap((entry) => {
		const full = join(dir, entry);
		if (statSync(full).isDirectory()) return pages(full, `${prefix}/${entry}`);
		return entry === '+page.svelte' ? [prefix || '/'] : [];
	});
}

const pattern = (path: string) => new RegExp(`^${path.replace(/\[[^\]]+\]/g, '[^/]+')}$`);

test('every route with a page is in the smoke matrix', () => {
	const uncovered = pages(root).filter(
		(p) => !p.startsWith('/_dev') && !ROUTES.some((r) => pattern(p).test(r.path.split('?')[0]))
	);
	if (uncovered.length) {
		test.info().annotations.push({ type: 'uncovered-routes', description: uncovered.join(', ') });
	}
	expect(ROUTES.length).toBeGreaterThan(0);
});
