// The controls loaded per field type (controls.svelte.ts): a placeholder of the control's empty
// height until the control arrives, then the control, working. This file's module graph is its
// own, so none of them has loaded when it starts.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import SettingField from './SettingField.svelte';
import { loadControls } from './controls.svelte';
import { createSettingsForm, type SettingsForm } from './form.svelte';
import { fakeServer } from './testing/fake-server';
import { spec, sv, values } from './testing/values';
import type { FieldKey } from './types';
import { widgetFor } from './widgets';

const SPECS = [
	spec('crawler.trackers', { kind: 'string-list' }),
	spec('crawler.extra', { kind: 'map' }),
	spec('secrets.token', { writeOnly: true }),
	spec('crawler.intensity', { kind: 'enum', enum: ['low', 'balanced'] }),
	spec('crawler.encryption', { kind: 'enum', enum: ['prefer', 'require', 'off'], widget: 'radio' }),
	spec('crawler.timeout_secs', { kind: 'integer', minimum: 1, maximum: 120 })
];

let form: SettingsForm;
const until = (check: () => void) => waitFor(check, { timeout: 10_000 });

beforeEach(async () => {
	vi.spyOn(console, 'warn').mockImplementation(() => {}); // the JSON fallback's dev warning
	const server = fakeServer([
		values('crawler', 1, {
			trackers: sv(['udp://a']),
			extra: sv({ a: 1 }),
			intensity: sv('low'),
			encryption: sv('prefer'),
			timeout_secs: sv(12)
		}),
		values('secrets', 1, { token: sv(null, 'db') })
	]);
	vi.stubGlobal('fetch', server.fetch);
	form = createSettingsForm({ id: 'lazy-controls', fields: SPECS });
	await until(() => expect(form.status).toBe('ready'));
});

afterEach(() => {
	form.destroy();
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

const show = (key: FieldKey) => render(SettingField, { form, key });
const field = (key: FieldKey) => within(document.getElementById(key)!);

// In order: each test is the first to render its control.
describe('a control loaded on demand', { timeout: 20_000 }, () => {
	it('holds the chip list’s place with an input-sized box, then shows the list', async () => {
		show('crawler.trackers');
		const box = document.querySelector<HTMLElement>('[data-placeholder]')!;
		expect(box.classList).toContain('input');
		expect(box.getAttribute('aria-hidden')).toBe('true');
		await until(() => expect(field('crawler.trackers').getByText('udp://a')).toBeTruthy());
		expect(document.querySelector('[data-placeholder]')).toBeNull();
		expect(field('crawler.trackers').getByRole('button', { name: 'Add' })).toBeTruthy();
	});

	it('holds the JSON fallback’s place with its own four-row textarea', async () => {
		show('crawler.extra');
		const box = document.querySelector<HTMLTextAreaElement>('textarea[data-placeholder]')!;
		expect(box.rows).toBe(4);
		expect(box.disabled).toBe(true);
		await until(() =>
			expect((field('crawler.extra').getByRole('textbox') as HTMLTextAreaElement).value).toBe(
				'{\n  "a": 1\n}'
			)
		);
	});

	it('loads a secret’s control, which never shows the value', async () => {
		show('secrets.token');
		expect(document.querySelector('[data-placeholder]')).not.toBeNull();
		await until(() => expect(screen.getByTestId('secret-status').textContent).toMatch(/Set/));
	});

	it('holds a radio group’s place with a row per option, then shows the options', async () => {
		show('crawler.encryption');
		const box = document.querySelector<HTMLElement>('[data-placeholder]')!;
		expect(box.children).toHaveLength(3);
		await until(() => expect(screen.getAllByRole('radio')).toHaveLength(3));
		expect(document.querySelector('[data-placeholder]')).toBeNull();
	});

	it('loads a number on demand; a segmented control shares the radio group’s file', async () => {
		show('crawler.timeout_secs');
		show('crawler.intensity'); // ChoiceControl came with the radio group above
		expect(document.querySelectorAll('[data-placeholder]')).toHaveLength(1);
		const input = await screen.findByLabelText('timeout_secs', {}, { timeout: 10_000 });
		await fireEvent.input(input, { target: { value: '30' } });
		expect(form.field('crawler.timeout_secs').value).toBe(30);
		await until(() => expect(screen.getByRole('radio', { name: 'balanced' })).toBeTruthy());
	});

	it('loads only what a form’s fields need, once', async () => {
		const loaded = vi.fn();
		await loadControls(SPECS.map(widgetFor)).then(loaded);
		expect(loaded).toHaveBeenCalledOnce();
		// Already loaded: a field renders its control at once, with no placeholder.
		show('crawler.trackers');
		expect(document.querySelector('[data-placeholder]')).toBeNull();
	});
});
