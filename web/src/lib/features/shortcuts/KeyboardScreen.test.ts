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

configure({ defaultHidden: true });

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
		await vi.waitFor(() => expect(patch).toHaveBeenCalledTimes(1));
		expect(patch).toHaveBeenCalledWith('/settings/{section}', {
			path: { section: 'preferences' },
			body: { shortcuts: false, if_revision: 7 }
		});
		const status = await screen.findByText('Saved.');
		expect(status.getAttribute('role')).toBe('status');
		patch.mockResolvedValueOnce(section(true, {}, 9));
		await fireEvent.click(screen.getByRole('button', { name: 'Undo' }));
		await vi.waitFor(() => expect(patch).toHaveBeenCalledTimes(2));
		expect(body(1)).toEqual({ shortcuts: true, if_revision: 8 });
		await vi.waitFor(() => expect(keys.singleKeys).toBe(true));
		// Undoing is not itself undoable.
		await vi.waitFor(() => expect(screen.queryByRole('button', { name: 'Undo' })).toBeNull());
	});

	it('a remap saves at once and the registry follows; Undo puts the old key back', async () => {
		patch.mockResolvedValueOnce(section(true, { 'results.next': 'n' }, 8));
		render(KeyboardScreen);
		await screen.findByRole('switch');
		await changeKey('Next result', 'n');
		await vi.waitFor(() => expect(patch).toHaveBeenCalledTimes(1));
		expect(body(0)).toEqual({ shortcut_keys: { 'results.next': 'n' }, if_revision: 7 });
		await screen.findByText('Saved.');
		expect(keyOf('results.next')).toContain('N');
		patch.mockResolvedValueOnce(section(true, {}, 9));
		await fireEvent.click(screen.getByRole('button', { name: 'Undo' }));
		await vi.waitFor(() => expect(patch).toHaveBeenCalledTimes(2));
		expect(body(1)).toEqual({ shortcut_keys: {}, if_revision: 8 });
		await vi.waitFor(() => expect(keyOf('results.next')).toContain('J'));
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
		await Promise.resolve();
		expect(patch).not.toHaveBeenCalled();

		patch.mockResolvedValueOnce(
			section(true, { 'results.next': 'c', 'detail.copy_magnet': 'j' }, 8)
		);
		await changeKey('Next result', 'c');
		expect(patch).not.toHaveBeenCalled();
		await fireEvent.click(screen.getByRole('button', { name: 'Swap' }));
		await vi.waitFor(() => expect(patch).toHaveBeenCalledTimes(1));
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
		await vi.waitFor(() => expect(patch).toHaveBeenCalledTimes(1));
		release();
		await vi.waitFor(() => expect(patch).toHaveBeenCalledTimes(2));
		expect(body(1)).toEqual({ shortcut_keys: { 'results.next': 'n' }, if_revision: 8 });
		await screen.findByText('Saved.');
		expect(keyOf('results.next')).toContain('N');
		expect((screen.getByRole('switch') as HTMLInputElement).checked).toBe(false);
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
		await vi.waitFor(() => expect(keyOf('results.next')).toContain('J'));
		expect(keyOf('search.focus')).toContain('F');
		expect(screen.queryByText('Saved.')).toBeNull();
	});

	it('says so when a save is refused because the setting is locked, and goes back', async () => {
		render(KeyboardScreen);
		patch.mockRejectedValueOnce(new ApiError(409, 'read_only', 'locked'));
		await fireEvent.click(await screen.findByRole('switch'));
		expect(
			await screen.findByText("These settings are locked, so your change wasn't saved.")
		).toBeTruthy();
		await vi.waitFor(() => expect(keys.singleKeys).toBe(true));
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
		await vi.waitFor(() => expect(row.textContent).toContain(message));
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
		await vi.waitFor(() => expect(row.textContent).toContain(message));
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
