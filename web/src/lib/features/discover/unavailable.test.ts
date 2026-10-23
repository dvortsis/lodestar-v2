import { describe, expect, it } from 'vitest';
import type { components } from '$lib/api/schema';
import { unavailableText, type Unavailable } from './unavailable';

const ALL: Record<Unavailable, true> = {
	search_disabled: true,
	no_title: true,
	needs_swarm_history: true,
	needs_resolution_history: true,
	needs_payload_signature: true,
	needs_piece_roots: true
};
// Keeps ALL in step with the generated union: a new reason is a type error here too.
const _same: components['schemas']['Unavailable'][] = Object.keys(ALL) as Unavailable[];

describe('unavailableText', () => {
	it('words every reason, and only search_disabled has a next step', () => {
		for (const reason of _same) {
			const t = unavailableText(reason);
			expect(t.text.length, reason).toBeGreaterThan(10);
			expect(t.text, reason).not.toMatch(/^discover_/);
			expect(t.action !== undefined, reason).toBe(reason === 'search_disabled');
		}
		expect(unavailableText('search_disabled').action?.href).toBe('/settings/search');
	});

	it('does not use words the glossary bans', () => {
		for (const reason of _same)
			expect(unavailableText(reason).text).not.toMatch(/\b(hash|spam)\b/i);
	});
});
