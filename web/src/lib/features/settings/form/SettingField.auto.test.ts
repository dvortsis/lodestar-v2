// Automatic on a field whose automatic value is the empty text: turning it off stays off, and
// only an outside change of value (Discard, Undo, a server update) turns it back on.
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, waitFor, within } from '@testing-library/svelte';
import SettingField from './SettingField.svelte';
import { fakeServer } from './testing/fake-server';
import { createSettingsForm, preloadSettingsForm, type SettingsForm } from './form.svelte';
import { loadMessages } from './messages';
import { spec, sv, values } from './testing/values';
import type { FieldKey } from './types';

const KEY: FieldKey = 'preferences.language';
const SPECS = [spec('preferences.language', { auto: '' })];
vi.setConfig({ testTimeout: 20_000 });
const until = (check: () => void) => waitFor(check, { timeout: 10_000 });

let form: SettingsForm;
let server: ReturnType<typeof fakeServer>;

beforeAll(async () => {
	await Promise.all([loadMessages(['preferences']), preloadSettingsForm(true)]);
});

afterEach(() => {
	form.destroy();
	vi.unstubAllGlobals();
});

async function open(mode: 'staged' | 'instant', saved = '') {
	vi.spyOn(console, 'warn').mockImplementation(() => {});
	sessionStorage.clear();
	server = fakeServer([values('preferences', 1, { language: sv(saved, 'db') })]);
	vi.stubGlobal('fetch', server.fetch);
	form = createSettingsForm({ id: `auto-${mode}`, fields: SPECS, mode });
	await until(() => expect(form.status).toBe('ready'));
	const view = render(SettingField, { form, key: KEY });
	const root = view.container.querySelector(`[data-field="${KEY}"]`) as HTMLElement;
	const toggle = () => within(root).getByRole('switch') as HTMLInputElement;
	return { root, toggle };
}

describe.each(['staged', 'instant'] as const)(
	'Automatic with an empty automatic value (%s)',
	(mode) => {
		it('stays off when turned off, even after its save', async () => {
			const { root, toggle } = await open(mode);
			expect(toggle().checked).toBe(true);
			await fireEvent.click(toggle());
			await until(() => expect(toggle().checked).toBe(false));
			await within(root).findByRole('textbox');
			if (mode === 'instant') await new Promise((r) => setTimeout(r, 700)); // past the debounce
			expect(toggle().checked).toBe(false);
			expect(within(root).getByRole('textbox')).toBeTruthy();
		});

		it('comes back on when Undo or Discard restores the empty value', async () => {
			const { root, toggle } = await open(mode);
			await fireEvent.click(toggle());
			await fireEvent.input(await within(root).findByRole('textbox'), { target: { value: 'de' } });
			if (mode === 'instant') {
				form.blur(KEY);
				await until(() => expect(server.calls.some((c) => c.method === 'PATCH')).toBe(true));
				await fireEvent.click(await within(root).findByRole('button', { name: 'Undo' }));
			} else form.discard();
			await until(() => expect(toggle().checked).toBe(true));
		});

		it('follows a server update to the empty value', async () => {
			const { toggle } = await open(mode, 'de');
			expect(toggle().checked).toBe(false);
			const section = server.sections.get('preferences')!;
			section.fields.language = sv('');
			section.revision = 2;
			form.received({ section: 'preferences', revision: 2 });
			await until(() => expect(toggle().checked).toBe(true));
		});
	}
);
