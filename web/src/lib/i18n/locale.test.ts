import { render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';
import {
	applyLocale,
	currentLocale,
	initLocale,
	matchLocale,
	resolveLocale
} from './locale.svelte';
import Probe from './testing/Probe.svelte';

afterEach(() => applyLocale('en'));

describe('resolveLocale', () => {
	it('prefers the setting over the browser', () => {
		expect(resolveLocale('zh', ['en-US'])).toBe('zh');
	});
	it('falls back to navigator languages by primary subtag, then en', () => {
		expect(resolveLocale(null, ['fr-FR', 'zh-CN'])).toBe('zh');
		expect(resolveLocale('xx', ['fr-FR'])).toBe('en');
		expect(resolveLocale(undefined, [])).toBe('en');
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
	it('renders zh, falling back to en where zh is not translated yet', () => {
		applyLocale('zh');
		render(Probe, { relative: '3 hours ago' });
		expect(screen.getByTestId('probe-language').textContent).toBe('中文');
		expect(screen.getByTestId('probe-age').textContent).toBe('Seen 3 hours ago');
	});
});
