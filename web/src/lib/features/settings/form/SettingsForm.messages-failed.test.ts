// A section's map that can't load: the fields still show (by name), and the form says part of
// the page couldn't load, with Reload.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/svelte';

vi.mock('$app/navigation', () => ({ beforeNavigate: () => {}, goto: async () => {} }));
vi.mock('./message-map', () => ({
	LOADERS: {
		storage: () => Promise.reject(new Error('chunk')),
		group: () => Promise.resolve({ MESSAGES: {} })
	}
}));

import { createSettingsForm, type SettingsForm } from './form.svelte';
import FormHarness from './testing/FormHarness.svelte';
import { fakeServer } from './testing/fake-server';
import { spec, sv, values } from './testing/values';

let form: SettingsForm;
afterEach(() => {
	form.destroy();
	vi.unstubAllGlobals();
});

describe('SettingsForm with a message map that fails', () => {
	it('shows the fields by name and the could-not-load line', async () => {
		vi.spyOn(console, 'warn').mockImplementation(() => {});
		sessionStorage.clear();
		vi.stubGlobal('fetch', fakeServer([values('storage', 1, { mmap_mib: sv(64) })]).fetch);
		form = createSettingsForm({
			id: 'messages-failed',
			fields: [spec('storage.mmap_mib', { kind: 'integer', minimum: 1, maximum: 4096 })]
		});
		render(FormHarness, { form });
		await waitFor(() => expect(screen.getByTestId('part-failed')).toBeTruthy(), {
			timeout: 10_000
		});
		await waitFor(() => expect(screen.getByText('mmap_mib')).toBeTruthy(), { timeout: 10_000 });
	});
});
