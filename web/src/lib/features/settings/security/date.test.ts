// The keys' date helper must say what `formatDate` says, in every locale and display zone.
import { describe, expect, it } from 'vitest';
import { formatDate } from '$lib/format';
import { setZone } from '$lib/i18n/locale.svelte';
import { keyDate } from './date';

describe('keyDate', () => {
	it('matches formatDate', () => {
		for (const zone of ['UTC', 'America/Los_Angeles', 'Asia/Shanghai', ''])
			for (const locale of ['en', 'zh'])
				for (const at of [0, 1_790_000_000, 1_790_049_999]) {
					setZone(zone);
					expect(keyDate(at, locale), `${zone} ${locale} ${at}`).toBe(formatDate(at, locale));
				}
		setZone('');
	});
});
