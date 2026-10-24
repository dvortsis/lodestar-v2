import { afterEach, describe, expect, it } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import ManualMagnet from './ManualMagnet.svelte';
import { manual } from './manual.svelte';

afterEach(() => (manual.magnet = null));

describe('ManualMagnet', () => {
	it('renders nothing until a copy fails', () => {
		render(ManualMagnet);
		expect(screen.queryByTestId('detail-magnet-field')).toBeNull();
	});

	it('shows the magnet in a selected read-only field with the Ctrl+C hint, and can be dismissed', async () => {
		render(ManualMagnet);
		manual.magnet = 'magnet:?xt=urn:btih:abc';
		const field = (await screen.findByTestId('detail-magnet-field')) as HTMLInputElement;
		expect(field.value).toBe('magnet:?xt=urn:btih:abc');
		expect(field.readOnly).toBe(true);
		await waitFor(() => expect(document.activeElement).toBe(field));
		expect(field.selectionStart).toBe(0);
		expect(field.selectionEnd).toBe(field.value.length);
		expect(screen.getByRole('group', { name: 'Magnet link' }).textContent).toMatch(
			/Press (Ctrl\+C|⌘C) to copy/
		);
		await fireEvent.click(screen.getByRole('button', { name: 'Dismiss' }));
		expect(screen.queryByTestId('detail-magnet-field')).toBeNull();
	});
});
