import { beforeEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen } from '@testing-library/svelte';
import { ApiError } from '$lib/api/client';
import { keys } from '$lib/keys';

const get = vi.hoisted(() => vi.fn());
const patch = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (orig) => ({
	...(await orig<typeof import('$lib/api/client')>()),
	api: { get, patch }
}));

import KeyboardScreen from './KeyboardScreen.svelte';

configure({ defaultHidden: true, asyncUtilTimeout: 10_000 });
/** Waits for a condition, with the 10 s deadline the slowest CI runner needs. */
const wait = (check: () => void) => vi.waitFor(check, { timeout: 10_000 });

const section = (
	shortcuts: unknown,
	shortcut_keys: unknown,
	revision = 7,
	locks: { env?: string; read_only?: boolean } = {}
) => ({
	section: 'preferences',
	revision,
	restart_required: [],
	fields: {
		shortcuts: {
			value: shortcuts,
			env: locks.env ?? null,
			read_only: locks.read_only ?? false,
			source: locks.env ? 'env' : 'database'
		},
		shortcut_keys: { value: shortcut_keys, env: null, read_only: false, source: 'database' }
	}
});
const press = (key: string) => fireEvent.keyDown(document.body, { key });
const changeKey = async (label: string, key: string) => {
	await fireEvent.click(screen.getByRole('button', { name: `Change the key for ${label}` }));
	await press(key);
};
const keyOf = (id: string) => screen.getByTestId(`shortcut-key-${id}`).textContent;
const body = (call: number) => patch.mock.calls[call]![1].body as Record<string, unknown>;

beforeEach(() => {
	get.mockReset().mockResolvedValue(section(true, {}));
	patch.mockReset();
	keys.setSingleKeys(true);
});

