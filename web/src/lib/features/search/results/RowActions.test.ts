import { expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import { toasts } from '$lib/ui/feedback/toasts.svelte';
import RowActions from './RowActions.svelte';
import { hit } from './fixtures';

const label = vi.hoisted(() => ({ value: null as string | null }));
const sendHash = vi.hoisted(() => vi.fn());
vi.mock('$lib/features/detail/send', () => ({ sendLabel: async () => label.value, sendHash }));

// The magnet chunk fails to load: Copy magnet says so instead of staying silent.
vi.mock('$lib/features/detail/magnet', () => {
	throw new Error('chunk failed');
});

it('toasts an error when the magnet chunk fails to load', async () => {
	render(RowActions, { hit: hit(1), density: 'comfortable', testid: 'r' });
	screen.getByTestId('r-copy').click();
	await vi.waitFor(() => expect(toasts.items).toHaveLength(1), { timeout: 10_000 });
	expect(toasts.items.map((t) => [t.kind, t.text])).toEqual([
		['error', "Lodestar didn't answer. Check that it's running, then try again."]
	]);
});

it('adds "Send to <client>" to More only with a client, and sends the row’s hash', async () => {
	label.value = 'Send to qBittorrent';
	const h = hit(1);
	render(RowActions, { hit: h, density: 'comfortable', testid: 'r' });
	await fireEvent.click(screen.getByTestId('r-more'));
	const item = await screen.findByRole(
		'menuitem',
		{ name: 'Send to qBittorrent' },
		{ timeout: 10_000 }
	);
	await fireEvent.click(item);
	await vi.waitFor(() => expect(sendHash).toHaveBeenCalledExactlyOnceWith(h.hash), {
		timeout: 10_000
	});
});

it('has no Send item without a client', async () => {
	label.value = null;
	render(RowActions, { hit: hit(1), density: 'comfortable', testid: 'r2' });
	await fireEvent.click(screen.getByTestId('r2-more'));
	await screen.findByRole('menuitem', { name: 'Open in a new tab' }, { timeout: 10_000 });
	await vi.dynamicImportSettled();
	expect(screen.queryByRole('menuitem', { name: /Send to/ })).toBeNull();
});
