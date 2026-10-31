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

describe('Language & region', () => {
	it('registers as a preferences screen', () => {
		expect(screens[0]).toMatchObject({ id: 'language', group: 'preferences' });
	});

	it('shows only the region fields, saves a language at once and applies it', async () => {
		await open();
		await screen.findByText('Time zone', undefined, { timeout: 10_000 });
		expect(screen.queryByLabelText('Theme')).toBeNull();
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
