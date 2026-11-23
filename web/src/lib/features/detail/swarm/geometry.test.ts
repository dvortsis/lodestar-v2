import { describe, expect, it } from 'vitest';
import { buildChart } from './geometry';

const DAY = 86_400;
describe('buildChart', () => {
	it('returns null when no sample has a value', () => {
		expect(buildChart([{ at: 0, seeders: null, leechers: null }], DAY, 100, 50)).toBeNull();
		expect(buildChart([], DAY, 100, 50)).toBeNull();
	});
	it('breaks the line at a null sample instead of drawing a zero', () => {
		const c = buildChart(
			[
				{ at: 0, seeders: 5, leechers: 1 },
				{ at: DAY, seeders: 6, leechers: 1 },
				{ at: 2 * DAY, seeders: null, leechers: null },
				{ at: 3 * DAY, seeders: 7, leechers: 2 },
				{ at: 4 * DAY, seeders: 8, leechers: 2 }
			],
			DAY,
			100,
			50
		)!;
		const seeders = c.lines.find((l) => l.key === 'seeders')!;
		expect(seeders.d.match(/M/g)).toHaveLength(2);
		expect(c.max).toBe(8);
	});
	it('breaks the line where a whole bucket is missing', () => {
		const c = buildChart(
			[
				{ at: 0, seeders: 5, leechers: 1 },
				{ at: DAY, seeders: 5, leechers: 1 },
				{ at: 4 * DAY, seeders: 5, leechers: 1 },
				{ at: 5 * DAY, seeders: 5, leechers: 1 }
			],
			DAY,
			100,
			50
		)!;
		expect(c.lines[0]!.d.match(/M/g)).toHaveLength(2);
	});
	it('draws an isolated sample as a dot and a single sample mid-width', () => {
		const c = buildChart([{ at: 10, seeders: 3, leechers: null }], DAY, 100, 50)!;
		expect(c.lines[0]!.dots).toEqual([{ x: 50, y: 4 }]);
		expect(c.lines[1]!.dots).toEqual([]);
	});
	it('puts zero at the bottom', () => {
		const c = buildChart(
			[
				{ at: 0, seeders: 0, leechers: 0 },
				{ at: DAY, seeders: 10, leechers: 0 }
			],
			DAY,
			100,
			54,
			4
		)!;
		expect(c.lines[1]!.d).toBe('M4 50L96 50');
	});
});
