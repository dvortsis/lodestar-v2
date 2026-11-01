import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { initLocale } from '$lib/i18n/locale.svelte';
import { fakeServer } from '../form/testing/fake-server';
import { spec, sv, values } from '../form/testing/values';
import type { FieldSpec } from '../form/types';
import { screens } from './screen';

const index = vi.hoisted(() => ({ load: undefined as unknown as () => Promise<unknown> }));
vi.mock('../form/schema', async (actual) => ({
	...(await actual<typeof import('../form/schema')>()),
	loadSettingsIndex: () => index.load()
}));
vi.mock('$app/navigation', () => ({ beforeNavigate: () => {}, goto: vi.fn() }));

const region = (name: string, over: Partial<FieldSpec>) =>
	spec(`preferences.${name}`, { group: 'region', ...over });
const SPECS = [
	region('language', { order: 8, widget: 'language', auto: '' }),
	region('time_zone', { order: 9, widget: 'time-zone', auto: '' }),
	region('first_day_of_week', {
		kind: 'enum',
		enum: ['auto', 'monday', 'saturday', 'sunday'],
		order: 10
	}),
	spec('preferences.theme', { group: 'appearance' })
];
let server: ReturnType<typeof fakeServer>;
const until = (check: () => void) => waitFor(check, { timeout: 10_000 });
vi.setConfig({ testTimeout: 20_000 });

beforeEach(() => {
	sessionStorage.clear();
	vi.spyOn(console, 'warn').mockImplementation(() => {});
	index.load = () => Promise.resolve({ fieldsOf: () => SPECS });
	server = fakeServer([
		values(
			'preferences',
			1,
			{
				theme: sv('riverstone'),
				language: sv(''),
				time_zone: sv(''),
				first_day_of_week: sv('auto')
			},
			[]
		)
	]);
	vi.stubGlobal('fetch', server.fetch);
});
afterEach(() => {
	vi.unstubAllGlobals();
	initLocale('en');
});

async function open() {
	const Screen = (await screens[0]!.load!()).default;
	render(Screen);
}

/** Turns a field's Automatic switch off (it starts on, its value being empty) and checks it stays off. */
async function turnAutomaticOff(key: string) {
	const toggle = await waitFor(
		() => {
			const el = document.querySelector<HTMLInputElement>(
				`[data-field="${key}"] input[role="switch"]`
			);
			if (!el) throw new Error('no Automatic switch');
			return el;
		},
		{ timeout: 10_000 }
	);
	expect(toggle.checked).toBe(true);
	await fireEvent.click(toggle);
	await until(() => expect(toggle.checked).toBe(false));
}

describe('Language & region', () => {
	it('registers as a preferences screen', () => {
		expect(screens[0]).toMatchObject({ id: 'language', group: 'preferences' });
	});

	it('shows only the region fields, saves a language at once and applies it', async () => {
		await open();
		await screen.findByText('Time zone', undefined, { timeout: 10_000 });
		expect(screen.queryByLabelText('Theme')).toBeNull();
		await turnAutomaticOff('preferences.language');
		const select = await waitFor(
			() => {
				const el = document.querySelector('select');
				if (!el) throw new Error('no select');
				return el;
			},
			{ timeout: 10_000 }
		);
		await fireEvent.change(select, { target: { value: 'zh' } });
		await until(() => expect(document.documentElement.lang).toBe('zh'));
		const sent = server.calls.filter((c) => c.method === 'PATCH');
		expect(sent.at(-1)?.body).toMatchObject({ preferences: { language: 'zh' } });
	});

	it('refuses an unknown time zone and saves a known one', async () => {
		await open();
		await screen.findByText('Time zone', undefined, { timeout: 10_000 });
		await turnAutomaticOff('preferences.time_zone');
		const input = await waitFor(
			() => {
				const el = document.querySelector<HTMLInputElement>('input[list]');
				if (!el) throw new Error('no time zone input');
				return el;
			},
			{ timeout: 10_000 }
		);
		const patches = () => server.calls.filter((c) => c.method === 'PATCH');
		await fireEvent.input(input, { target: { value: 'Foo/Bar' } });
		await until(() => expect(input.getAttribute('aria-invalid')).toBe('true'));
		// Nothing is announced while typing; the error shows once the field is left.
		expect(screen.queryByRole('alert')).toBeNull();
		await fireEvent.blur(input);
		await until(() => expect(screen.getByRole('alert').textContent).toMatch(/isn't known/));
		await fireEvent.input(input, { target: { value: 'Europe/Ber' } });
		expect(screen.getByRole('alert')).toBeTruthy(); // stays until the value is valid
		expect(patches()).toHaveLength(0);
		await fireEvent.input(input, { target: { value: 'Europe/Berlin' } });
		await until(() =>
			expect(patches().at(-1)?.body).toMatchObject({ preferences: { time_zone: 'Europe/Berlin' } })
		);
		await until(() => expect(screen.queryByRole('alert')).toBeNull());
		// Nothing invalid was ever sent.
		expect(JSON.stringify(patches())).not.toMatch(/Foo|Europe\/Ber"/);
	});

	it('offers a retry when the schema fails to load', async () => {
		index.load = () => Promise.reject(new Error('down'));
		await open();
		await until(() => expect(screen.getByRole('button', { name: 'Retry' })).toBeTruthy());
	});

	it('says so when the server has no region fields', async () => {
		index.load = () => Promise.resolve({ fieldsOf: () => [] });
		await open();
		await until(() => expect(screen.getByText(/does not provide settings/)).toBeTruthy());
	});
});
