// Remapping with no pointer: capture a key, it saves at once, and the registry follows
// (architecture §10.16, §10.10.1).
import type { Page, Request } from '@playwright/test';
import { liveEvents, openPaletteWithKey } from './support/events-server';
import { expect, test } from './support/test';

type State = { shortcuts: boolean; keys: Record<string, string>; revision: number };
const body = (s: State) => ({
	section: 'preferences',
	revision: s.revision,
	restart_required: [],
	fields: {
		shortcuts: {
			value: s.shortcuts,
			env: null,
			read_only: false,
			restart_required: false,
			source: 'db' as const
		},
		shortcut_keys: {
			value: s.keys,
			env: null,
			read_only: false,
			restart_required: false,
			source: 'db' as const
		}
	}
});

const fired = (page: Page) => page.getByTestId('fired');
const count = async (page: Page) => Number(await fired(page).textContent());

test('remap j to n by keyboard; it saves at once, n fires the command and j no longer does', async ({
	page,
	mock,
	go,
	kbd
}) => {
	const state: State = { shortcuts: true, keys: {}, revision: 1 };
	const patches: unknown[] = [];
	mock.on('getSettings', () => body(state));
	mock.on('patchSettings', (request: Request) => {
		const sent = request.postDataJSON() as Record<string, unknown>;
		patches.push(sent);
		if (typeof sent.shortcuts === 'boolean') state.shortcuts = sent.shortcuts;
		if (sent.shortcut_keys) state.keys = sent.shortcut_keys as Record<string, string>;
		state.revision++;
		return body(state);
	});
	await liveEvents(page);
	await go('/dev/shortcuts');

	const change = page.getByRole('button', { name: 'Change the key for Next result' });
	await kbd.tabTo(change);
	await kbd.press('Enter');
	await expect(change).toHaveAttribute('aria-pressed', 'true');
	await expect(page.getByRole('status').filter({ hasText: 'Press a key' })).toBeAttached();
	await kbd.press('n');
	await expect(page.getByTestId('shortcut-key-results.next')).toContainText('N');
	await expect(page.getByText('Saved.')).toBeVisible();
	expect(patches).toEqual([{ shortcut_keys: { 'results.next': 'n' }, if_revision: 1 }]);
	await expect(page.getByRole('button', { name: 'Save' })).toHaveCount(0);

	// Choosing c (Copy magnet's key) shows the conflict, and Cancel leaves n in place, unsaved.
	await kbd.tabTo(page.getByRole('button', { name: 'Change the key for Next result' }));
	await kbd.press('Enter');
	await kbd.press('c');
	await expect(page.getByText('Already used by Copy magnet')).toBeVisible();
	await kbd.tabTo(page.getByRole('button', { name: 'Cancel' }));
	await kbd.press('Enter');
	await expect(page.getByText('Already used by Copy magnet')).toBeHidden();
	await expect(page.getByTestId('shortcut-key-results.next')).toContainText('N');
	expect(patches).toHaveLength(1);

	// The shell attaches its listener on the first interaction: press until n lands.
	await expect.poll(async () => (await kbd.press('n'), count(page)), { timeout: 10_000 }).toBe(1);
	// n already landed above, so the remap is applied; a key handler runs synchronously on keydown,
	// so j has either fired by the time press() returns or it never will.
	await kbd.press('j');
	await expect(fired(page)).toHaveText('1');

	// Single-key shortcuts off: n stops, Ctrl+K still opens the palette.
	const toggle = page.getByRole('switch', { name: 'Single-key shortcuts' });
	await kbd.tabTo(toggle);
	await kbd.press('Space');
	await expect.poll(() => patches.length).toBe(2);
	expect(patches[1]).toEqual({ shortcuts: false, if_revision: 2 });
	await page.locator('body').press('Escape');
	// The preference is applied once the palette offers to turn the shortcuts back on.
	await openPaletteWithKey(page, () => kbd.press('Control+k'));
	await expect(page.getByText('Turn on single-key shortcuts')).toBeVisible();
	await kbd.press('Escape');
	await kbd.press('n');
	await expect(fired(page)).toHaveText('1');
	await openPaletteWithKey(page, () => kbd.press('Control+k'));
});
