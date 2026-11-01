import { expect, it } from 'vitest';
import { render, screen, waitFor } from '@testing-library/svelte';
import Lazy from './Lazy.svelte';

it('says so, as an alert, when its chunk fails to load', async () => {
	render(Lazy, { load: () => Promise.reject(new Error('offline')) });
	const alert = await waitFor(() => screen.getByRole('alert'), { timeout: 10_000 });
	expect(alert.textContent?.trim()).not.toBe('');
});
