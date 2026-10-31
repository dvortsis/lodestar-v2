// A section's labels load before its fields render: a raw key or field name never flashes.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/svelte';
import { createSettingsForm, type SettingsForm } from './form.svelte';
import FormHarness from './testing/FormHarness.svelte';
import { fakeServer } from './testing/fake-server';
import { messagesLoaded } from './messages';
import { spec, sv, values } from './testing/values';

vi.mock('$app/navigation', () => ({ beforeNavigate: () => {}, goto: async () => {} }));

let form: SettingsForm;
afterEach(() => {
	form.destroy();
	vi.unstubAllGlobals();
});

describe('SettingsForm messages', () => {
	it('waits for the section’s map, then shows the label and never the field name', async () => {
		vi.spyOn(console, 'warn').mockImplementation(() => {});
		sessionStorage.clear();
		vi.stubGlobal('fetch', fakeServer([values('storage', 1, { mmap_mib: sv(64) })]).fetch);
		expect(messagesLoaded(['storage'])).toBe(false);
		form = createSettingsForm({
			id: 'messages',
			fields: [spec('storage.mmap_mib', { kind: 'integer', minimum: 1, maximum: 4096 })]
		});
		const seen: string[] = [];
		const watch = new MutationObserver(() => seen.push(document.body.textContent ?? ''));
		watch.observe(document.body, { subtree: true, childList: true, characterData: true });
		render(FormHarness, { form });
		await waitFor(() => expect(screen.getByText(/Database mapping limit/)).toBeTruthy(), {
			timeout: 10_000
		});
		watch.disconnect();
		expect(seen.some((text) => text.includes('mmap_mib'))).toBe(false);
	});
});
