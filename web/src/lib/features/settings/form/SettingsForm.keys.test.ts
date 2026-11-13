// A form built from bare keys gets its specs only once `GET /settings` is in, after it mounts. It
// still loads those sections' labels before its fields render: a raw key never shows
// (X-wizard-labels-signout). Its own file: the label maps load once per module.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/svelte';
import { createSettingsForm, type SettingsForm } from './form.svelte';
import FormHarness from './testing/FormHarness.svelte';
import { messagesLoaded } from './messages';
import { clearIndexCache } from './schema';
import { wizardServer } from '../../wizard/testing/server';

vi.mock('$app/navigation', () => ({ beforeNavigate: () => {}, goto: async () => {} }));
vi.setConfig({ testTimeout: 30_000 });

let form: SettingsForm;
afterEach(() => {
	form.destroy();
	vi.unstubAllGlobals();
});

describe('SettingsForm from keys', () => {
	it('loads the sections’ labels once the specs arrive, never showing a raw key', async () => {
		vi.spyOn(console, 'warn').mockImplementation(() => {});
		sessionStorage.clear();
		clearIndexCache();
		vi.stubGlobal('fetch', wizardServer().fetch);
		expect(messagesLoaded(['crawler'])).toBe(false);
		form = createSettingsForm({ id: 'keys', fields: ['crawler.intensity'], drafts: false });
		const seen: string[] = [];
		const watch = new MutationObserver(() => seen.push(document.body.textContent ?? ''));
		watch.observe(document.body, { subtree: true, childList: true, characterData: true });
		render(FormHarness, { form, saveBar: false });
		await waitFor(
			() => {
				expect(screen.getByRole('button', { name: 'Options for Crawl intensity' })).toBeTruthy();
				expect(screen.getByRole('radio', { name: 'Balanced' })).toBeTruthy();
			},
			{ timeout: 10_000 }
		);
		watch.disconnect();
		expect(seen.some((text) => /(?<!Crawl )\bintensity\b|\bbalanced\b/.test(text))).toBe(false);
	});
});
