// The wizard's settings steps label their fields the way Settings does (X-wizard-labels-signout,
// review B1): every step built on `FieldsStep` shows the same label, help, option labels and field
// menu name as the Settings form for the same keys, and none of them is the raw setting key.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, configure, render, waitFor } from '@testing-library/svelte';
import { writeProgress, type StepId } from './steps';
import { wizardServer } from './testing/server';
import { createSettingsForm, type FieldKey } from '../settings/form/embed';
import { clearIndexCache, loadSettingsIndex } from '../settings/form/schema';
import FormHarness from '../settings/form/testing/FormHarness.svelte';

vi.mock('$app/paths', () => ({
	resolve: (path: string, params?: Record<string, string>) =>
		params ? path.replace(/\[(\w+)\]/g, (_, k: string) => params[k]!) : path
}));
vi.mock('$app/navigation', () => ({ beforeNavigate: () => {}, goto: vi.fn() }));
import WizardPage from './WizardPage.svelte';

vi.setConfig({ testTimeout: 30_000 });
configure({ asyncUtilTimeout: 15_000 });

/** Every step that renders `FieldsStep`, with the settings it shows. */
const FIELD_STEPS: { step: StepId; keys: FieldKey[] }[] = [
	{ step: 'intensity', keys: ['crawler.intensity'] },
	{ step: 'storage', keys: ['storage.archive_raw', 'storage.mmap_mib'] },
	{ step: 'download', keys: ['integrations.clients'] }
];

interface Shown {
	menu: string | null;
	help: string | null;
	options: string[];
}

/** What a field shows a person: its menu's name (which carries the label), help and choices. */
function shown(key: string): Shown {
	const field = document.querySelector<HTMLElement>(`[data-field="${key}"]`)!;
	return {
		menu: field.querySelector('[aria-haspopup="menu"]')?.getAttribute('aria-label') ?? null,
		help: field.querySelector('[id$="-help"]')?.textContent?.trim() ?? null,
		options: [...field.querySelectorAll<HTMLInputElement>('input[type="radio"]')].map(
			(r) => r.labels?.[0]?.textContent?.trim() ?? ''
		)
	};
}

/** Waits for every field, its menu button and (for a choice) every option, then reads them. */
async function read(keys: FieldKey[]): Promise<Record<string, Shown>> {
	const index = await loadSettingsIndex();
	await waitFor(() => {
		for (const key of keys) {
			const field = document.querySelector(`[data-field="${key}"]`);
			expect(field?.querySelector('[aria-haspopup="menu"]'), key).toBeTruthy();
			const choices = index.spec(key)?.enum?.length ?? 0;
			expect(field!.querySelectorAll('input[type="radio"]').length, key).toBe(choices);
		}
	});
	return Object.fromEntries(keys.map((key) => [key, shown(key)]));
}

let server: ReturnType<typeof wizardServer>;
beforeEach(() => {
	sessionStorage.clear();
	clearIndexCache();
	vi.spyOn(console, 'warn').mockImplementation(() => {});
	server = wizardServer();
	vi.stubGlobal('fetch', server.fetch);
});
afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

describe('wizard field labels', () => {
	// First: the label maps load once per module, so this one sees the wizard load them itself.
	it('names the intensity choices in words', async () => {
		writeProgress({ step: 'intensity', skipped: [], done: [] });
		render(WizardPage);
		const { 'crawler.intensity': f } = await read(['crawler.intensity']);
		expect(f!.menu).toBe('Options for Crawl intensity');
		expect(f!.options).toEqual(['Low', 'Balanced', 'Aggressive']);
	});

	for (const { step, keys } of FIELD_STEPS) {
		it(`${step}: shows Settings' labels, help and options, never the raw key`, async () => {
			writeProgress({ step, skipped: [], done: [] });
			render(WizardPage);
			const wizard = await read(keys);
			cleanup();

			// The Settings form for the same keys, built the way a Settings screen builds it.
			const index = await loadSettingsIndex();
			const specs = keys.map((k) => index.spec(k)!);
			const form = createSettingsForm({ id: `settings.${step}`, fields: specs, drafts: false });
			render(FormHarness, { form, saveBar: false });
			const settings = await read(keys);
			form.destroy();

			for (const key of keys) {
				const name = key.slice(key.indexOf('.') + 1);
				const w = wizard[key]!;
				expect(w, key).toEqual(settings[key]);
				for (const raw of [key, name]) {
					expect(w.menu).not.toBe(`Options for ${raw}`);
					expect(w.options).not.toContain(raw);
				}
				for (const value of index.spec(key)!.enum ?? []) expect(w.options).not.toContain(value);
			}
			// Every field Settings gives help has it here too (the equality above), and these all do.
			expect(Object.values(wizard).every((w) => w.help)).toBe(true);
		});
	}
});
