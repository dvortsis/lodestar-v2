import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, within } from '@testing-library/svelte';
import { keys } from '$lib/keys';
import { shown } from './keyname';

const patch = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (orig) => ({
	...(await orig<typeof import('$lib/api/client')>()),
	api: { get: vi.fn(), patch }
}));

import ShortcutSheet from './ShortcutSheet.svelte';
import { sheet } from './sheet.svelte';

configure({ defaultHidden: true });

const offs: (() => void)[] = [];
const add = (id: string, title: string, extra: Record<string, unknown> = {}) =>
	offs.push(keys.register({ id, title, run: () => {}, ...extra }));
let pop: (() => void)[] = [];

beforeEach(() => {
	keys.setSingleKeys(true);
	sheet.open = true;
	patch.mockReset();
});
afterEach(() => {
	offs.splice(0).forEach((off) => off());
	pop.forEach((f) => f());
	pop = [];
	sheet.open = false;
});

describe('ShortcutSheet', () => {
	it('is a labelled dialog that lists commands with keys, grouped by section', async () => {
		add('t.go', 'Go to Search', { keys: ['g'], section: 'Navigate' });
		add('t.palette', 'Open palette', { keys: ['mod+k'], section: 'General', allowInInput: true });
		add('t.nokey', 'No key', { keys: [], section: 'General' });
		render(ShortcutSheet);
		const dialog = await screen.findByRole('dialog', { name: 'Keyboard shortcuts' });
		const headings = within(dialog)
			.getAllByRole('heading', { level: 3 })
			.map((h) => h.textContent);
		expect(headings).toEqual(['Navigate', 'General']);
		expect(dialog.textContent).toContain('Go to Search');
		expect(dialog.textContent).toContain('Open palette');
		expect(dialog.textContent).not.toContain('No key');
		expect(within(dialog).getAllByText('Ctrl+K').length).toBeGreaterThan(0);
	});

	it('puts commands from other screens under "On other screens"', async () => {
		add('t.here', 'Here', { keys: ['h'], section: 'Page' });
		add('t.there', 'There', { keys: ['t'], section: 'Detail', scope: 'detail' });
		render(ShortcutSheet);
		const dialog = await screen.findByRole('dialog');
		const headings = within(dialog)
			.getAllByRole('heading', { level: 3 })
			.map((h) => h.textContent);
		expect(headings).toEqual(['Page', 'On other screens']);
		const popScope = keys.pushScope('detail');
		pop.push(popScope);
		await vi.waitFor(() =>
			expect(
				within(dialog)
					.getAllByRole('heading', { level: 3 })
					.map((h) => h.textContent)
			).toEqual(['Page', 'Detail'])
		);
	});

	it('shows combos as ⌘ on a Mac and Ctrl elsewhere', () => {
		expect(shown('mod+k', true)).toBe('⌘K');
		expect(shown('mod+k', false)).toBe('Ctrl+K');
		expect(shown('/')).toBe('/');
	});

	it('names both commands when two share a key', async () => {
		add('t.a', 'Alpha', { keys: ['x'], section: 'S' });
		add('t.b', 'Beta', { keys: ['x'], section: 'S' });
		vi.spyOn(console, 'error').mockImplementation(() => {});
		render(ShortcutSheet);
		const dialog = await screen.findByRole('dialog');
		expect(within(dialog).getByText(/Alpha and Beta both use X/)).toBeTruthy();
	});

	it('says when single-key shortcuts are off, marks them, and Turn on saves', async () => {
		add('t.a', 'Alpha', { keys: ['a'], section: 'S' });
		add('t.k', 'Palette', { keys: ['mod+k'], section: 'S' });
		keys.setSingleKeys(false);
		patch.mockResolvedValue({
			fields: {
				shortcuts: { value: true },
				shortcut_keys: { value: {} }
			},
			revision: 2
		});
		render(ShortcutSheet);
		const dialog = await screen.findByRole('dialog');
		expect(within(dialog).getByText(/Single-key shortcuts are off/)).toBeTruthy();
		const rows = within(dialog).getAllByRole('listitem');
		expect(rows[0]!.textContent).toContain('Off');
		expect(rows[1]!.textContent).not.toContain('Off');
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Turn on' }));
		await vi.waitFor(() => expect(keys.singleKeys).toBe(true));
		expect(patch.mock.calls[0]![1].body).toEqual({ shortcuts: true });
		// The banner with the button is gone: focus stays in the dialog.
		await vi.waitFor(() => expect(document.activeElement).toBe(dialog));
	});

	it('links to the Keyboard settings screen', async () => {
		render(ShortcutSheet);
		const link = await screen.findByRole('link', { name: 'Change shortcuts' });
		expect(link.getAttribute('href')).toBe('/settings/keyboard');
	});

	it('closes with Escape', async () => {
		render(ShortcutSheet);
		const dialog = await screen.findByRole('dialog');
		await fireEvent.keyDown(dialog, { key: 'Escape' });
		await vi.waitFor(() => expect(sheet.open).toBe(false));
	});
});
