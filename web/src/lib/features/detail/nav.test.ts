import { describe, expect, it } from 'vitest';
import { DESTINATIONS, isActive } from '$lib/shell/nav';

describe('Search is current at /t/{hash}', () => {
	it('marks only Search for a torrent path', () => {
		const hash = '/t/' + 'a'.repeat(40);
		const active = DESTINATIONS.filter((d) => isActive(d, hash)).map((d) => d.id);
		expect(active).toEqual(['search']);
		expect(isActive(DESTINATIONS[0]!, '/search')).toBe(true);
		expect(isActive(DESTINATIONS[1]!, '/discover')).toBe(true);
	});
});
