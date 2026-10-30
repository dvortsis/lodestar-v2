import { beforeEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { ApiError } from '$lib/api/client';

const get = vi.hoisted(() => vi.fn());
const patch = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (orig) => ({
	...(await orig<typeof import('$lib/api/client')>()),
	api: { get, patch }
}));

import AppearanceScreen from './AppearanceScreen.svelte';
import { narrow } from './prefs';

configure({ defaultHidden: true });

const field = (value: unknown, extra = {}) => ({ value, env: null, read_only: false, ...extra });
const prefs = (over: Record<string, unknown> = {}, revision = 4) => ({
	section: 'preferences',
	revision,
	fields: {
		theme: field('riverstone'),
		theme_dark: field(''),
		mode: field('system'),
		density: field('comfortable'),
		motion: field('system'),
		illustrations: field(true),
		...Object.fromEntries(Object.entries(over).map(([k, v]) => [k, field(v)]))
	}
});
const content = (on = false) => ({
	section: 'content',
	revision: 2,
	fields: { show_adult: field(on) }
});

beforeEach(() => {
	get
		.mockReset()
		.mockImplementation(async (_p: string, o: { path: { section: string } }) =>
			o.path.section === 'content' ? content() : prefs()
		);
	patch.mockReset().mockResolvedValue({ revision: 5 });
	document.documentElement.removeAttribute('style');
	delete document.documentElement.dataset.density;
});

describe('narrow', () => {
	it('refuses a malformed preference', () => {
		expect(narrow(prefs({ mode: 'sepia' }).fields)).toBeUndefined();
		expect(narrow(prefs({ theme: 'Bad Id' }).fields)).toBeUndefined();
		expect(narrow(prefs().fields)?.density).toBe('comfortable');
	});
});

describe('AppearanceScreen', () => {
	it('has one h1 and applies a change at once, saving with the revision', async () => {
		render(AppearanceScreen);
		expect(await screen.findByRole('heading', { level: 1, name: 'Appearance' })).toBeTruthy();
		await fireEvent.click(await screen.findByRole('radio', { name: 'Compact' }));
		expect(document.documentElement.dataset.density).toBe('compact');
		await waitFor(() => expect(patch).toHaveBeenCalled());
		expect(patch.mock.calls[0]![1].body).toEqual({ density: 'compact', if_revision: 4 });
		expect(await screen.findByRole('button', { name: 'Undo' })).toBeTruthy();
	});

	it('Undo puts the previous value back', async () => {
		render(AppearanceScreen);
		await fireEvent.click(await screen.findByRole('radio', { name: 'Reduced' }));
		await fireEvent.click(await screen.findByRole('button', { name: 'Undo' }));
		await waitFor(() => expect(patch).toHaveBeenCalledTimes(2));
		expect(patch.mock.calls[1]![1].body.motion).toBe('system');
	});

	it('puts the old value back and says so when the save fails', async () => {
		patch.mockRejectedValue(new ApiError(500, 'internal', 'x'));
		render(AppearanceScreen);
		await fireEvent.click(await screen.findByRole('radio', { name: 'Compact' }));
		expect(await screen.findByText(/Couldn't save that change/)).toBeTruthy();
		expect(document.documentElement.dataset.density).toBe('comfortable');
	});

	it('asks for the 18+ confirmation before turning adult content on', async () => {
		render(AppearanceScreen);
		await fireEvent.click(await screen.findByRole('switch', { name: 'Show adult content' }));
		const calls = () => patch.mock.calls.filter((c) => c[1].path.section === 'content');
		expect(calls()).toHaveLength(0);
		await fireEvent.click(screen.getByRole('button', { name: /18 or older/ }));
		await waitFor(() => expect(calls()).toHaveLength(1));
		expect(calls()[0]![1].body).toEqual({ show_adult: true, if_revision: 2 });
	});

	it('cancelling the confirmation saves nothing', async () => {
		render(AppearanceScreen);
		await fireEvent.click(await screen.findByRole('switch', { name: 'Show adult content' }));
		await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
		expect(patch).not.toHaveBeenCalled();
	});

	it('shows a load failure', async () => {
		get.mockRejectedValue(new ApiError(500, 'internal', 'x'));
		render(AppearanceScreen);
		expect(await screen.findByText(/Couldn't load your appearance/)).toBeTruthy();
	});
});
