import { beforeEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen } from '@testing-library/svelte';
import { ApiError } from '$lib/api/client';
import { keys } from '$lib/keys';
import { toasts } from '$lib/ui/feedback/toasts.svelte';

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
const save = () => screen.getByRole('button', { name: 'Save' });

beforeEach(() => {
	get.mockReset().mockResolvedValue(section(true, {}));
	patch.mockReset();
	toasts.items = [];
	keys.setSingleKeys(true);
});

describe('KeyboardScreen', () => {
	it('shows the heading, the purpose, the switch and the editor', async () => {
		render(KeyboardScreen);
		expect(await screen.findByRole('heading', { level: 1, name: 'Keyboard' })).toBeTruthy();
		const toggle = await screen.findByRole('switch', { name: 'Single-key shortcuts' });
		expect((toggle as HTMLInputElement).checked).toBe(true);
		expect(toggle.getAttribute('aria-describedby')).toBe('shortcuts-single-help');
		expect(screen.getByTestId('shortcuts-field')).toBeTruthy();
		expect(save().hasAttribute('disabled')).toBe(true);
	});

	it('saves only what changed, with if_revision, and applies it at once', async () => {
		patch.mockResolvedValue(section(false, {}, 8));
		render(KeyboardScreen);
		await fireEvent.click(await screen.findByRole('switch'));
		await fireEvent.click(save());
		await vi.waitFor(() => expect(patch).toHaveBeenCalled());
		expect(patch).toHaveBeenCalledWith('/settings/{section}', {
			path: { section: 'preferences' },
			body: { shortcuts: false, if_revision: 7 }
		});
		await vi.waitFor(() => expect(keys.singleKeys).toBe(false));
		expect(toasts.items.map((t) => t.text)).toContain('Keyboard settings saved');
		expect(save().hasAttribute('disabled')).toBe(true);
	});

	it('sends a remap, and Cancel throws the edits away', async () => {
		patch.mockResolvedValue(section(true, { 'results.next': 'n' }, 8));
		render(KeyboardScreen);
		await screen.findByRole('switch');
		await fireEvent.click(screen.getByRole('button', { name: 'Change the key for Next result' }));
		await press('n');
		expect(save().hasAttribute('disabled')).toBe(false);
		await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
		expect(screen.getByTestId('shortcut-key-results.next').textContent).toContain('J');
		await fireEvent.click(screen.getByRole('button', { name: 'Change the key for Next result' }));
		await press('n');
		await fireEvent.click(save());
		await vi.waitFor(() => expect(patch).toHaveBeenCalled());
		expect(patch.mock.calls[0]![1].body).toEqual({
			shortcut_keys: { 'results.next': 'n' },
			if_revision: 7
		});
		await vi.waitFor(() =>
			expect(keys.list().find((c) => c.id === 'results.next')).toBeUndefined()
		);
	});

	it('on a stale revision reloads, keeps the edits and says so', async () => {
		render(KeyboardScreen);
		await screen.findByRole('switch');
		await fireEvent.click(screen.getByRole('button', { name: 'Change the key for Next result' }));
		await press('n');
		patch.mockRejectedValueOnce(new ApiError(409, 'stale_revision', 'stale'));
		get.mockResolvedValue(section(true, { 'search.focus': 'f' }, 8));
		await fireEvent.click(save());
		expect(
			await screen.findByText('Changed in another tab or device; your edits were kept.')
		).toBeTruthy();
		// Their change and mine are both there.
		expect(screen.getByTestId('shortcut-key-results.next').textContent).toContain('N');
		expect(screen.getByTestId('shortcut-key-search.focus').textContent).toContain('F');
		patch.mockResolvedValue(section(true, { 'search.focus': 'f', 'results.next': 'n' }, 9));
		await fireEvent.click(save());
		await vi.waitFor(() => expect(patch).toHaveBeenCalledTimes(2));
		expect(patch.mock.calls[1]![1].body).toEqual({
			shortcut_keys: { 'search.focus': 'f', 'results.next': 'n' },
			if_revision: 8
		});
	});

	it('maps a 422 to the row it names', async () => {
		render(KeyboardScreen);
		await screen.findByRole('switch');
		await fireEvent.click(screen.getByRole('button', { name: 'Change the key for Next result' }));
		await press('n');
		const message = 'results.previous and results.next both use k; give each its own key.';
		patch.mockRejectedValueOnce(
			new ApiError(422, 'invalid_setting', message, { field: 'shortcut_keys' })
		);
		await fireEvent.click(save());
		const row = await screen.findByTestId('shortcut-row-results.previous');
		await vi.waitFor(() => expect(row.textContent).toContain(message));
		// The edit is still there to fix.
		expect(screen.getByTestId('shortcut-key-results.next').textContent).toContain('N');
	});

	it('finds the row when the message leads with an action the editor does not list', async () => {
		render(KeyboardScreen);
		await screen.findByRole('switch');
		await fireEvent.click(screen.getByRole('button', { name: 'Change the key for Next result' }));
		await press('p');
		const message = 'custom.thing and results.next both use p; give each its own key.';
		patch.mockRejectedValueOnce(
			new ApiError(422, 'invalid_setting', message, { field: 'shortcut_keys' })
		);
		await fireEvent.click(save());
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
