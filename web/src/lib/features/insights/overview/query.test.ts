import { describe, expect, it } from 'vitest';
import {
	apiQuery,
	compareAllowed,
	customFromDays,
	dayOf,
	dayStart,
	lastDay,
	needsCanonical,
	parseQuery,
	searchFor
} from './query';

const p = (s: string) => new URLSearchParams(s);

describe('parseQuery', () => {
	it('defaults to 7 days without compare', () => {
		expect(parseQuery(p(''))).toEqual({ range: '7d', compare: false });
	});
	it.each(['24h', '7d', '30d', 'all'])('accepts %s', (range) => {
		expect(parseQuery(p(`range=${range}`)).range).toBe(range);
	});
	it('falls back from an unknown range', () => {
		expect(parseQuery(p('range=yesterday'))).toEqual({ range: '7d', compare: false });
		expect(parseQuery(p('range='))).toEqual({ range: '7d', compare: false });
	});
	it('needs a valid ordered from < to for custom', () => {
		expect(parseQuery(p('range=custom&from=100&to=200'))).toEqual({
			range: 'custom',
			from: 100,
			to: 200,
			compare: false
		});
		for (const bad of [
			'range=custom',
			'range=custom&from=100',
			'range=custom&from=200&to=100',
			'range=custom&from=100&to=100',
			'range=custom&from=abc&to=200',
			'range=custom&from=-5&to=200',
			'range=custom&from=1.5&to=200'
		]) {
			expect(parseQuery(p(bad)).range, bad).toBe('7d');
		}
	});
	it('reads compare only as 1', () => {
		expect(parseQuery(p('compare=1')).compare).toBe(true);
		for (const v of ['0', 'true', '', '2', 'yes']) {
			expect(parseQuery(p(`compare=${v}`)).compare, v).toBe(false);
		}
	});
	it('keeps compare when the range falls back', () => {
		expect(parseQuery(p('range=zzz&compare=1'))).toEqual({ range: '7d', compare: true });
	});
});

describe('searchFor', () => {
	it('leaves defaults out', () => {
		expect(searchFor(p(''), { range: '7d', compare: false })).toBe('');
		expect(searchFor(p('range=30d'), { range: '7d', compare: false })).toBe('');
	});
	it('writes the range, custom dates and compare', () => {
		expect(searchFor(p(''), { range: '30d', compare: true })).toBe('?range=30d&compare=1');
		expect(searchFor(p(''), { range: 'custom', from: 5, to: 9, compare: false })).toBe(
			'?range=custom&from=5&to=9'
		);
	});
	it('keeps other parameters and drops stale custom bounds', () => {
		expect(searchFor(p('density=compact&from=1&to=2'), { range: '24h', compare: false })).toBe(
			'?density=compact&range=24h'
		);
	});
	it('round-trips', () => {
		const q = { range: 'custom' as const, from: 1000, to: 2000, compare: true };
		expect(parseQuery(p(searchFor(p(''), q)))).toEqual(q);
	});
});

describe('needsCanonical', () => {
	it('is false for canonical and default URLs', () => {
		expect(needsCanonical(p(''))).toBe(false);
		expect(needsCanonical(p('range=30d&compare=1'))).toBe(false);
		expect(needsCanonical(p('density=compact'))).toBe(false);
	});
	it('is true when parsing fell back or a default was written out', () => {
		expect(needsCanonical(p('range=nope'))).toBe(true);
		expect(needsCanonical(p('range=7d'))).toBe(true);
		expect(needsCanonical(p('compare=0'))).toBe(true);
		expect(needsCanonical(p('range=custom&from=9&to=1'))).toBe(true);
	});
});

describe('compare and the API query', () => {
	it('is ignored for all', () => {
		expect(compareAllowed({ range: 'all', compare: true })).toBe(false);
		expect(apiQuery({ range: 'all', compare: true }).compare).toBeUndefined();
		expect(apiQuery({ range: '30d', compare: true }).compare).toBe(1);
		expect(apiQuery({ range: '30d', compare: false }).compare).toBeUndefined();
	});
	it('sends from and to for custom only', () => {
		expect(apiQuery({ range: 'custom', from: 1, to: 2, compare: false })).toMatchObject({
			from: 1,
			to: 2
		});
		expect(apiQuery({ range: '24h', from: 1, to: 2, compare: false }).from).toBeUndefined();
	});
});

describe('UTC days', () => {
	it('converts a chosen day to Unix seconds', () => {
		expect(dayStart('2026-10-22')).toBe(Date.UTC(2026, 9, 22) / 1000);
		expect(dayStart('2026-02-30')).toBeUndefined();
		expect(dayStart('22/10/2026')).toBeUndefined();
	});
	it('includes the last chosen day', () => {
		const r = customFromDays('2026-10-01', '2026-10-07')!;
		expect(r.from).toBe(Date.UTC(2026, 9, 1) / 1000);
		expect(r.to).toBe(Date.UTC(2026, 9, 8) / 1000);
		expect(lastDay(r.to)).toBe('2026-10-07');
		expect(dayOf(r.from)).toBe('2026-10-01');
	});
	it('accepts one day and refuses a reversed range', () => {
		expect(customFromDays('2026-10-01', '2026-10-01')).toBeDefined();
		expect(customFromDays('2026-10-02', '2026-10-01')).toBeUndefined();
		expect(customFromDays('', '2026-10-01')).toBeUndefined();
	});
});
