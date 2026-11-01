import { render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

// Lets a test make the zh message for scrape_age look missing; everything else is the real catalogue.
const stub = vi.hoisted(() => ({ zhMissing: false }));
vi.mock('$lib/paraglide/messages.js', async (importOriginal) => {
	const real = await importOriginal<typeof import('$lib/paraglide/messages.js')>();
	return {
		...real,
		m: {
			...real.m,
			scrape_age: (params: { relative: string }, opts: { locale?: 'en' | 'zh' } = {}) =>
				real.m.scrape_age(params, stub.zhMissing ? { ...opts, locale: 'en' as const } : opts)
		}
	};
});
import {
	applyLocale,
	currentLocale,
	initLocale,
	matchLocale,
	resolveLocale
} from './locale.svelte';
import Probe from './testing/Probe.svelte';

afterEach(() => {
	stub.zhMissing = false;
	applyLocale('en');
	vi.doUnmock('./coverage.generated');
	vi.resetModules();
});

// Loads locale.svelte.ts against a stubbed catalogue coverage, so the gate is tested with
// known numbers rather than the live catalogue (which changes as translations land).
async function localeWithCoverage(zh: number) {
	vi.resetModules();
	vi.doMock('./coverage.generated', () => ({
		AUTO_LOCALE_COVERAGE: 0.95,
		coverage: { en: 1, zh }
	}));
	return import('./locale.svelte');
}

describe('resolveLocale', () => {
	it('prefers the setting over the browser', () => {
		expect(resolveLocale('zh', ['en-US'])).toBe('zh');
	});
	it('falls back to navigator languages by primary subtag, then en', () => {
		expect(resolveLocale(null, ['fr-FR', 'en-GB'])).toBe('en');
		expect(resolveLocale('xx', ['fr-FR'])).toBe('en');
		expect(resolveLocale(undefined, [])).toBe('en');
	});
	it('does not auto-select zh from the browser while its catalogue is mostly untranslated', async () => {
		const { resolveLocale: gated } = await localeWithCoverage(0.5);
		expect(gated(null, ['zh-CN'])).toBe('en');
		expect(gated(null, ['zh-CN', 'en-US'])).toBe('en');
		// An explicit choice still applies.
		expect(gated('zh', ['zh-CN'])).toBe('zh');
	});
	it('auto-selects zh from the browser once its catalogue is at full coverage', async () => {
		const { resolveLocale: gated } = await localeWithCoverage(1);
		expect(gated(null, ['zh-CN'])).toBe('zh');
		expect(gated(null, ['fr-FR', 'zh-TW'])).toBe('zh');
	});
	it('auto-selects a locale the covered() check accepts', () => {
		expect(matchLocale(['zh-CN'], () => true)).toBe('zh');
		expect(matchLocale(['zh-CN', 'en-US'], (l) => l === 'en')).toBe('en');
	});
	it('matches tags case-insensitively', () => {
		expect(matchLocale(['ZH-Hans-CN'])).toBe('zh');
		expect(matchLocale([null, '', 'en-GB'])).toBe('en');
	});
});

describe('locale state', () => {
	it('keeps <html lang> and currentLocale() in sync', () => {
		initLocale('zh');
		expect(document.documentElement.lang).toBe('zh');
		expect(currentLocale()).toBe('zh');
		initLocale('en');
		expect(document.documentElement.lang).toBe('en');
		expect(currentLocale()).toBe('en');
	});
});

describe('rendered messages', () => {
	it('renders en', () => {
		applyLocale('en');
		render(Probe, { relative: '3 hours ago' });
		expect(screen.getByTestId('probe-language').textContent).toBe('English');
		expect(screen.getByTestId('probe-age').textContent).toBe('Seen 3 hours ago');
	});
	it('renders the en message where a zh message is missing', () => {
		// Simulate a key with no zh entry: the zh path yields the en text, as the runtime does.
		stub.zhMissing = true;
		applyLocale('zh');
		render(Probe, { relative: '3 hours ago' });
		expect(screen.getByTestId('probe-age').textContent).toBe('Seen 3 hours ago');
	});
	it('renders zh where zh is translated', () => {
		// Every en key now has a zh message (scripts/messages.zh-complete.test.ts), so there is no
		// untranslated key left to use as a fallback probe; this checks the zh path instead.
		applyLocale('zh');
		render(Probe, { relative: '3 hours ago' });
		expect(screen.getByTestId('probe-language').textContent).toBe('中文');
		expect(screen.getByTestId('probe-age').textContent).toBe('检查于3 hours ago');
	});
});
