import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { keys } from '$lib/keys';
import CommandPalette from './CommandPalette.svelte';

configure({ defaultHidden: true });

const offs: (() => void)[] = [];
const add = (id: string, title: string, run = () => {}, extra: Record<string, unknown> = {}) =>
	offs.push(keys.register({ id, title, run, ...extra }));

beforeEach(() => {
	// jsdom has no layout, so scrollIntoView does not exist.
	Element.prototype.scrollIntoView = vi.fn();
});
afterEach(() => offs.splice(0).forEach((off) => off()));

const options = () => screen.getAllByRole('option').map((o) => o.textContent?.trim());

describe('CommandPalette', () => {
	it('lists registered commands, labelled, with the input as a combobox', async () => {
		add('t.go', 'Go to Search', () => {}, { keys: ['g'] });
		add('t.copy', 'Copy magnet');
		render(CommandPalette, { open: true });
		const input = await screen.findByRole('combobox', { name: 'Type a command' });
		expect(screen.getByRole('dialog', { name: 'Command palette' })).toBeTruthy();
		expect(screen.getByRole('listbox', { name: 'Commands' })).toBeTruthy();
		expect(options()).toEqual(['Go to Search G', 'Copy magnet']);
		expect(input.getAttribute('aria-activedescendant')).toBe('palette-option-0');
	});

	it('hides commands that are shortcut-only, out of scope or switched off', async () => {
		add('t.a', 'Visible');
		add('t.b', 'Shortcut only', () => {}, { palette: false });
		add('t.c', 'Other scope', () => {}, { scope: 'detail' });
		add('t.d', 'Switched off', () => {}, { when: () => false });
		render(CommandPalette, { open: true });
		await screen.findByRole('listbox');
		expect(options()).toEqual(['Visible']);
	});

	it('filters as you type and says so when nothing matches', async () => {
		add('t.a', 'Go to Alerts');
		add('t.b', 'Copy magnet');
		render(CommandPalette, { open: true });
		const input = await screen.findByRole('combobox');
		await fireEvent.input(input, { target: { value: 'mag' } });
		expect(options()).toEqual(['Copy magnet']);
		await fireEvent.input(input, { target: { value: 'zzz' } });
		expect(screen.queryByRole('listbox')).toBeNull();
		expect(screen.getByText(/No command matches “zzz”/)).toBeTruthy();
		expect(input.getAttribute('aria-activedescendant')).toBeNull();
	});

	it('moves with the arrows (wrapping) and runs the selection on Enter, then closes', async () => {
		const first = vi.fn();
		const second = vi.fn();
		add('t.a', 'First', first);
		add('t.b', 'Second', second);
		render(CommandPalette, { open: true });
		const input = await screen.findByRole('combobox');
		await fireEvent.keyDown(input, { key: 'ArrowDown' });
		expect(input.getAttribute('aria-activedescendant')).toBe('palette-option-1');
		await fireEvent.keyDown(input, { key: 'ArrowDown' });
		expect(input.getAttribute('aria-activedescendant')).toBe('palette-option-0');
		await fireEvent.keyDown(input, { key: 'ArrowUp' });
		await fireEvent.keyDown(input, { key: 'Enter' });
		await waitFor(() => expect(second).toHaveBeenCalledOnce());
		expect(first).not.toHaveBeenCalled();
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
	});

	it('runs a clicked command', async () => {
		const run = vi.fn();
		add('t.a', 'Click me', run);
		render(CommandPalette, { open: true });
		await fireEvent.click(await screen.findByRole('option', { name: 'Click me' }));
		await waitFor(() => expect(run).toHaveBeenCalledOnce());
	});

	it('closes on Escape without running anything', async () => {
		const run = vi.fn();
		add('t.a', 'Never', run);
		render(CommandPalette, { open: true });
		const dialog = await screen.findByRole('dialog');
		await fireEvent.keyDown(dialog, { key: 'Escape' });
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(run).not.toHaveBeenCalled();
	});
});
