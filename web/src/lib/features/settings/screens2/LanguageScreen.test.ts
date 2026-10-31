import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
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
	region('language', { order: 8 }),
	region('time_zone', { order: 9 }),
	region('first_day_of_week', {
		kind: 'enum',
		enum: ['auto', 'monday', 'saturday', 'sunday'],
		order: 10
	}),
	spec('preferences.theme', { group: 'appearance' })
];
const until = (check: () => void) => waitFor(check, { timeout: 10_000 });
vi.setConfig({ testTimeout: 20_000 });

beforeEach(() => {
	sessionStorage.clear();
	vi.spyOn(console, 'warn').mockImplementation(() => {});
	index.load = () => Promise.resolve({ fieldsOf: () => SPECS });
	vi.stubGlobal(
		'fetch',
		fakeServer([
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
		]).fetch
	);
});
afterEach(() => vi.unstubAllGlobals());

async function open() {
	const Screen = (await screens[0]!.load!()).default;
	render(Screen);
}

describe('Language & region', () => {
	it('registers as a preferences screen', () => {
		expect(screens[0]).toMatchObject({ id: 'language', group: 'preferences' });
	});

	it('shows only the region fields and saves an edit', async () => {
		await open();
		const language = await screen.findByLabelText('Language', undefined, { timeout: 10_000 });
		expect(screen.queryByLabelText('Theme')).toBeNull();
		await fireEvent.input(language, { target: { value: 'zh-CN' } });
		await fireEvent.click(await screen.findByRole('button', { name: /save/i }));
		await until(() => expect(screen.queryByRole('button', { name: /^save/i })).toBeNull());
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
