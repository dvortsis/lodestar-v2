// A dialog that has closed but is still waiting out its unmount delay must not outlive the screen:
// dismissing removes it at once and leaves no timer to fire after the DOM is gone.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor, within } from '@testing-library/svelte';
import { initLocale } from '$lib/i18n/locale.svelte';
import { confirmWithPassword, dismissPasswordDialogs } from './confirm';
import { dismissReveals, revealSecret } from './reveal';

vi.setConfig({ testTimeout: 30_000 });

beforeEach(() => {
	initLocale('en');
});
afterEach(() => {
	vi.restoreAllMocks();
	vi.useRealTimers();
	dismissPasswordDialogs();
	dismissReveals();
	document.body.innerHTML = '';
});

/** Spies on the 400 ms unmount timer: whether one was set, and whether it was cleared. */
function watch400() {
	const set: unknown[] = [];
	const cleared: unknown[] = [];
	const realSet = globalThis.setTimeout;
	const realClear = globalThis.clearTimeout;
	vi.spyOn(globalThis, 'setTimeout').mockImplementation(((f: () => void, ms?: number) => {
		const id = realSet(f, ms);
		if (ms === 400) set.push(id);
		return id;
	}) as typeof setTimeout);
	vi.spyOn(globalThis, 'clearTimeout').mockImplementation(((id: never) => {
		cleared.push(id);
		return realClear(id);
	}) as typeof clearTimeout);
	return { cleared: () => set.length > 0 && set.every((id) => cleared.includes(id)) };
}

describe('closed dialogs waiting to unmount', () => {
	it('a closed password dialog is removed by dismissPasswordDialogs, with no timer left', async () => {
		const answer = confirmWithPassword({
			title: 'Confirm',
			body: [],
			action: 'Do it',
			send: async () => 'ok'
		});
		const dialog = await screen.findByRole('dialog', {}, { timeout: 10_000 });
		vi.useFakeTimers();
		const unmountTimer = watch400();
		await fireEvent.click(within(dialog).getByTestId('password-dialog-cancel'));
		expect(await answer).toBeNull();

		dismissPasswordDialogs();
		expect(unmountTimer.cleared()).toBe(true);
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
	});

	it('a closed reveal is removed by dismissReveals, with no timer left', async () => {
		const shown = revealSecret('key', 'secret-value');
		const dialog = await screen.findByRole('dialog', {}, { timeout: 10_000 });
		vi.useFakeTimers();
		const unmountTimer = watch400();
		await fireEvent.click(within(dialog).getAllByRole('button').at(-1)!);
		await shown;

		dismissReveals();
		expect(unmountTimer.cleared()).toBe(true);
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
	});

	it('the normal path still unmounts after the delay', async () => {
		const answer = confirmWithPassword({ title: 'T', body: [], action: 'Go', send: async () => 1 });
		const dialog = await screen.findByRole('dialog', {}, { timeout: 10_000 });
		vi.useFakeTimers();
		const unmountTimer = watch400();
		await fireEvent.click(within(dialog).getByTestId('password-dialog-cancel'));
		await answer;
		expect(unmountTimer.cleared()).toBe(false);
		vi.advanceTimersByTime(400);
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
	});
});
