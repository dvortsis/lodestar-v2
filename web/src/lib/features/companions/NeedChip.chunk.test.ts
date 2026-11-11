// If the install flow's code can't be fetched (a stale tab after an upgrade), Set up says so in
// plain words and can be pressed again, rather than spinning for ever (CMP-9b).
import { describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import NeedChip from './NeedChip.svelte';

configure({ asyncUtilTimeout: 10_000 });

vi.mock('./SetupFlow.svelte', () => {
	throw new Error('Failed to fetch dynamically imported module');
});

describe('the install flow fails to load', () => {
	it('says so, stops spinning, and lets Set up be pressed again', async () => {
		render(NeedChip, { need: { companion: 'qbittorrent', state: 'missing' } });
		const button = await screen.findByTestId('companion-need-setup');
		await fireEvent.click(button);
		const alert = await screen.findByRole('alert');
		expect(alert.textContent).toContain('Reload the page');
		await waitFor(() => expect(button.getAttribute('aria-busy')).toBeNull());
		await fireEvent.click(button);
		await waitFor(() => expect(screen.getAllByRole('alert')).toHaveLength(1));
		expect(button.getAttribute('aria-busy')).toBeNull();
	});
});
