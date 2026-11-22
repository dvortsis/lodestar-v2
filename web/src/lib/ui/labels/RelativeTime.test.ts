// RelativeTime passes the catalogue's `common_just_now` (X-pseudo-text-gaps), so the moment reads
// as the catalogue says in every locale, not as text kept in the formatter.
import { cleanup, render } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { initLocale } from '$lib/i18n/locale.svelte';
import { m } from '$lib/paraglide/messages.js';
import RelativeTime from './RelativeTime.svelte';

const NOW = Date.parse('2026-10-22T12:00:00Z') / 1000;

beforeEach(() => {
	vi.useFakeTimers();
	vi.setSystemTime(NOW * 1000);
});
afterEach(() => {
	cleanup();
	vi.useRealTimers();
	initLocale('en');
});

describe('RelativeTime', () => {
	for (const [locale, want] of [
		['en', 'just now'],
		['zh', '刚刚']
	] as const) {
		it(`shows the catalogue's just_now in ${locale}`, () => {
			initLocale(locale);
			const { container } = render(RelativeTime, { seconds: NOW - 10, timeZone: 'UTC' });
			const shown = container.querySelector('time')!.firstChild!.textContent!.trim();
			expect(shown).toBe(m.common_just_now());
			expect(shown).toBe(want);
		});
	}
});
