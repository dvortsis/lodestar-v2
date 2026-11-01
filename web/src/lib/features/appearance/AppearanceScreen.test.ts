import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { ApiError } from '$lib/api/client';

const get = vi.hoisted(() => vi.fn());
const patch = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (orig) => ({
	...(await orig<typeof import('$lib/api/client')>()),
	api: { get, patch }
}));

import AppearanceScreen from './AppearanceScreen.svelte';
import { narrow } from './prefs';
import { savePending } from './themes';

configure({ defaultHidden: true, asyncUtilTimeout: 10_000 });
/** Waits for a condition, with the 10 s deadline the slowest CI runner needs. */
const wait = (check: () => void) => vi.waitFor(check, { timeout: 10_000 });
/** The next save stays open until `release` (or `fail`) is called. */
const hold = () => {
	const h: { release: () => void; fail: (e: unknown) => void } = { release() {}, fail() {} };
	patch.mockImplementationOnce(
		() =>
			new Promise((resolve, reject) => {
				h.release = () => resolve({ revision: 5 });
				h.fail = reject;
			})
	);
	return h;
};
/** Serves a `themes.json` with one family, Moss. */
const stubThemes = () => {
	const entry = (id: string, mode: string) => ({
		...{ id, name: id, family: 'Moss', group: 'original', mode, contrast: 'AA' },
		...{ values: [`#${(0x111111).toString(16)}`], keys: ['--color-base-100'] }
	});
	const file = {
		...{ v: 1, keys: ['--color-base-100'] },
		themes: [entry('moss-light', 'light'), entry('moss-dark', 'dark')]
	};
	vi.stubGlobal('fetch', async () => new Response(JSON.stringify(file)));
};
const lockedDensity = () => ({
	...prefs(),
	fields: { ...prefs().fields, density: field('comfortable', { env: 'LODESTAR_DENSITY' }) }
});

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

afterEach(() => vi.unstubAllGlobals());

