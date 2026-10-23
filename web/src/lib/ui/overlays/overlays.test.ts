import { beforeAll, describe, expect, it, vi } from 'vitest';
import { createRawSnippet } from 'svelte';
import { stubLayout } from '../lists/testing/layout';
import { configure, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import Dialog from './Dialog.svelte';
import DialogHarness from './testing/DialogHarness.svelte';
import OverlayHarness from './testing/OverlayHarness.svelte';
import DrawerHarness from './testing/DrawerHarness.svelte';

// Floating panels are `visibility: hidden` until Floating UI measures them, which jsdom cannot do.
configure({ defaultHidden: true });

// jsdom cannot position floating panels, so they stay `visibility: hidden` and the accessible-name
// computation skips their text; match menu items by text instead.
const item = (name: string) =>
	screen.getAllByRole('menuitem').find((e) => e.textContent?.trim() === name) as HTMLElement;

const key = (el: Element, k: string, init: KeyboardEventInit = {}) =>
	fireEvent.keyDown(el, { key: k, ...init });

describe('Dialog', () => {
	it('opens as a labelled modal and closes on Esc, restoring focus to the trigger', async () => {
		render(DialogHarness);
		const trigger = screen.getByRole('button', { name: 'Open dialog' });
		trigger.focus();
		await fireEvent.click(trigger);
		const dialog = await screen.findByRole('dialog', { name: 'Confirm' });
		expect(dialog.getAttribute('aria-describedby')).toBeTruthy();
		await key(dialog, 'Escape');
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		await waitFor(() => expect(document.activeElement).toBe(trigger));
	});

	it('moves focus inside on open and restores it to the invoker when opened from code', async () => {
		render(DialogHarness, { withTrigger: false });
		const invoker = screen.getByRole('button', { name: 'Open programmatically' });
		invoker.focus();
		await fireEvent.click(invoker);
		const dialog = await screen.findByRole('dialog', { name: 'Confirm' });
		await waitFor(() => expect(dialog.contains(document.activeElement)).toBe(true));
		await fireEvent.click(screen.getByRole('button', { name: 'Close' }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		await waitFor(() => expect(document.activeElement).toBe(invoker));
	});

	it('closes from the footer action and from the close button', async () => {
		render(DialogHarness);
		await fireEvent.click(screen.getByRole('button', { name: 'Open dialog' }));
		await screen.findByRole('dialog');
		await fireEvent.click(screen.getByRole('button', { name: 'Done' }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
	});
});

describe('Dialog initialFocus', () => {
	const body = createRawSnippet(() => ({
		render: () => '<div><input aria-label="First" /><button type="button">Keep</button></div>'
	}));
	const open = async (initialFocus?: () => HTMLElement | null | undefined) => {
		const invoker = document.createElement('button');
		invoker.textContent = 'Invoker';
		document.body.append(invoker);
		invoker.focus();
		const r = render(Dialog, {
			props: { open: true, title: 'Remove', closeLabel: 'Close', children: body, initialFocus }
		});
		const dialog = await screen.findByRole('dialog', { name: 'Remove' });
		return { r, dialog, invoker };
	};

	it('starts focus on the element it returns and restores it to the invoker on close', async () => {
		const { dialog, invoker } = await open(() => screen.getByRole('button', { name: 'Keep' }));
		await waitFor(() => expect(document.activeElement?.textContent).toBe('Keep'));
		await key(dialog, 'Escape');
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		await waitFor(() => expect(document.activeElement).toBe(invoker));
		invoker.remove();
	});

	it('keeps the default focus when it returns nothing, or is absent', async () => {
		for (const initialFocus of [() => null, undefined]) {
			const { r, dialog, invoker } = await open(initialFocus);
			// Bits' own choice (jsdom can't tell what's tabbable, so it focuses the panel itself).
			await waitFor(() => expect(dialog.contains(document.activeElement)).toBe(true));
			expect(document.activeElement?.textContent).not.toBe('Keep');
			r.unmount();
			invoker.remove();
		}
	});
});

describe('Sheet', () => {
	it('is a modal dialog with the side recorded for motion', async () => {
		render(DialogHarness, { sheet: true });
		await fireEvent.click(screen.getByRole('button', { name: 'Open sheet' }));
		const sheet = await screen.findByRole('dialog', { name: 'Filters' });
		expect(sheet.getAttribute('data-side')).toBe('bottom');
		await key(sheet, 'Escape');
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
	});
});

describe('Tooltip', () => {
	it('opens on keyboard focus, describes the trigger and closes on Esc', async () => {
		render(OverlayHarness);
		const trigger = screen.getByRole('button', { name: 'Copy' });
		trigger.focus();
		await fireEvent.focus(trigger);
		const tip = await screen.findByRole('tooltip');
		expect(tip.textContent).toContain('Copies the magnet link');
		await key(trigger, 'Escape');
		await waitFor(() => expect(screen.queryByRole('tooltip')).toBeNull());
	});
});

describe('Tooltip hover', () => {
	it('opens on pointer hover and closes on leave', async () => {
		render(OverlayHarness);
		const trigger = screen.getByRole('button', { name: 'Copy' });
		await fireEvent.pointerEnter(trigger, { pointerType: 'mouse' });
		await fireEvent.pointerMove(trigger, { pointerType: 'mouse' });
		expect((await screen.findByRole('tooltip')).textContent).toContain('Copies the magnet link');
		await fireEvent.pointerLeave(trigger, { pointerType: 'mouse' });
		await waitFor(() => expect(screen.queryByRole('tooltip')).toBeNull());
	});
});

describe('Popover', () => {
	it('opens from its trigger, is named, does not trap focus, and closes on Esc', async () => {
		render(OverlayHarness);
		const trigger = screen.getByRole('button', { name: 'Pop' });
		await fireEvent.click(trigger);
		const panel = await screen.findByLabelText('Columns', { selector: '[role="dialog"]' });
		expect(panel.isConnected).toBe(true);
		// Non-modal: the rest of the page stays reachable and focus can sit outside.
		const outside = screen.getByRole('button', { name: 'Outside' });
		outside.focus();
		expect(document.activeElement).toBe(outside);
		expect(screen.getByLabelText('Inside popover').isConnected).toBe(true);
		await key(panel, 'Escape');
		await waitFor(() => expect(screen.queryByLabelText('Inside popover')).toBeNull());
	});
});

describe('Menu', () => {
	async function openMenu() {
		const onSelect = vi.fn();
		render(OverlayHarness, { onSelect });
		const trigger = screen.getByRole('button', { name: 'Actions' });
		trigger.focus();
		await key(trigger, 'ArrowDown');
		const menu = await screen.findByRole('menu', { name: 'Actions' });
		return { menu, trigger, onSelect };
	}

	it('opens with ArrowDown and focuses the first item', async () => {
		const { menu } = await openMenu();
		await waitFor(() => expect(document.activeElement).toBe(item('Favorite')));
		expect(menu.isConnected).toBe(true);
	});

	it('moves with arrows, skips disabled items, selects with Enter and closes', async () => {
		const { onSelect } = await openMenu();
		const fav = item('Favorite');
		await waitFor(() => expect(document.activeElement).toBe(fav));
		await key(document.activeElement as Element, 'ArrowDown');
		await waitFor(() => expect(document.activeElement).toBe(item('Hide')));
		await key(document.activeElement as Element, 'ArrowDown');
		await waitFor(() => expect(document.activeElement?.textContent?.trim()).toBe('Labels'));
		await key(document.activeElement as Element, 'ArrowDown');
		await waitFor(() => expect(document.activeElement).toBe(item('Fix parse')));
		await key(item('Fix parse'), 'Enter');
		await waitFor(() => expect(onSelect).toHaveBeenCalledWith('fix'));
		await waitFor(() => expect(screen.queryByRole('menu')).toBeNull());
	});

	it('jumps by typeahead and closes on Esc returning focus to the trigger', async () => {
		const { trigger } = await openMenu();
		await waitFor(() => expect(document.activeElement).toBe(item('Favorite')));
		await key(document.activeElement as Element, 'l');
		await waitFor(() => expect(document.activeElement?.textContent?.trim()).toBe('Labels'));
		await key(document.activeElement as Element, 'Escape');
		await waitFor(() => expect(screen.queryByRole('menu')).toBeNull());
		await waitFor(() => expect(document.activeElement).toBe(trigger));
	});

	it('marks disabled items', async () => {
		await openMenu();
		expect(item('Disabled one').getAttribute('aria-disabled')).toBe('true');
	});
});

describe('Drawer Esc and nested overlays', () => {
	it('Esc closes an open tooltip first and the drawer only on the next press', async () => {
		render(DrawerHarness);
		const row = screen.getByTestId('row');
		row.focus();
		await fireEvent.click(row);
		await screen.findByRole('heading', { name: 'Ubuntu 24.04' });
		const copy = screen.getByRole('button', { name: 'Copy' });
		copy.focus();
		await fireEvent.focus(copy);
		await screen.findByRole('tooltip');
		await key(copy, 'Escape');
		await waitFor(() => expect(screen.queryByRole('tooltip')).toBeNull());
		expect(screen.queryByRole('complementary')).not.toBeNull();
		await key(copy, 'Escape');
		await waitFor(() => expect(screen.queryByRole('complementary')).toBeNull());
	});
});

describe('Drawer Esc with an expanded tree row', () => {
	beforeAll(() => stubLayout());
	it('closes the drawer: aria-expanded on a row is not an open popup', async () => {
		render(DrawerHarness);
		const row = screen.getByTestId('row');
		row.focus();
		await fireEvent.click(row);
		await screen.findByRole('heading', { name: 'Ubuntu 24.04' });
		const folder = screen.getByRole('treeitem', { name: /Season 1/ });
		expect(folder.getAttribute('aria-expanded')).toBe('true');
		folder.focus();
		await key(folder, 'Escape');
		await waitFor(() => expect(screen.queryByRole('complementary')).toBeNull());
	});
});

describe('Drawer', () => {
	async function openDrawer(props = {}) {
		render(DrawerHarness, props);
		const row = screen.getByTestId('row');
		row.focus();
		await fireEvent.click(row);
		const title = await screen.findByRole('heading', { name: 'Ubuntu 24.04' });
		return { row, title };
	}

	it('is a non-modal complementary region named by its title, and moves focus to the title', async () => {
		const { title } = await openDrawer();
		await waitFor(() => expect(document.activeElement).toBe(title));
		const region = screen.getByRole('complementary', { name: 'Ubuntu 24.04' });
		expect(region.hasAttribute('aria-modal')).toBe(false);
		// The list stays reachable: other buttons are not inert.
		expect(screen.getByTestId('row').hasAttribute('inert')).toBe(false);
	});

	it('Esc closes and returns focus to the invoking row', async () => {
		const { row, title } = await openDrawer();
		await key(title, 'Escape');
		await waitFor(() => expect(screen.queryByRole('complementary')).toBeNull());
		expect(document.activeElement).toBe(row);
	});

	it('the close button also returns focus to the row', async () => {
		const { row } = await openDrawer();
		await fireEvent.click(screen.getByRole('button', { name: 'Close' }));
		await waitFor(() => expect(screen.queryByRole('complementary')).toBeNull());
		expect(document.activeElement).toBe(row);
	});

	it('resizes from the keyboard, clamped, reporting each width through the callback', async () => {
		const onWidthChange = vi.fn();
		await openDrawer({ onWidthChange, width: 560 });
		const handle = screen.getByRole('separator', { name: 'Resize' });
		expect(handle.getAttribute('aria-valuenow')).toBe('560');
		expect(handle.getAttribute('aria-valuemin')).toBe('480');
		expect(handle.getAttribute('aria-valuemax')).toBe('640');
		await key(handle, 'ArrowLeft');
		expect(onWidthChange).toHaveBeenLastCalledWith(576);
		await key(handle, 'ArrowLeft', { shiftKey: true });
		expect(onWidthChange).toHaveBeenLastCalledWith(624);
		await key(handle, 'Home');
		expect(onWidthChange).toHaveBeenLastCalledWith(640);
		await key(handle, 'End');
		expect(onWidthChange).toHaveBeenLastCalledWith(480);
		await key(handle, 'ArrowRight', { shiftKey: true });
		expect(onWidthChange).toHaveBeenLastCalledWith(496);
	});

	it('clamps a stored width that is out of range', async () => {
		await openDrawer({ width: 9999 });
		expect(screen.getByRole('separator', { name: 'Resize' }).getAttribute('aria-valuenow')).toBe(
			'640'
		);
	});
});
