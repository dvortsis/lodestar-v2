import { describe, expect, it } from 'vitest';
import { screens } from '$lib/features/settings/shell/screens';
import { MORE } from './nav';

// Route files that exist: `/logs` is `src/routes/logs/+page.svelte`.
const pages = Object.keys(import.meta.glob('/src/routes/**/+page.svelte'));
const routeExists = (href: string) => pages.includes(`/src/routes${href}/+page.svelte`);

describe('overflow menu (§10.3)', () => {
	it('offers only screens that exist', () => {
		for (const item of MORE) {
			const section = /^\/settings\/([^/]+)$/.exec(item.href)?.[1];
			const exists = section ? screens.some((s) => s.id === section) : routeExists(item.href);
			expect(exists, `${item.id} → ${item.href}`).toBe(true);
		}
	});

	it('offers Logs and About, each once', () => {
		expect(MORE.map((i) => i.id)).toEqual(['logs', 'about']);
	});
});
