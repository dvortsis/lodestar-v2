// About → Legal: the notice this build shows, with its version, rendered from the server's text.
import { render, screen } from '@testing-library/svelte';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { resetNoticeCache } from '../../auth/notice';
import Legal from './Legal.svelte';

const NOTICE = {
	version: 2,
	text: '# Lodestar Legal Notice\n\n**Version 2 · 2026-11-04**\n\n## 1. Who publishes Lodestar\n\nText.'
};

beforeEach(() => {
	resetNoticeCache();
	vi.stubGlobal(
		'fetch',
		vi.fn(async () => new Response(JSON.stringify(NOTICE), { status: 200 }))
	);
});
afterEach(() => vi.unstubAllGlobals());

it('shows the notice with its version', async () => {
	render(Legal);
	const region = await screen.findByRole('region', { name: 'Legal Notice' });
	expect(region.textContent).toContain('Who publishes Lodestar');
	expect(screen.getByTestId('about-notice-version').textContent?.trim()).toBe('Version 2');
	expect(screen.getByRole('heading', { level: 3, name: '1. Who publishes Lodestar' })).toBeTruthy();
});

it('offers a retry when the notice cannot be loaded', async () => {
	vi.stubGlobal(
		'fetch',
		vi.fn(async () => new Response('{}', { status: 500 }))
	);
	render(Legal);
	expect(await screen.findByText(/Couldn't load the Legal Notice/)).toBeTruthy();
	expect(screen.queryByTestId('about-notice-version')).toBeNull();
});
