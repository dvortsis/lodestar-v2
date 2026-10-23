import { describe, expect, it } from 'vitest';
import { csvHref, discoveredClause, fixHref, searchHref, stageHref } from './links';
import type { InsightsQuery } from './query';

const q = (range: InsightsQuery['range'], extra: Partial<InsightsQuery> = {}): InsightsQuery => ({
	range,
	compare: false,
	...extra
});
const custom = q('custom', { from: Date.UTC(2026, 9, 1) / 1000, to: Date.UTC(2026, 9, 8) / 1000 });

describe('stageHref', () => {
	it('carries the range', () => {
		expect(stageHref('fetch', q('24h'))).toBe('/insights/fetch?range=24h');
		expect(stageHref('index', q('7d'))).toBe('/insights/index?range=7d');
	});
	it('carries custom bounds', () => {
		expect(stageHref('discover', custom)).toBe(
			`/insights/discover?range=custom&from=${custom.from}&to=${custom.to}`
		);
	});
	it('adds compare only when asked for and active', () => {
		expect(stageHref('serve', q('30d', { compare: true }), { compare: true })).toBe(
			'/insights/serve?range=30d&compare=1'
		);
		expect(stageHref('serve', q('30d', { compare: true }))).toBe('/insights/serve?range=30d');
		expect(stageHref('serve', q('all', { compare: true }), { compare: true })).toBe(
			'/insights/serve?range=all'
		);
	});
	it('adds the loss reason', () => {
		expect(stageHref('fetch', q('7d'), { reason: 'timed_out' })).toBe(
			'/insights/fetch?range=7d&reason=timed_out'
		);
	});
});

describe('searchHref', () => {
	it('uses a discovered window per range', () => {
		expect(discoveredClause(q('24h'))).toBe('discovered:<24h');
		expect(discoveredClause(q('7d'))).toBe('discovered:<7d');
		expect(discoveredClause(q('30d'))).toBe('discovered:<30d');
		expect(discoveredClause(q('all'))).toBeUndefined();
		expect(discoveredClause(custom)).toBe('discovered:2026-10-01..2026-10-07');
	});
	it('maps a group to its content types', () => {
		const href = searchHref('group', 'video', q('7d'))!;
		expect(decodeURIComponent(href.split('q=')[1]!)).toBe('type:movie,tv,video discovered:<7d');
		expect(decodeURIComponent(searchHref('group', 'audio', q('all'))!.split('q=')[1]!)).toBe(
			'type:music,audiobook'
		);
	});
	it('links a type to itself and leaves unclassified without a link', () => {
		expect(decodeURIComponent(searchHref('type', 'tv', q('30d'))!.split('q=')[1]!)).toBe(
			'type:tv discovered:<30d'
		);
		expect(searchHref('type', 'unclassified', q('7d'))).toBeUndefined();
		expect(searchHref('group', 'unclassified', q('7d'))).toBeUndefined();
	});
	it('refuses a key that is not a plain name', () => {
		expect(searchHref('type', 'a b', q('7d'))).toBeUndefined();
		expect(searchHref('type', 'x:y', q('7d'))).toBeUndefined();
		expect(searchHref('group', 'nonsense', q('7d'))).toBeUndefined();
	});
});

describe('fixHref', () => {
	it('links a stage and a settings section', () => {
		expect(fixHref({ kind: 'stage', target: 'index' })).toBe('/insights/index');
		expect(fixHref({ kind: 'settings', target: 'crawler' })).toBe('/settings/crawler');
	});
	it('links nothing for a target that is not a plain name or an unknown kind', () => {
		for (const target of ['', 'a/b', '../x', 'Index', 'a'.repeat(33), 'a-b', 'x?y']) {
			expect(fixHref({ kind: 'settings', target }), target).toBeUndefined();
		}
		expect(fixHref({ kind: 'url', target: 'index' })).toBeUndefined();
	});
});

describe('csvHref', () => {
	it('names the panel, range and compare', () => {
		expect(csvHref('trend', q('30d', { compare: true }))).toBe(
			'/api/v1/insights/export.csv?panel=trend&range=30d&compare=1'
		);
		expect(csvHref('segments.by_group', q('7d'))).toBe(
			'/api/v1/insights/export.csv?panel=segments.by_group&range=7d'
		);
	});
});
