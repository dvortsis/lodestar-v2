// The grouped count line and the "+N other versions" words (UI review 02 §5.1).
import { describe, expect, it } from 'vitest';
import { groupsCount } from './count';
import { othersText, versionsCell } from './others';

describe('groupsCount', () => {
	it('reads groups, then torrents, exact', () => {
		expect(groupsCount(128, false, 412, false, 'en')).toBe('128 groups · 412 torrents');
		expect(groupsCount(1, false, 3, false, 'en')).toBe('1 group · 3 torrents');
		expect(groupsCount(1, false, 1, false, 'en')).toBe('1 group · 1 torrent');
	});
	it('marks estimates with ≈, compactly', () => {
		expect(groupsCount(2_400_000, true, 3_100_000, true, 'en')).toBe(
			'≈ 2.4M groups · ≈ 3.1M torrents'
		);
		// An estimated single group is still "groups": the figure isn't exact.
		expect(groupsCount(1, true, 412, false, 'en')).toBe('≈ 1 groups · 412 torrents');
	});
});

describe('othersText and versionsCell', () => {
	it('words one and many', () => {
		expect(othersText(1, 'en')).toBe('+1 other version');
		expect(othersText(1200, 'en')).toBe('+1,200 other versions');
	});
	it('counts the row itself in Versions, and — when ungrouped', () => {
		expect(versionsCell(3, 'en')).toBe('4');
		expect(versionsCell(0, 'en')).toBe('1');
		expect(versionsCell(undefined, 'en')).toBe('—');
	});
});
