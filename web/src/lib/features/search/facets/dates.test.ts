import { describe, expect, it } from 'vitest';
import { dayText, parseDay, rangeOfTyped } from './dates';

const DAY = 86_400;
const d = (s: string) => Date.parse(`${s}T00:00:00Z`) / 1000;

describe('typed dates (UI review U15)', () => {
	it('reads a real day as its UTC midnight, and nothing else', () => {
		expect(parseDay('2026-11-28')).toBe(d('2026-11-28'));
		for (const bad of ['', '2026-13-01', '2026-02-30', '28/11/2026', 'x'])
			expect(parseDay(bad)).toBeNull();
		expect(dayText(d('2026-11-28'))).toBe('2026-11-28');
		expect(dayText(null)).toBe('');
	});

	it('makes whole UTC days, the way the brush does', () => {
		expect(rangeOfTyped('2026-11-01', '2026-11-03')).toEqual({
			ok: true,
			range: { lo: d('2026-11-01'), hi: d('2026-11-03') + DAY - 1 }
		});
	});

	it('From = To is that single day', () => {
		expect(rangeOfTyped('2026-11-28', '2026-11-28')).toEqual({
			ok: true,
			range: { lo: d('2026-11-28'), hi: d('2026-11-28') + DAY - 1 }
		});
	});

	it('an empty end is open, both empty is no clause', () => {
		expect(rangeOfTyped('2026-11-01', '')).toEqual({
			ok: true,
			range: { lo: d('2026-11-01'), hi: null }
		});
		expect(rangeOfTyped('', '2026-11-03')).toEqual({
			ok: true,
			range: { lo: null, hi: d('2026-11-03') + DAY - 1 }
		});
		expect(rangeOfTyped('', '')).toEqual({ ok: true, range: null });
	});

	it('refuses From after To and text that is not a day', () => {
		expect(rangeOfTyped('2026-11-05', '2026-11-03')).toEqual({ ok: false, reason: 'order' });
		expect(rangeOfTyped('nope', '')).toEqual({ ok: false, reason: 'bad-day' });
	});
});