beforeEach(() => {
	vi.stubGlobal('matchMedia', () => ({
		matches: false,
		addEventListener() {},
		removeEventListener() {}
	}));
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

	it('sends confirm_adult with the real 18+ dialog, and nothing on Cancel', async () => {
		render(AppearanceScreen);
		const contentCalls = () =>
			patch.mock.calls.filter(
				(c) => c[0] === '/settings/{section}' && c[1].path.section === 'content'
			);
		await fireEvent.click(await screen.findByRole('switch', { name: 'Show adult content' }));
		await screen.findByRole('dialog', { name: 'Show adult content?' });
		await fireEvent.click(screen.getByTestId('adult-cancel'));
		await waitFor(() => expect(screen.queryByTestId('adult-cancel')).toBeNull());
		expect(contentCalls()).toHaveLength(0);
		expect(
			(screen.getByRole('switch', { name: 'Show adult content' }) as HTMLInputElement).checked
		).toBe(false);
		await fireEvent.click(screen.getByRole('switch', { name: 'Show adult content' }));
		await screen.findByRole('dialog', { name: 'Show adult content?' });
		await fireEvent.click(screen.getByTestId('adult-show'));
		await waitFor(() => expect(contentCalls()).toHaveLength(1));
		expect(contentCalls()[0]![1].body).toEqual({ show_adult: true, confirm_adult: true });
	}, 20_000);

	it('shows the switch as the server has it when turning adult off fails', async () => {
		get.mockImplementation(async (_p: string, o: { path: { section: string } }) =>
			o.path.section === 'content' ? content(true) : prefs()
		);
		patch.mockRejectedValue(new ApiError(500, 'internal', 'x'));
		render(AppearanceScreen);
		const toggle = await screen.findByRole('switch', { name: 'Show adult content' });
		expect((toggle as HTMLInputElement).checked).toBe(true);
		await fireEvent.click(toggle);
		expect(await screen.findByText(/Couldn't change this setting/)).toBeTruthy();
		await waitFor(() =>
			expect(
				(screen.getByRole('switch', { name: 'Show adult content' }) as HTMLInputElement).checked
			).toBe(true)
		);
	});

	it('locks adult content set by the environment', async () => {
		get.mockImplementation(async (_p: string, o: { path: { section: string } }) =>
			o.path.section === 'content'
				? {
						...content(false),
						fields: { show_adult: field(false, { env: 'LODESTAR_SHOW_ADULT' }) }
					}
				: prefs()
		);
		render(AppearanceScreen);
		const toggle = await screen.findByRole('switch', { name: 'Show adult content' });
		expect((toggle as HTMLInputElement).disabled).toBe(true);
		expect(screen.getByText(/LODESTAR_SHOW_ADULT/)).toBeTruthy();
	});

	it('saves one change at a time, each with the revision the last one returned', async () => {
		let release!: () => void;
		patch.mockImplementationOnce(() => new Promise((r) => (release = () => r({ revision: 5 }))));
		patch.mockResolvedValueOnce({ revision: 6 });
		render(AppearanceScreen);
		await fireEvent.click(await screen.findByRole('radio', { name: 'Compact' }));
		await fireEvent.click(screen.getByRole('radio', { name: 'Reduced' }));
		expect(patch).toHaveBeenCalledTimes(1);
		release();
		await waitFor(() => expect(patch).toHaveBeenCalledTimes(2));
		expect(patch.mock.calls[0]![1].body.if_revision).toBe(4);
		expect(patch.mock.calls[1]![1].body.if_revision).toBe(5);
	});

	it('marks saves pending until the last queued one ends, success or failure', async () => {
		let release!: () => void;
		patch.mockImplementationOnce(() => new Promise((r) => (release = () => r({ revision: 5 }))));
		patch.mockRejectedValueOnce(new ApiError(500, 'internal', 'x'));
		render(AppearanceScreen);
		await fireEvent.click(await screen.findByRole('radio', { name: 'Compact' }));
		await fireEvent.click(screen.getByRole('radio', { name: 'Reduced' }));
		expect(savePending()).toBe(true);
		release();
		await screen.findByText(/Couldn't save that change/);
		await waitFor(() => expect(savePending()).toBe(false));
	});

	it('a failed save puts back only its own field', async () => {
		patch.mockResolvedValueOnce({ revision: 5 });
		patch.mockRejectedValueOnce(new ApiError(500, 'internal', 'x'));
		render(AppearanceScreen);
		await fireEvent.click(await screen.findByRole('radio', { name: 'Reduced' }));
		await fireEvent.click(screen.getByRole('radio', { name: 'Compact' }));
		await screen.findByText(/Couldn't save that change/);
		expect(screen.getByRole('radio', { name: 'Reduced' }).getAttribute('aria-checked')).toBe(
			'true'
		);
		expect(screen.getByRole('radio', { name: 'Comfortable' }).getAttribute('aria-checked')).toBe(
			'true'
		);
	});

	it('reloads on a stale revision', async () => {
		patch.mockRejectedValueOnce(new ApiError(409, 'stale_revision', 'x'));
		render(AppearanceScreen);
		await fireEvent.click(await screen.findByRole('radio', { name: 'Compact' }));
		await waitFor(() =>
			expect(get.mock.calls.filter((c) => c[1].path.section === 'preferences').length).toBe(2)
		);
	});

	it('does not save a field the environment sets, and says which', async () => {
		get.mockImplementation(async (_p: string, o: { path: { section: string } }) =>
			o.path.section === 'content'
				? content()
				: {
						...prefs(),
						fields: {
							...prefs().fields,
							density: field('comfortable', { env: 'LODESTAR_DENSITY' })
						}
					}
		);
		render(AppearanceScreen);
		expect(await screen.findByText(/density \(LODESTAR_DENSITY\)/)).toBeTruthy();
		const compact = screen.getByRole('radio', { name: 'Compact' }) as HTMLInputElement;
		expect(compact.disabled).toBe(true);
	});

	it('offers Undo only once nothing later is saving, and then it reverts the latest change', async () => {
		patch.mockResolvedValueOnce({ revision: 5 });
		const b = { release: () => {} };
		patch.mockImplementationOnce(() => new Promise((r) => (b.release = () => r({ revision: 6 }))));
		render(AppearanceScreen);
		await fireEvent.click(await screen.findByRole('radio', { name: 'Compact' }));
		await fireEvent.click(screen.getByRole('radio', { name: 'Reduced' }));
		// A has saved, B is still open: no Undo for the older change.
		await wait(() => expect(patch).toHaveBeenCalledTimes(2));
		expect(screen.queryByRole('button', { name: 'Undo' })).toBeNull();
		b.release();
		await fireEvent.click(await screen.findByRole('button', { name: 'Undo' }));
		await wait(() => expect(patch).toHaveBeenCalledTimes(3));
		expect(patch.mock.calls[2]![1].body).toEqual({ motion: 'system', if_revision: 6 });
	});

	it('Undo moves focus to the control the change was made with', async () => {
		render(AppearanceScreen);
		await fireEvent.click(await screen.findByRole('radio', { name: 'Compact' }));
		await fireEvent.click(await screen.findByRole('button', { name: 'Undo' }));
		await wait(() =>
			expect(document.activeElement).toBe(screen.getByRole('radio', { name: 'Comfortable' }))
		);
		await fireEvent.click(screen.getByRole('switch', { name: 'Show illustrations' }));
		await fireEvent.click(await screen.findByRole('button', { name: 'Undo' }));
		await wait(() =>
			expect(document.activeElement).toBe(
				screen.getByRole('switch', { name: 'Show illustrations' })
			)
		);
	});

	it('Undo of a theme focuses the checked theme in the picker', async () => {
		stubThemes();
		render(AppearanceScreen);
		await fireEvent.click(await screen.findByRole('button', { name: 'Browse themes' }));
		const other = await screen.findByRole('radio', { name: /Moss/ });
		await fireEvent.click(other);
		await fireEvent.click(await screen.findByRole('button', { name: 'Undo' }));
		await wait(() => expect((document.activeElement as HTMLInputElement).checked).toBe(true));
		expect((document.activeElement as HTMLInputElement).type).toBe('radio');
		expect(document.activeElement).not.toBe(other);
	});

	it('Undo of a theme whose tile is filtered out focuses the filter', async () => {
		stubThemes();
		render(AppearanceScreen);
		await fireEvent.click(await screen.findByRole('button', { name: 'Browse themes' }));
		await fireEvent.click(await screen.findByRole('radio', { name: /Moss/ }));
		const filter = document.querySelector<HTMLSelectElement>('#ap-filter')!;
		await fireEvent.change(filter, { target: { value: 'community' } });
		await wait(() => expect(screen.queryByRole('radio', { name: /Moss/ })).toBeNull());
		await fireEvent.click(await screen.findByRole('button', { name: 'Undo' }));
		await wait(() => expect(document.activeElement).toBe(filter));
		expect(patch.mock.calls[1]![1].body.theme).toBe('riverstone');
	});

	it('Undo of turning the dark theme off turns it back on and focuses its select', async () => {
		stubThemes();
		get.mockImplementation(async (_p: string, o: { path: { section: string } }) =>
			o.path.section === 'content' ? content() : prefs({ theme_dark: 'moss' })
		);
		render(AppearanceScreen);
		await fireEvent.click(await screen.findByRole('button', { name: 'Browse themes' }));
		const other = await screen.findByRole('switch', { name: /different theme in dark mode/i });
		await wait(() => expect((other as HTMLInputElement).checked).toBe(true));
		await fireEvent.click(other);
		await wait(() => expect(patch.mock.calls[0]![1].body.theme_dark).toBe(''));
		await fireEvent.click(await screen.findByRole('button', { name: 'Undo' }));
		await wait(() => expect(document.activeElement?.id).toBe('ap-dark'));
		expect(patch.mock.calls[1]![1].body.theme_dark).toBe('moss');
		expect(
			(screen.getByRole('switch', { name: /different theme in dark mode/i }) as HTMLInputElement)
				.checked
		).toBe(true);
	});

	it('Undo of the mode focuses the mode that is back', async () => {
		render(AppearanceScreen);
		await fireEvent.click(await screen.findByRole('radio', { name: 'Dark' }));
		await fireEvent.click(await screen.findByRole('button', { name: 'Undo' }));
		await wait(() =>
			expect(document.activeElement).toBe(
				within(screen.getByRole('radiogroup', { name: 'Mode' })).getByRole('radio', {
					name: 'System'
				})
			)
		);
	});

	it('Undo never restores a value that never saved', async () => {
		const a = hold();
		render(AppearanceScreen);
		await fireEvent.click(await screen.findByRole('radio', { name: 'Compact' }));
		await fireEvent.click(screen.getByRole('radio', { name: 'Cards' }));
		a.fail(new ApiError(500, 'internal', 'x'));
		await fireEvent.click(await screen.findByRole('button', { name: 'Undo' }));
		await wait(() => expect(patch).toHaveBeenCalledTimes(3));
		expect(patch.mock.calls[2]![1].body.density).toBe('comfortable');
	});

	it('a re-read keeps a change that is still waiting to save', async () => {
		const a = hold();
		render(AppearanceScreen);
		await fireEvent.click(await screen.findByRole('radio', { name: 'Compact' }));
		await fireEvent.click(screen.getByRole('radio', { name: 'Reduced' }));
		a.fail(new ApiError(409, 'stale_revision', 'x'));
		await wait(() => expect(patch).toHaveBeenCalledTimes(2));
		await wait(() => expect(savePending()).toBe(false));
		expect(screen.getByRole('radio', { name: 'Reduced' }).getAttribute('aria-checked')).toBe(
			'true'
		);
	});

	it('hides Undo when the field it would change is locked', async () => {
		get.mockImplementation(async (_p: string, o: { path: { section: string } }) =>
			o.path.section === 'content'
				? content()
				: get.mock.calls.filter((c) => c[1].path.section === 'preferences').length > 1
					? lockedDensity()
					: prefs()
		);
		const a = hold();
		render(AppearanceScreen);
		await fireEvent.click(await screen.findByRole('radio', { name: 'Compact' }));
		await fireEvent.click(screen.getByRole('radio', { name: 'Cards' }));
		// The mock accepts B on purpose: it stands in for Undo meeting a lock that arrived meanwhile.
		a.fail(new ApiError(409, 'stale_revision', 'x'));
		await screen.findByText(/density \(LODESTAR_DENSITY\)/);
		await wait(() => expect(savePending()).toBe(false));
		expect(patch).toHaveBeenCalledTimes(2);
		expect(screen.queryByRole('button', { name: 'Undo' })).toBeNull();
	});

	it('a later save that succeeds clears the error banner', async () => {
		const a = hold();
		render(AppearanceScreen);
		await fireEvent.click(await screen.findByRole('radio', { name: 'Compact' }));
		await fireEvent.click(screen.getByRole('radio', { name: 'Reduced' }));
		a.fail(new ApiError(500, 'internal', 'x'));
		await screen.findByRole('button', { name: 'Undo' });
		expect(screen.queryByText(/Couldn't save that change/)).toBeNull();
	});

	it('a 2xx it cannot read re-reads the server and shows no error', async () => {
		patch.mockResolvedValueOnce({});
		render(AppearanceScreen);
		await fireEvent.click(await screen.findByRole('radio', { name: 'Compact' }));
		await wait(() =>
			expect(get.mock.calls.filter((c) => c[1].path.section === 'preferences').length).toBe(2)
		);
		await wait(() => expect(savePending()).toBe(false));
		expect(screen.queryByText(/Couldn't save that change/)).toBeNull();
		expect(screen.queryByRole('button', { name: 'Undo' })).toBeNull();
	});

	it('shows a load failure', async () => {
		get.mockRejectedValue(new ApiError(500, 'internal', 'x'));
		render(AppearanceScreen);
		expect(await screen.findByText(/Couldn't load your appearance/)).toBeTruthy();
	});
});

describe('Save-Data hint', () => {
	const hint = /data saver is on/;
	it('shows under the toggle while Save-Data is on', async () => {
		vi.stubGlobal('navigator', { ...navigator, connection: { saveData: true } });
		get.mockResolvedValue(prefs());
		render(AppearanceScreen);
		expect(await screen.findByText(hint)).toBeTruthy();
		vi.unstubAllGlobals();
	});
	it('is absent otherwise', async () => {
		get.mockResolvedValue(prefs());
		render(AppearanceScreen);
		await screen.findByText('Show illustrations');
		expect(screen.queryByText(hint)).toBeNull();
	});
});
