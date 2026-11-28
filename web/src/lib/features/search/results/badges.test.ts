import { describe, expect, it } from 'vitest';
import { badgesOf, collapseBadges, MAX_BADGES, type BadgeLabels, type RowBadge } from './badges';
import { hit } from './fixtures';

const labels: BadgeLabels = {
	type: (k) => k.toUpperCase(),
	private: 'Private',
	imported: 'Imported'
};
const b = (key: string, keep = false): RowBadge => ({ key, kind: 'text', label: key, keep });
const keys = (l: RowBadge[]) => l.map((x) => x.key);

describe('badgesOf', () => {
	it("lists the type, resolution, flags, then verification and safety; v2 and hybrid are the drawer's (§10.4)", () => {
		const out = badgesOf(
			hit(1, {
				content_type: 'movie',
				resolution: '2160p',
				flags: ['v2', 'hybrid', 'private'],
				verified: false,
				swarm_suspect: true
			}),
			labels
		);
		expect(keys(out)).toEqual(['type', 'res', 'private', 'unverified', 'suspect']);
		expect(out.filter((x) => x.keep).map((x) => x.key)).toEqual(['unverified', 'suspect']);
	});

	it('reads an unverified import once, as the verification badge', () => {
		const out = badgesOf(hit(1, { flags: ['imported'], verified: false }), labels);
		expect(keys(out)).toEqual(['type', 'unverified']);
	});

	it('keeps an adult torrent’s kind on the row (content safety)', () => {
		const out = badgesOf(hit(1, { content_type: 'adult', adult_kind: 'vr' }), {
			...labels,
			type: () => 'VR'
		});
		expect(out[0]).toMatchObject({
			key: 'type',
			keep: true,
			filter: { facet: 'adult', value: 'vr' }
		});
	});

	it('gives each filterable badge the rail’s facet and value', () => {
		const out = badgesOf(
			hit(1, { content_type: 'tv', resolution: '720p', flags: ['private'] }),
			labels
		);
		expect(out.map((x) => x.filter)).toEqual([
			{ facet: 'type', value: 'tv' },
			{ facet: 'res', value: '720p' },
			{ facet: 'private', value: 'yes' }
		]);
	});
});

describe('collapseBadges', () => {
	it('shows all of five or fewer, with no chip', () => {
		const all = ['a', 'b', 'c', 'd', 'e'].map((k) => b(k));
		expect(collapseBadges(all)).toEqual({ shown: all, hidden: null });
		expect(MAX_BADGES).toBe(5);
	});

	it('over five, the chip takes one slot: four shown, the rest counted', () => {
		const all = ['a', 'b', 'c', 'd', 'e', 'f', 'g'].map((k) => b(k));
		const r = collapseBadges(all);
		expect(keys(r.shown)).toEqual(['a', 'b', 'c', 'd']);
		expect(r.hidden).toBe(3);
	});

	it('never hides a safety or verification badge, whatever its place in the list', () => {
		const all = [b('a'), b('b'), b('c'), b('d'), b('e'), b('v', true), b('s', true)];
		const r = collapseBadges(all);
		expect(keys(r.shown)).toEqual(['a', 'b', 'v', 's']);
		expect(r.hidden).toBe(3);
	});

	it('shows every keeper even when they alone fill the line', () => {
		const all = ['k1', 'k2', 'k3', 'k4', 'k5', 'k6'].map((k) => b(k, true));
		const r = collapseBadges(all);
		expect(r.shown).toHaveLength(6);
		expect(r.hidden).toBeNull();
	});
});
