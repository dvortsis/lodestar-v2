import { describe, expect, it } from 'vitest';
import {
	conversionInto,
	lossRows,
	sortExceptions,
	startupState,
	type Exception,
	type FunnelTotal
} from './model';

const ex = (kind: string, severity: 'warning' | 'error', since: number): Exception => ({
	kind,
	severity,
	since,
	unit: null,
	value: null,
	fix: { kind: 'stage', target: 'index' }
});
const total = (final: number) => ({ funnel_total: { final_cohorts: final } as FunnelTotal });

describe('sortExceptions', () => {
	it('puts errors before warnings, then the oldest first', () => {
		const out = sortExceptions([
			ex('a', 'warning', 10),
			ex('b', 'error', 50),
			ex('c', 'warning', 5),
			ex('d', 'error', 20)
		]);
		expect(out.map((e) => e.kind)).toEqual(['d', 'b', 'c', 'a']);
	});
	it('does not change its input', () => {
		const input = [ex('a', 'warning', 1), ex('b', 'error', 2)];
		sortExceptions(input);
		expect(input.map((e) => e.kind)).toEqual(['a', 'b']);
	});
});

describe('startupState', () => {
	it('is not starting up with a final cohort in the range', () => {
		expect(startupState('7d', total(3), undefined)).toBe('no');
	});
	it('asks for the all-time check when the range has none', () => {
		expect(startupState('24h', total(0), undefined)).toBe('check');
	});
	it('is not starting up when 24 h has none but all has one', () => {
		expect(startupState('24h', total(0), total(5))).toBe('no');
	});
	it('is starting up when all has none either', () => {
		expect(startupState('24h', total(0), total(0))).toBe('yes');
	});
	it('needs no second request for all', () => {
		expect(startupState('all', total(0), undefined)).toBe('yes');
		expect(startupState('all', total(2), undefined)).toBe('no');
	});
});

describe('funnel helpers', () => {
	it('gives the first stage no conversion', () => {
		const t = { conversion: { peer_found: 0.5, fetched: 0.4, stored: 0.3, searchable: 0.2 } };
		expect(conversionInto(t, 'harvested')).toBeNull();
		expect(conversionInto(t, 'stored')).toBe(0.3);
		expect(conversionInto({ conversion: { ...t.conversion, stored: null } }, 'stored')).toBeNull();
	});
	it('lists only reasons that lost something, largest first', () => {
		const rows = lossRows({
			losses: { no_peers: 50, timed_out: 50, refused: 0, gave_up: 70 },
			loss_share: { no_peers: 0.3, timed_out: 0.3, refused: 0, gave_up: 0.4 }
		});
		expect(rows.map((r) => r.reason)).toEqual(['gave_up', 'no_peers', 'timed_out']);
		expect(rows[0]).toEqual({ reason: 'gave_up', count: 70, share: 0.4 });
	});
	it('keeps an unknown share as null', () => {
		expect(lossRows({ losses: { x: 1 }, loss_share: {} })[0]!.share).toBeNull();
		expect(lossRows({ losses: {}, loss_share: {} })).toEqual([]);
	});
});
