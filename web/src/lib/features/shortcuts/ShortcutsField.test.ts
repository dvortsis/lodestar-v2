import { describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen } from '@testing-library/svelte';
import { keys } from '$lib/keys';
import ShortcutsField from './ShortcutsField.svelte';

configure({ defaultHidden: true });

const setup = (value: Record<string, string> = {}, extra: Record<string, unknown> = {}) => {
	const onchange = vi.fn();
	render(ShortcutsField, { value, onchange, ...extra });
	return onchange;
};
const change = (action: string) =>
	screen.getByRole('button', { name: `Change the key for ${action}` });
const press = (key: string, init: KeyboardEventInit = {}) =>
	fireEvent.keyDown(document.body, { key, ...init });

describe('ShortcutsField', () => {
	it('lists the eight actions in §10.16 order with their effective keys', () => {
		setup({ 'results.next': 'n' });
		const rows = screen.getAllByRole('listitem').map((li) => li.textContent?.replace(/\s+/g, ' '));
		expect(rows).toHaveLength(8);
		expect(rows[0]).toContain('Focus search');
		expect(rows[1]).toContain('Next result');
		expect(screen.getByTestId('shortcut-key-results.next').textContent).toContain('N');
		expect(screen.getByTestId('shortcut-key-results.previous').textContent).toContain('K');
		// Symbols have a spoken name.
		expect(screen.getByTestId('shortcut-key-search.focus').textContent).toContain('Slash');
		expect(screen.getByTestId('shortcut-key-help.shortcuts').textContent).toContain(
			'Question mark'
		);
	});

	it('Change takes the next key: announced, pressed, then stored', async () => {
		const onchange = setup();
		const button = change('Next result');
		await fireEvent.click(button);
		expect(button.getAttribute('aria-pressed')).toBe('true');
		expect(screen.getByRole('status').textContent).toBe('Press a key');
		await press('n');
		expect(onchange).toHaveBeenCalledWith({ 'results.next': 'n' });
		expect(button.getAttribute('aria-pressed')).toBe('false');
	});

	it('accepts a symbol such as /', async () => {
		const onchange = setup({ 'search.focus': 'f' });
		await fireEvent.click(change('Next result'));
		await press('/');
		expect(onchange).toHaveBeenCalledWith({ 'search.focus': 'f', 'results.next': '/' });
	});

	it.each([
		['J', { shiftKey: true }, 'Use a key without Shift'],
		['k', { ctrlKey: true }, 'Use a key without Ctrl, Alt or Cmd'],
		[' ', {}, 'Use a key that shows a character'],
		['​', {}, 'Use a key that shows a character'],
		['́', {}, 'Use a key that shows a character']
	])('refuses %j and says why', async (key, init, message) => {
		const onchange = setup();
		await fireEvent.click(change('Next result'));
		await press(key, init);
		expect(onchange).not.toHaveBeenCalled();
		expect(screen.getByText(message)).toBeTruthy();
		// Still capturing: the next good key is taken.
		await press('n');
		expect(onchange).toHaveBeenCalledWith({ 'results.next': 'n' });
		expect(screen.queryByText(message)).toBeNull();
	});

	it('Escape and Tab cancel; Tab still moves focus', async () => {
		const onchange = setup();
		const button = change('Next result');
		await fireEvent.click(button);
		await press('Escape');
		expect(button.getAttribute('aria-pressed')).toBe('false');
		await fireEvent.click(button);
		const proceeded = await press('Tab');
		expect(button.getAttribute('aria-pressed')).toBe('false');
		expect(proceeded).toBe(true); // not default-prevented
		expect(onchange).not.toHaveBeenCalled();
	});

	it('does not let the key reach the registry', async () => {
		const run = vi.fn();
		const off = keys.register({ id: 'test.n', title: 'N', keys: ['n'], run });
		const detach = keys.attach(window);
		setup();
		await fireEvent.click(change('Next result'));
		await press('n');
		expect(run).not.toHaveBeenCalled();
		detach();
		off();
	});

	it('names the other action on a clash and offers Swap or Cancel', async () => {
		const onchange = setup();
		await fireEvent.click(change('Next result'));
		await press('k');
		expect(onchange).not.toHaveBeenCalled();
		expect(screen.getByText('Already used by Previous result')).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: 'Swap' }));
		expect(onchange).toHaveBeenCalledWith({ 'results.next': 'k', 'results.previous': 'j' });
	});

	it('Cancel leaves everything as it was', async () => {
		const onchange = setup();
		await fireEvent.click(change('Next result'));
		await press('k');
		await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
		expect(onchange).not.toHaveBeenCalled();
		expect(screen.queryByText('Already used by Previous result')).toBeNull();
	});

	it('warns, without blocking, when a registered command uses the key', async () => {
		const off = keys.register({ id: 'test.g', title: 'Go home', keys: ['g'], run: () => {} });
		const onchange = setup();
		await fireEvent.click(change('Next result'));
		await press('g');
		expect(onchange).toHaveBeenCalledWith({ 'results.next': 'g' });
		off();
	});

	it('shows the warning for a remap that already collides', () => {
		const off = keys.register({ id: 'test.g', title: 'Go home', keys: ['g'], run: () => {} });
		setup({ 'results.next': 'g' });
		expect(screen.getByText(/G is also used by Go home/)).toBeTruthy();
		off();
	});

	it('Reset restores a default and Reset all clears every remap', async () => {
		const onchange = setup({ 'results.next': 'n', 'detail.send': 'z' });
		await fireEvent.click(screen.getByRole('button', { name: 'Reset the key for Next result' }));
		expect(onchange).toHaveBeenLastCalledWith({ 'detail.send': 'z' });
		await fireEvent.click(screen.getByRole('button', { name: 'Reset all' }));
		expect(onchange).toHaveBeenLastCalledWith({});
	});

	it('puts focus back on the Change button after Reset', async () => {
		setup({ 'results.next': 'n' });
		await fireEvent.click(screen.getByRole('button', { name: 'Reset the key for Next result' }));
		await vi.waitFor(() => expect(document.activeElement).toBe(change('Next result')));
	});

	it('has no Reset buttons when nothing is remapped', () => {
		setup();
		expect(screen.queryByRole('button', { name: /^Reset/ })).toBeNull();
	});

	it('Reset onto a key another action holds asks first', async () => {
		const onchange = setup({ 'results.next': 'k', 'results.previous': 'j' });
		await fireEvent.click(screen.getByRole('button', { name: 'Reset the key for Next result' }));
		expect(onchange).not.toHaveBeenCalled();
		expect(screen.getByText('Already used by Previous result')).toBeTruthy();
	});

	it('shows the server messages under their row and under the field', () => {
		setup({}, { errors: { 'results.next': 'results.next: bad', '': 'whole field' } });
		const row = screen.getByTestId('shortcut-row-results.next');
		expect(row.textContent).toContain('results.next: bad');
		expect(screen.getByText('whole field')).toBeTruthy();
	});

	it('disables every control when locked', () => {
		setup({ 'results.next': 'n' }, { disabled: true });
		for (const b of screen.getAllByRole('button')) expect(b.hasAttribute('disabled')).toBe(true);
	});
});
