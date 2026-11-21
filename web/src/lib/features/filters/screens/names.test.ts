// The names holder: every id reads as its code until the texts load, and after a failed load.
import { describe, expect, it, vi } from 'vitest';

describe('names before and after the texts load', () => {
	it('falls back to the code, then names it once loaded', async () => {
		vi.resetModules();
		const names = await import('./names.svelte');
		expect(names.nameOf('CS-4a')).toBe('CS-4a');
		expect(names.named('CS-4a')).toBe(false);
		expect(names.textOf('CS-4a')).toBeNull();
		await names.loadNames();
		expect(names.nameOf('CS-4a')).toBe('Child terms near sexual terms');
		expect(names.named('CS-4a')).toBe(true);
		expect(names.textOf('CS-4a')).toContain('sexual term');
		// A code it has no words for stays its code.
		expect(names.nameOf('CS-9z')).toBe('CS-9z');
		expect(names.named('CS-9z')).toBe(false);
	});
	it('shows codes when the texts fail to load, and settles rather than throwing', async () => {
		vi.resetModules();
		vi.doMock('./rule-text', () => {
			throw new Error('chunk failed');
		});
		const names = await import('./names.svelte');
		await expect(names.loadNames()).resolves.toBeUndefined();
		expect(names.nameOf('CS-4a')).toBe('CS-4a');
		expect(names.textOf('CS-4a')).toBeNull();
		vi.doUnmock('./rule-text');
	});
});
