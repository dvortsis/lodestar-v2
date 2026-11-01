import { expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { toasts } from '$lib/ui/feedback/toasts.svelte';
import RowActions from './RowActions.svelte';
import { hit } from './fixtures';

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
