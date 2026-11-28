import { describe, expect, it } from 'vitest';
import { term } from './fixtures';
import { swarmPresets } from './presets';

describe('swarm presets (UI review I6)', () => {
	it('are the same plain clauses with no server counts, with a null count, never 0', () => {
		expect(swarmPresets('seeders', null)).toEqual([
			{ n: 1, count: null },
			{ n: 5, count: null },
			{ n: 50, count: null }
		]);
		expect(swarmPresets('trackers', null).map((p) => p.n)).toEqual([1, 3]);
	});

	it('take a count from the server bucket when it lists the preset', () => {
		const bucket = term([
			['≥1', 900],
			['≥50', 40],
			['never_scraped', 384]
		]);
		expect(swarmPresets('seeders', bucket)).toEqual([
			{ n: 1, count: 900 },
			{ n: 5, count: null },
			{ n: 50, count: 40 }
		]);
	});
});