describe('KeyboardScreen', () => {
	it('shows the heading, the purpose, the switch and the editor, and no Save', async () => {
		render(KeyboardScreen);
		expect(await screen.findByRole('heading', { level: 1, name: 'Keyboard' })).toBeTruthy();
		const toggle = await screen.findByRole('switch', { name: 'Single-key shortcuts' });
		expect((toggle as HTMLInputElement).checked).toBe(true);
		expect(toggle.getAttribute('aria-describedby')).toBe('shortcuts-single-help');
		expect(screen.getByTestId('shortcuts-field')).toBeTruthy();
		expect(screen.queryByRole('button', { name: 'Save' })).toBeNull();
	});

	it('a toggle applies and saves at once, with if_revision, then offers Undo', async () => {
		patch.mockResolvedValueOnce(section(false, {}, 8));
		render(KeyboardScreen);
		await fireEvent.click(await screen.findByRole('switch'));
		expect(keys.singleKeys).toBe(false);
		await wait(() => expect(patch).toHaveBeenCalledTimes(1));
		expect(patch).toHaveBeenCalledWith('/settings/{section}', {
			path: { section: 'preferences' },
			body: { shortcuts: false, if_revision: 7 }
		});
		const status = await screen.findByText('Saved.');
		expect(status.getAttribute('role')).toBe('status');
		patch.mockResolvedValueOnce(section(true, {}, 9));
		await fireEvent.click(screen.getByRole('button', { name: 'Undo' }));
		await wait(() => expect(patch).toHaveBeenCalledTimes(2));
		expect(body(1)).toEqual({ shortcuts: true, if_revision: 8 });
		await wait(() => expect(keys.singleKeys).toBe(true));
		// Undoing is not itself undoable.
		await wait(() => expect(screen.queryByRole('button', { name: 'Undo' })).toBeNull());
	});

	it('a remap saves at once and the registry follows; Undo puts the old key back', async () => {
		patch.mockResolvedValueOnce(section(true, { 'results.next': 'n' }, 8));
		render(KeyboardScreen);
		await screen.findByRole('switch');
		await changeKey('Next result', 'n');
		await wait(() => expect(patch).toHaveBeenCalledTimes(1));
		expect(body(0)).toEqual({ shortcut_keys: { 'results.next': 'n' }, if_revision: 7 });
		await screen.findByText('Saved.');
		expect(keyOf('results.next')).toContain('N');
		patch.mockResolvedValueOnce(section(true, {}, 9));
		await fireEvent.click(screen.getByRole('button', { name: 'Undo' }));
		await wait(() => expect(patch).toHaveBeenCalledTimes(2));
		expect(body(1)).toEqual({ shortcut_keys: {}, if_revision: 8 });
		await wait(() => expect(keyOf('results.next')).toContain('J'));
	});

	it('a clashing key saves nothing until Swap, and Cancel saves nothing at all', async () => {
		render(KeyboardScreen);
		await screen.findByRole('switch');
		// c is Copy magnet's key.
		await changeKey('Next result', 'c');
		expect(screen.getByText('Already used by Copy magnet')).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
		expect(screen.queryByText('Already used by Copy magnet')).toBeNull();
		expect(keyOf('results.next')).toContain('J');
		// Nothing was sent: the one save below is Swap's.

		patch.mockResolvedValueOnce(
			section(true, { 'results.next': 'c', 'detail.copy_magnet': 'j' }, 8)
		);
		await changeKey('Next result', 'c');
		expect(patch).not.toHaveBeenCalled();
		await fireEvent.click(screen.getByRole('button', { name: 'Swap' }));
		await wait(() => expect(patch).toHaveBeenCalledTimes(1));
		expect(body(0)).toEqual({
			shortcut_keys: { 'results.next': 'c', 'detail.copy_magnet': 'j' },
			if_revision: 7
		});
	});

	it('changes made quickly save one at a time, each with the revision the last returned', async () => {
		let release!: () => void;
		patch.mockImplementationOnce(
			() =>
				new Promise((resolve) => {
					release = () => resolve(section(false, {}, 8));
				})
		);
		patch.mockResolvedValueOnce(section(false, { 'results.next': 'n' }, 9));
		render(KeyboardScreen);
		await fireEvent.click(await screen.findByRole('switch'));
		await changeKey('Next result', 'n');
		await wait(() => expect(patch).toHaveBeenCalledTimes(1));
		release();
		await wait(() => expect(patch).toHaveBeenCalledTimes(2));
		expect(body(1)).toEqual({ shortcut_keys: { 'results.next': 'n' }, if_revision: 8 });
		await screen.findByText('Saved.');
		expect(keyOf('results.next')).toContain('N');
		expect((screen.getByRole('switch') as HTMLInputElement).checked).toBe(false);
	});

	it('offers Undo only once nothing later is still saving, and then for the latest change', async () => {
		let release!: () => void;
		patch.mockResolvedValueOnce(section(false, {}, 8));
		patch.mockImplementationOnce(
			() =>
				new Promise((resolve) => {
					release = () => resolve(section(false, { 'results.next': 'n' }, 9));
				})
		);
		render(KeyboardScreen);
		await fireEvent.click(await screen.findByRole('switch'));
		await changeKey('Next result', 'n');
		// The switch's save finished, the remap's is still open: no Undo for the older change.
		await wait(() => expect(patch).toHaveBeenCalledTimes(2));
		expect(screen.queryByRole('button', { name: 'Undo' })).toBeNull();
		release();
		await screen.findByText('Saved.');
		patch.mockResolvedValueOnce(section(false, {}, 10));
		await fireEvent.click(screen.getByRole('button', { name: 'Undo' }));
		await wait(() => expect(patch).toHaveBeenCalledTimes(3));
		expect(body(2)).toEqual({ shortcut_keys: {}, if_revision: 9 });
	});

	it('hides Undo once the field it would change is locked', async () => {
		patch.mockResolvedValueOnce(section(false, {}, 8, { env: 'LODESTAR_SHORTCUTS' }));
		render(KeyboardScreen);
		await fireEvent.click(await screen.findByRole('switch'));
		await wait(() => expect(screen.getByText('Set by LODESTAR_SHORTCUTS')).toBeTruthy());
		expect(screen.queryByRole('button', { name: 'Undo' })).toBeNull();
	});

	it('a later save that succeeds clears the error banner', async () => {
		patch
			.mockRejectedValueOnce(new ApiError(500, 'internal', 'boom'))
			.mockResolvedValueOnce(section(false, {}, 8));
		render(KeyboardScreen);
		await fireEvent.click(await screen.findByRole('switch'));
		expect(await screen.findByText(/Couldn't save that change/)).toBeTruthy();
		await fireEvent.click(screen.getByRole('switch'));
		await screen.findByText('Saved.');
		expect(screen.queryByText(/Couldn't save that change/)).toBeNull();
	});

	it('a 2xx it cannot read re-reads the server and shows no error', async () => {
		patch.mockResolvedValueOnce({ revision: 8, fields: {} });
		get.mockResolvedValue(section(false, {}, 8));
		render(KeyboardScreen);
		await fireEvent.click(await screen.findByRole('switch'));
		await wait(() => expect(get).toHaveBeenCalledTimes(2));
		await wait(() => expect((screen.getByRole('switch') as HTMLInputElement).checked).toBe(false));
		expect(screen.queryByText(/Couldn't save that change/)).toBeNull();
		expect(screen.queryByText('Saved.')).toBeNull();
	});

	it('a stale revision reloads and lays only this change over the newer settings', async () => {
		render(KeyboardScreen);
		await screen.findByRole('switch');
		// Another device turned single keys off and remapped Focus search to f.
		get.mockResolvedValue(section(false, { 'search.focus': 'f' }, 8));
		patch
			.mockRejectedValueOnce(new ApiError(409, 'stale_revision', 'stale'))
			.mockResolvedValueOnce(section(false, { 'search.focus': 'f', 'results.next': 'n' }, 9));
		await changeKey('Next result', 'n');
		expect(
			await screen.findByText('Changed in another tab or device too; both changes were kept.')
		).toBeTruthy();
		expect(patch).toHaveBeenCalledTimes(2);
		// Their remap kept, and the switch (which this change didn't touch) not sent at all.
		expect(body(1)).toEqual({
			shortcut_keys: { 'search.focus': 'f', 'results.next': 'n' },
			if_revision: 8
		});
		expect(keyOf('results.next')).toContain('N');
		expect(keyOf('search.focus')).toContain('F');
		expect((screen.getByRole('switch') as HTMLInputElement).checked).toBe(false);
	});

	it('stale twice gives up, says so and shows the newer settings, never overwriting them', async () => {
		render(KeyboardScreen);
		await screen.findByRole('switch');
		get.mockResolvedValue(section(true, { 'search.focus': 'f' }, 8));
		patch.mockRejectedValue(new ApiError(409, 'stale_revision', 'stale'));
		await changeKey('Next result', 'n');
		expect(await screen.findByText(/Couldn't save that change/)).toBeTruthy();
		expect(patch).toHaveBeenCalledTimes(2);
		await wait(() => expect(keyOf('results.next')).toContain('J'));
		expect(keyOf('search.focus')).toContain('F');
		expect(screen.queryByText('Saved.')).toBeNull();
	});

	it("Undo after a stale merge reverts only this change, keeping the other device's", async () => {
		render(KeyboardScreen);
		await screen.findByRole('switch');
		get.mockResolvedValue(section(true, { 'search.focus': 'f' }, 8));
		patch
			.mockRejectedValueOnce(new ApiError(409, 'stale_revision', 'stale'))
			.mockResolvedValueOnce(section(true, { 'search.focus': 'f', 'results.next': 'n' }, 9))
			.mockResolvedValueOnce(section(true, { 'search.focus': 'f' }, 10));
		await changeKey('Next result', 'n');
		await screen.findByText('Saved.');
		await fireEvent.click(screen.getByRole('button', { name: 'Undo' }));
		await wait(() => expect(patch).toHaveBeenCalledTimes(3));
		expect(body(2)).toEqual({ shortcut_keys: { 'search.focus': 'f' }, if_revision: 9 });
		// Focus stays on the row the change was made with, not on the vanished Undo button.
		await wait(() =>
			expect(document.activeElement).toBe(
				screen.getByRole('button', { name: 'Change the key for Next result' })
			)
		);
	});

	it('Undo never brings back an earlier change that failed', async () => {
		let fail!: () => void;
		patch.mockImplementationOnce(
			() => new Promise((_, reject) => (fail = () => reject(new ApiError(500, 'internal', 'x'))))
		);
		patch.mockResolvedValueOnce(section(true, { 'results.previous': 'p' }, 8));
		render(KeyboardScreen);
		await screen.findByRole('switch');
		await changeKey('Next result', 'n');
		await changeKey('Previous result', 'p');
		await wait(() => expect(patch).toHaveBeenCalledTimes(1));
		fail();
		await wait(() => expect(patch).toHaveBeenCalledTimes(2));
		expect(body(1)).toEqual({ shortcut_keys: { 'results.previous': 'p' }, if_revision: 7 });
		await screen.findByText('Saved.');
		patch.mockResolvedValueOnce(section(true, {}, 9));
		await fireEvent.click(screen.getByRole('button', { name: 'Undo' }));
		await wait(() => expect(patch).toHaveBeenCalledTimes(3));
		expect(body(2)).toEqual({ shortcut_keys: {}, if_revision: 8 });
	});

	it('Undo of the switch focuses the switch and sends what the server had', async () => {
		patch.mockResolvedValueOnce(section(false, {}, 8)).mockResolvedValueOnce(section(true, {}, 9));
		render(KeyboardScreen);
		const toggle = await screen.findByRole('switch');
		await fireEvent.click(toggle);
		await fireEvent.click(await screen.findByRole('button', { name: 'Undo' }));
		await wait(() => expect(patch).toHaveBeenCalledTimes(2));
		expect(body(1)).toEqual({ shortcuts: true, if_revision: 8 });
		await wait(() => expect(document.activeElement).toBe(toggle));
	});

	it("a failed save shows the server's current values, not the screen's older copy", async () => {
		render(KeyboardScreen);
		await screen.findByRole('switch');
		// Meanwhile another device remapped Focus search.
		get.mockResolvedValue(section(true, { 'search.focus': 'f' }, 8));
		patch.mockRejectedValueOnce(new ApiError(500, 'internal', 'boom'));
		await changeKey('Next result', 'n');
		expect(await screen.findByText(/Couldn't save that change/)).toBeTruthy();
		await wait(() => expect(keyOf('search.focus')).toContain('F'));
		expect(keyOf('results.next')).toContain('J');
		// What the registry was given, through the per-browser cache it writes alongside.
		expect(JSON.parse(localStorage.getItem('lodestar.shortcuts') ?? '{}')).toEqual({
			shortcuts: true,
			shortcut_keys: { 'search.focus': 'f' }
		});
	});

	it('says so when a save is refused because the setting is locked, and goes back', async () => {
		render(KeyboardScreen);
		patch.mockRejectedValueOnce(new ApiError(409, 'read_only', 'locked'));
		await fireEvent.click(await screen.findByRole('switch'));
		expect(
			await screen.findByText("These settings are locked, so your change wasn't saved.")
		).toBeTruthy();
		await wait(() => expect(keys.singleKeys).toBe(true));
		expect((screen.getByRole('switch') as HTMLInputElement).checked).toBe(true);
	});

	it('a failed save puts the saved value back and offers no Undo', async () => {
		render(KeyboardScreen);
		patch.mockRejectedValueOnce(new ApiError(500, 'internal', 'boom'));
		await fireEvent.click(await screen.findByRole('switch'));
		expect(await screen.findByText(/Couldn't save that change/)).toBeTruthy();
		expect((screen.getByRole('switch') as HTMLInputElement).checked).toBe(true);
		expect(keys.singleKeys).toBe(true);
		expect(screen.queryByRole('button', { name: 'Undo' })).toBeNull();
	});

	it('maps a 422 to the row it names', async () => {
		render(KeyboardScreen);
		await screen.findByRole('switch');
		const message = 'results.previous and results.next both use k; give each its own key.';
		patch.mockRejectedValueOnce(
			new ApiError(422, 'invalid_setting', message, { field: 'shortcut_keys' })
		);
		await changeKey('Next result', 'n');
		const row = await screen.findByTestId('shortcut-row-results.previous');
		await wait(() => expect(row.textContent).toContain(message));
		// Nothing was saved, so the row shows the saved key.
		expect(keyOf('results.next')).toContain('J');
	});

	it('finds the row when the message leads with an action the editor does not list', async () => {
		render(KeyboardScreen);
		await screen.findByRole('switch');
		const message = 'custom.thing and results.next both use p; give each its own key.';
		patch.mockRejectedValueOnce(
			new ApiError(422, 'invalid_setting', message, { field: 'shortcut_keys' })
		);
		await changeKey('Next result', 'p');
		const row = await screen.findByTestId('shortcut-row-results.next');
		await wait(() => expect(row.textContent).toContain(message));
	});

	it('shows a field the environment sets as locked, with its source', async () => {
		get.mockResolvedValue(section(false, {}, 7, { env: 'LODESTAR_SHORTCUTS', read_only: true }));
		render(KeyboardScreen);
		const toggle = await screen.findByRole('switch');
		expect(toggle.hasAttribute('disabled')).toBe(true);
		expect(screen.getByText('Set by LODESTAR_SHORTCUTS')).toBeTruthy();
	});

	it('says so when the values cannot be loaded', async () => {
		get.mockRejectedValue(new ApiError(500, 'internal', 'boom'));
		render(KeyboardScreen);
		expect(await screen.findByText("Couldn't load the keyboard settings.")).toBeTruthy();
	});
});
